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
use crate::adapters::cache::DiskCache;
use crate::adapters::clock::SystemClock;
use crate::adapters::gh::GhCliForge;
use crate::adapters::gh::probe::GhCliProbe;
use crate::adapters::git::GitCli;
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
    let state_store: Box<dyn StateStore> = Box::new(startup.state_store.clone());

    let workspace = Arc::new(if let Some(path) = startup.path.clone() {
        GitCli::new().in_dir(path)
    } else {
        GitCli::new()
    });
    let probe = Arc::new(GhCliProbe::new(startup.config.forge.gh_path.clone()));
    let request = DetectRequest {
        repo: startup.repo.clone(),
        gh_program: Some(startup.config.forge.gh_path.clone()),
    };
    let cache_root = startup.home.cache();

    let mut app = App::new(startup)?;
    let mut terminal = terminal::TerminalGuard::enter(app.mouse_enabled())?;
    let mut runner = JobRunner::new(workspace, probe, request);

    // Detection is a job, not a startup step: it runs `gh auth status`, which reaches
    // the network, and the first frame must not wait for it (NFR-1.1).
    runner.submit(Job::Detect);

    while !app.should_quit() {
        app.set_now(clock.now_unix_secs());
        terminal.draw(|frame| app.render(frame))?;
        app.tick();

        // Results from background jobs, then whatever they asked for next.
        let mut follow_ups: Vec<Effect> = Vec::new();
        for completion in runner.poll() {
            // A repository is only known once detection has answered, so the ports
            // that need one are built at that moment and kept for the session.
            let detected = matches!(completion.outcome, Outcome::Environment(_));
            let effect = app.apply_completion(completion);
            if detected && let Some(executor) = executor_for(&app, &cache_root) {
                runner.set_executor(executor);
            }
            if let Some(effect) = effect {
                follow_ups.push(effect);
            }
        }
        for effect in follow_ups {
            apply(effect, &mut app, state_store.as_ref(), &mut runner);
        }

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

        apply(effect, &mut app, state_store.as_ref(), &mut runner);
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
) {
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

        Effect::ReloadDiff => {
            // The head SHA keys the cached diff, so a reload needs the open PR's.
            let Some(head_sha) = app
                .detail
                .as_ref()
                .map(|detail| detail.summary.head_sha.clone())
            else {
                return;
            };
            let Some(number) = app.detail.as_ref().map(|detail| detail.summary.number) else {
                return;
            };
            let id = runner.submit(Job::Patch { number, head_sha });
            app.patch_job = id;
            app.diff_loading = true;
        }

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
    }
}

/// Builds the ports a repository-scoped job needs, once detection has resolved one.
pub(crate) fn executor_for(app: &App, cache_root: &std::path::Path) -> Option<Arc<Executor>> {
    let environment = app.environment.as_ref()?;
    let repo: RepoId = environment.repo.clone();
    let forge = GhCliForge::new(&app.config.forge.gh_path, repo.clone());
    Some(Arc::new(Executor::new(
        Arc::new(forge),
        Arc::new(DiskCache::new(cache_root)),
        Arc::new(SystemClock),
        repo,
        CachePolicy {
            list_ttl_secs: app.config.cache.ttl_list_secs,
            detail_ttl_secs: app.config.cache.ttl_detail_secs,
            ..CachePolicy::default()
        },
    )))
}
