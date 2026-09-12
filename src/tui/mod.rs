//! Presentation layer (ARCH-1).
//!
//! The UI is a function of application state plus a stream of events. It never
//! performs IO and never calls an adapter directly: keys become actions, actions
//! mutate state and return an [`Effect`], and this loop is the only place that turns
//! an effect into IO.
//!
//! The loop owns three things the reducer cannot: the terminal, the state file, and
//! the [`JobRunner`](crate::tui::jobs::JobRunner) that runs `gh` and `git` off the
//! event loop. Everything the reducer needs to decide with arrives as state, which is
//! what keeps a frame cheap and the reducer testable (NFR-1.2).

pub mod action;
pub mod app;
pub mod clipboard;
pub mod components;
pub mod diff_view;
pub mod event;
pub mod jobs;
pub mod keymap;
pub mod layout;
pub mod list_view;
pub mod terminal;
pub mod text;
pub mod theme;
pub mod update;

#[cfg(test)]
pub(crate) mod test_support;

use std::io::Write;
use std::sync::Arc;

use crate::Startup;
use crate::application::analysis::AnalysisIntent;
use crate::application::environment::DetectRequest;
use crate::application::prs::CachePolicy;
use crate::domain::repo::RepoId;
use crate::error::Result;
use crate::logging::{self, Level};
use crate::ports::{Clock, StateStore};
use crate::tui::jobs::{Executor, Job, JobRunner, Outcome};
pub use app::{App, Effect, Overlay, Pane};
pub use keymap::Mode;

/// Runs the interface until the user quits.
///
/// # Errors
///
/// Returns an error when the terminal cannot be taken over, drawn to, or restored.
pub fn run(startup: Startup) -> Result<()> {
    // The loop owns the clock, the state file and the background jobs so the reducer
    // can stay a pure function of state.
    let clock = startup.clock;
    let clock_source: Arc<dyn Clock> = Arc::new(clock);
    let cache = startup.cache.clone();
    let forge_factory = startup.forge_factory.clone();
    let state_store: Box<dyn StateStore> = Box::new(startup.state_store.clone());

    let catalog = startup.catalog.clone();
    let llm = startup.llm.clone();
    let workspace = startup.workspace.clone();
    let workspace_for_executor = startup.workspace.clone();
    let probe = startup.probe.clone();
    let request = DetectRequest {
        repo: startup.repo.clone(),
        // `--remote` beats `[forge].remote`, which beats automatic detection
        // (FR-8.1's precedence order).
        remote: startup
            .remote
            .clone()
            .or_else(|| startup.config.forge.remote.clone()),
        gh_program: Some(startup.config.forge.gh_path.clone()),
    };
    let mut app = App::new(startup)?;
    let mut terminal = terminal::TerminalGuard::enter(app.mouse_enabled())?;
    let mut runner = JobRunner::new(workspace, probe, catalog, llm, request);

    // Detection is a job, not a startup step: it runs `gh auth status`, which reaches
    // the network, and the first frame must not wait for it (NFR-1.1).
    //
    // Submitted through `apply` rather than directly, so its job id is recorded: the
    // completion is matched against that id, and a result nobody recorded is dropped
    // (which is exactly what happened while this was a bare `runner.submit`).
    apply(
        Effect::DetectEnvironment,
        &mut app,
        state_store.as_ref(),
        &mut runner,
        &mut terminal,
    );

    // A model configured in an earlier run is resolved from the cache, if there is
    // one: the status line can then name it, and `:model show` can say what is wrong
    // with it, without the 4 MB catalog fetch that the picker asks for when it opens
    // (FR-4.7 keeps the network for an explicit request). When there is no cache the
    // fetch follows by itself, because a user who has already chosen a model should
    // not have to open a picker to make the app notice — see `catalog_unavailable`.
    if app.config.llm.active.is_some() {
        let _ = apply(
            Effect::LoadCatalog(crate::ports::catalog::CatalogPolicy::CacheOnly),
            &mut app,
            state_store.as_ref(),
            &mut runner,
            &mut terminal,
        );
    }

    while !app.should_quit() {
        app.set_now(clock.now_unix_secs());
        terminal.draw(|frame| app.render(frame))?;
        app.tick();

        // Results from background jobs, then whatever they asked for next.
        let mut queued = drain_completions(
            &mut app,
            &mut runner,
            forge_factory.as_ref(),
            &cache,
            &clock_source,
            &workspace_for_executor,
        );

        let effect = if event::poll(app.poll_timeout())? {
            match event::read()? {
                event::Event::Key(key) if key.kind != event::KeyEventKind::Release => {
                    app.on_key(key)
                }
                event::Event::Mouse(mouse) => app.on_mouse(mouse),
                // Resizing needs no handling: the next draw reads the new size, and
                // the app keeps no cached geometry (FR-7.8).
                _ => Effect::None,
            }
        } else {
            app.on_timeout()
        };

        queued.extend(apply(
            effect,
            &mut app,
            state_store.as_ref(),
            &mut runner,
            &mut terminal,
        ));
        drain_effects(
            queued,
            &mut app,
            state_store.as_ref(),
            &mut runner,
            &mut terminal,
        );
    }

    runner.cancel_all();
    logging::log(Level::Info, "shutting down normally");
    Ok(())
}

/// Performs whatever the reducer asked for.
///
/// This is the only function in `tui` that touches the outside world, which is what
/// lets [`App::render`] stay pure and keeps the loop responsive (NFR-1.2).
pub(crate) fn apply(
    effect: Effect,
    app: &mut App,
    state_store: &dyn StateStore,
    runner: &mut JobRunner,
    terminal: &mut terminal::TerminalGuard,
) -> Vec<Effect> {
    // Effects that produce further effects queue them here rather than recursing, so
    // a chain cannot nest the stack; the caller drains the queue.
    let mut pending_effects: Vec<Effect> = Vec::new();
    match effect {
        Effect::None | Effect::KeepPending => {}

        Effect::SaveState => {
            if let Err(error) = state_store.save(&app.state) {
                app.notice(
                    app::NoticeLevel::Warn,
                    format!("could not save state: {error}"),
                );
            }
        }

        Effect::DetectEnvironment
        | Effect::LoadPullRequests
        | Effect::CountPullRequests
        | Effect::LoadMore
        | Effect::OpenPullRequest(_)
        | Effect::RunDoctor => {
            let context = app.doctor_request();
            if let Some(job) = jobs::job_for(&effect, &app.list, context) {
                let id = runner.submit(job);
                app.record_job(&effect, id);
            }
        }

        Effect::CancelInFlight => {
            // Cancelling the slot rather than the process only: whichever job is
            // running for this screen kills its child within a poll interval
            // (NFR-1.4).
            runner.cancel(jobs::Slot::List);
            runner.cancel(jobs::Slot::Count);
            runner.cancel(jobs::Slot::Detail);
            runner.cancel(jobs::Slot::Patch);
            app.cancelled_in_flight();
        }

        Effect::SetMouse(enabled) => {
            // Only the loop owns the terminal, so the toggle is applied here.
            if let Err(error) = terminal.set_mouse(enabled) {
                app.notice(
                    app::NoticeLevel::Warn,
                    format!("could not change the mouse setting: {error}"),
                );
            }
        }

        Effect::ReloadDiff => reload_diff(app, runner, &mut pending_effects),

        Effect::CopyPath(path) => {
            // OSC 52 asks the terminal to set the clipboard. A terminal that does not
            // implement it simply ignores the sequence, so the notice says what was
            // attempted rather than claiming it worked.
            let sequence = clipboard::osc52(&path);
            let mut stdout = std::io::stdout();
            match stdout
                .write_all(sequence.as_bytes())
                .and_then(|()| stdout.flush())
            {
                Ok(()) => app.notice(
                    app::NoticeLevel::Info,
                    format!("copied {path} (via the terminal's clipboard)"),
                ),
                Err(error) => app.notice(
                    app::NoticeLevel::Warn,
                    format!("could not copy {path}: {error}"),
                ),
            }
        }

        // The analysis effects are their own group for the same reason the model
        // ones are: they share state, and they queue each other (a gather is followed
        // by the request it was gathered for).
        Effect::LoadAnalysis
        | Effect::GatherContext(_)
        | Effect::RunAnalysis { .. }
        | Effect::CancelAnalysis
        | Effect::SavePlan(_) => {
            let _ = apply_analysis_effect(&effect, app, runner, &mut pending_effects);
        }

        // The model, key and workspace effects are their own group: they share the
        // picker's state and they queue work for each other (a saved key commits the
        // selection, which then asks to be checked).
        Effect::LoadCatalog(_)
        | Effect::EnsureWorkspace(_)
        | Effect::CheckModel
        | Effect::SaveKey { .. }
        | Effect::SaveSelection(_)
        | Effect::ClearKey(_)
        | Effect::CleanWorkspaces(_) => {
            let _ = apply_model_effect(&effect, app, runner, &mut pending_effects);
        }
    }

    pending_effects
}

/// Reloads the diff of the open pull request, from the best source available (FR-3.2).
///
/// Extracted from [`apply`] because it is the one effect with a decision in it — the
/// worktree if the code is on disk, the forge otherwise — plus two follow-ups.
fn reload_diff(app: &mut App, runner: &mut JobRunner, pending_effects: &mut Vec<Effect>) {
    // The head SHA keys the cached diff, so a reload needs the open PR's.
    let Some(detail) = app.detail.as_ref() else {
        return;
    };
    let number = detail.summary.number;
    let head_sha = detail.summary.head_sha.clone();
    let options = app.diff_options;

    // Which source answers depends on whether the code is on disk yet (FR-3.2). The
    // forge is always the fallback: a worktree that cannot be built must not mean a
    // diff that cannot be read.
    let job = match (&app.workspace, app.workspace_ready()) {
        (Some(workspace), true) => Job::LocalPatch {
            request: Box::new(crate::ports::workspace::DiffRequest {
                path: workspace.path.clone(),
                base_sha: workspace.base_sha.clone(),
                head_sha: workspace.head_sha.clone(),
                options,
            }),
            number,
            head_sha: head_sha.clone(),
            options,
        },
        _ => Job::Patch { number, head_sha },
    };
    let id = runner.submit(job);
    app.patch_job = id;
    app.diff_loading = true;
    // The second step of the same wait: fetching a large diff is the slow half.
    app.advance_opening();

    // The analysis cache is checked on the same occasion: a diff is reloaded when a
    // pull request opens and when its context changes, and both are moments when the
    // head may have moved (FR-4.3).
    pending_effects.push(Effect::LoadAnalysis);

    // Ask for the worktree in the background while the diff is being read, but only
    // once per pull request: this handler runs again for every context or whitespace
    // change (FR-3.1).
    if app.workspace.is_none() && app.workspace_job == 0 && app.wants_workspace() {
        pending_effects.push(Effect::EnsureWorkspace(number));
    }
}

/// Handles the effects that read, gather or run an analysis (FR-4.1, FR-4.3, FR-4.6).
///
/// Returns whether the effect belonged to this group, so `apply` stays exhaustive.
fn apply_analysis_effect(
    effect: &Effect,
    app: &mut App,
    runner: &mut JobRunner,
    pending_effects: &mut Vec<Effect>,
) -> bool {
    match effect {
        Effect::LoadAnalysis => {
            let Some(key) = app.analysis_key() else {
                // Nothing is open, or no model is chosen: there is no question to ask
                // the cache, and asking it with half a key would be a bug.
                return true;
            };
            let id = runner.submit(jobs::Job::LoadAnalysis { key: Box::new(key) });
            app.record_stored_job(id);
        }

        Effect::GatherContext(intent) => {
            let Some(request) = app.analysis_request() else {
                app.notice(
                    app::NoticeLevel::Warn,
                    if app.has_model() {
                        "open a pull request first".to_owned()
                    } else {
                        "choose a provider and model first: <leader>m".to_owned()
                    },
                );
                return true;
            };
            app.panel.state = app::AnalysisState::Gathering;
            let id = runner.submit(jobs::Job::GatherContext {
                request: Box::new(request),
                intent: *intent,
            });
            app.record_context_job(id);
        }

        Effect::RunAnalysis { force } => {
            // Without a bundle there is nothing the user agreed to send, so the run
            // starts by gathering one (FR-4.6).
            let Some(bundle) = app.take_context_bundle_for_current_head() else {
                pending_effects.push(Effect::GatherContext(AnalysisIntent::Estimate));
                return true;
            };
            if *force {
                // `--force` recomputes; the cache is not consulted and the result
                // replaces the entry (FR-4.3).
                app.forget_analysis();
            }
            let Some(request) = app.analysis_request() else {
                return true;
            };
            app.begin_analysis();
            let id = runner.submit(jobs::Job::RunAnalysis {
                request: Box::new(request),
                bundle: Box::new(bundle),
            });
            app.record_analysis_job(id);
        }

        Effect::CancelAnalysis => {
            runner.cancel(jobs::Slot::Analyze);
            runner.cancel(jobs::Slot::Analysis);
            app.cancelled_analysis();
        }

        Effect::SavePlan(plan) => {
            // A small, local write next to the analysis it belongs to: the same
            // exception the config write-back gets, and it happens here because this
            // is where the cache lives.
            let Some(repo) = app
                .environment()
                .map(|environment| environment.repo.clone())
            else {
                return true;
            };
            let Some(pr) = app.detail.as_ref().map(|detail| detail.summary.number) else {
                return true;
            };
            if let Err(error) = app.analysis_cache.put_plan(&repo, pr, plan) {
                app.notice(
                    app::NoticeLevel::Warn,
                    format!("could not save the review order: {error}"),
                );
            }
        }

        _ => return false,
    }
    true
}

/// Handles the effects that configure a model, a key or the worktrees.
///
/// Returns whether the effect belonged to this group, so `apply` can stay exhaustive
/// without carrying twenty lines of arms that belong together.
fn apply_model_effect(
    effect: &Effect,
    app: &mut App,
    runner: &mut JobRunner,
    pending_effects: &mut Vec<Effect>,
) -> bool {
    match effect {
        Effect::LoadCatalog(policy) => {
            let id = runner.submit(jobs::Job::Catalog { policy: *policy });
            app.record_catalog_job(id);
        }

        Effect::EnsureWorkspace(number) => {
            // Without a resolved repository and an open detail there is nothing to
            // fetch, and saying nothing is right: the request came from the loop.
            let (Some(detail), Some(repo)) = (
                app.detail.as_ref(),
                app.environment
                    .as_ref()
                    .map(|environment| environment.repo.clone()),
            ) else {
                return true;
            };
            // The remote the repository was identified from is the one to fetch: a
            // fork's pull request lives on the base repository's `refs/pull/N/head`,
            // which is exactly where this looks (FR-3.1).
            let remote = app
                .environment
                .as_ref()
                .and_then(|environment| environment.remote.clone())
                .unwrap_or_else(|| "origin".to_owned());
            let request = workspace_request(&repo, &remote, detail, *number);
            let id = runner.submit(jobs::Job::Workspace {
                request: Box::new(request),
            });
            app.record_workspace_job(id);
        }

        Effect::SaveKey { provider, key } => {
            // A local, bounded write: the same exception the theme and keybind files
            // get, and it happens here because this is where the store lives.
            match app.secret_store().set(provider, key) {
                Ok(()) => {
                    app.picker_key_stored();
                    // Storing a key is the last step of the picker's flow, so the
                    // selection is committed straight away: the user typed a key in
                    // order to use a model, not to keep choosing.
                    if let Some(selection) = app.picker_selection() {
                        pending_effects.push(Effect::SaveSelection(Box::new(selection)));
                    }
                }
                Err(error) => {
                    let message = format!("could not save the key: {error}");
                    if let Some(picker) = app.picker_mut() {
                        picker.set_notice(Some(message));
                    }
                }
            }
        }

        Effect::SaveSelection(selection) => {
            match crate::config::write_selection(&app.config_path, selection) {
                Ok(()) => {
                    // The running session uses it immediately: no restart (FR-4.5).
                    app.set_active_selection((**selection).clone());
                    app.close_picker();
                    pending_effects.push(Effect::CheckModel);
                }
                Err(error) => {
                    let message = error.to_string();
                    if let Some(picker) = app.picker_mut() {
                        picker.set_notice(Some(message));
                    }
                }
            }
        }

        Effect::CheckModel => {
            if let Some(request) = app.check_request() {
                let id = runner.submit(jobs::Job::ModelCheck {
                    request: Box::new(request),
                });
                app.record_check_job(id);
            }
        }

        Effect::ClearKey(provider) => match app.secret_store().remove(provider) {
            Ok(()) => {
                app.resolve_active_model();
                app.notice(
                    app::NoticeLevel::Info,
                    format!("cleared the stored key for {provider}"),
                );
            }
            Err(error) => app.notice(
                app::NoticeLevel::Error,
                format!("could not clear the key: {error}"),
            ),
        },

        Effect::CleanWorkspaces(all) => {
            // Removing worktrees is git work, and git work is a job (ARCH-5).
            let id = runner.submit(jobs::Job::CleanWorkspaces {
                all: *all,
                max_age_secs: u64::from(app.config.workspace.auto_clean_days) * 24 * 60 * 60,
            });
            app.record_workspace_job(id);
        }

        _ => return false,
    }
    true
}

/// The request that materialises a pull request (FR-3.1).
fn workspace_request(
    repo: &RepoId,
    remote: &str,
    detail: &crate::domain::pr::PullRequestDetail,
    number: u64,
) -> crate::ports::workspace::WorkspaceRequest {
    crate::ports::workspace::WorkspaceRequest {
        repo: repo.clone(),
        remote: remote.to_owned(),
        number,
        base: detail.summary.base_ref.clone(),
        head_sha: detail.summary.head_sha.clone(),
    }
}

/// Collects finished jobs, tells the app, and returns the effects it asked for.
///
/// The executor is installed the moment detection resolves a repository, which is why
/// the ports for repository-scoped jobs are built here: before that there is no
/// repository to scope them to (FR-1.1).
fn drain_completions(
    app: &mut App,
    runner: &mut JobRunner,
    factory: &dyn crate::ports::ForgeFactory,
    cache: &Arc<dyn crate::ports::CacheStore>,
    clock: &Arc<dyn Clock>,
    workspace: &Arc<dyn crate::ports::WorkspacePort>,
) -> Vec<Effect> {
    // Streaming text arrives on its own channel, so it is drained with the
    // completions: both are "what the background has to say right now" (FR-4.4).
    for progress in runner.poll_progress() {
        app.apply_progress(progress);
    }
    let mut follow_ups: Vec<Effect> = Vec::new();
    for completion in runner.poll() {
        let detected = matches!(completion.outcome, Outcome::Environment(_));
        let effect = app.apply_completion(completion);
        if detected
            && let Some(executor) = executor_for(
                app,
                factory,
                cache.clone(),
                clock.clone(),
                workspace.clone(),
                app.analysis_cache.clone(),
                runner.llm(),
            )
        {
            runner.set_executor(executor);
            // Cache first: painting what is already on disk before asking the network
            // is the difference between an instant first list and a `gh` round trip
            // (FR-2.3). The fetch this triggers replaces it in place, keeping the
            // cursor on the same pull request.
            runner.submit(Job::CachedList {
                query: app.list.query(),
            });
        }
        if let Some(effect) = effect {
            follow_ups.push(effect);
        }
    }
    follow_ups
}

/// Applies queued effects, including the ones they queue in turn.
///
/// Bounded, because a cycle in the follow-up graph would spin the loop with the
/// terminal taken over — the worst possible failure for a TUI.
fn drain_effects(
    mut queued: Vec<Effect>,
    app: &mut App,
    state_store: &dyn StateStore,
    runner: &mut JobRunner,
    terminal: &mut terminal::TerminalGuard,
) {
    let mut guard = 0;
    while let Some(effect) = queued.pop() {
        guard += 1;
        if guard > MAX_FOLLOW_UPS {
            logging::log(
                Level::Error,
                "the work queued by one input did not settle; stopping it",
            );
            return;
        }
        let mut more = apply(effect, app, state_store, runner, terminal);
        queued.append(&mut more);
    }
}

/// How many effects one input may queue behind it before the loop gives up.
///
/// A number rather than "as many as it takes": a cycle in the follow-up graph would
/// otherwise spin the event loop with the terminal taken over.
const MAX_FOLLOW_UPS: usize = 16;

/// Builds the ports a repository-scoped job needs, once detection has resolved one.
pub(crate) fn executor_for(
    app: &App,
    factory: &dyn crate::ports::ForgeFactory,
    cache: Arc<dyn crate::ports::CacheStore>,
    clock: Arc<dyn Clock>,
    workspace: Arc<dyn crate::ports::WorkspacePort>,
    analysis: Arc<dyn crate::ports::AnalysisCachePort>,
    llm: std::sync::Arc<dyn crate::ports::LlmPort>,
) -> Option<Arc<Executor>> {
    let repo: RepoId = app.environment.as_ref()?.repo.clone();
    Some(Arc::new(Executor::new(jobs::ExecutorPorts {
        forge: factory.forge(&repo),
        cache,
        clock,
        workspace,
        analysis,
        llm,
        repo,
        policy: CachePolicy {
            list_ttl_secs: app.config.cache.ttl_list_secs,
            detail_ttl_secs: app.config.cache.ttl_detail_secs,
            ..CachePolicy::default()
        },
    })))
}
