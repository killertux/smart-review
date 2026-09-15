//! Actions and the `:` command line (FR-7.2, FR-7.3, FR-7.4).
//!
//! Every action in the registry is dispatched from here, so adding a feature
//! means adding a registry entry, a default binding and a match arm. Dispatch
//! returns an [`Effect`] rather than performing IO, so the reducer stays pure.

use std::time::Duration;

use crate::application::analysis::AnalysisIntent;
use crate::tui::action;
use crate::tui::app::{App, Effect, NoticeLevel, Overlay};
use crate::tui::keymap::{self, KeyCombo, Mode};

/// Commands accepted by the `:` line: name, the action it triggers, and the
/// description the palette shows.
///
/// The display text lives here rather than being taken from the action because a
/// command can be more specific than the action behind it (`:set` uses the
/// command-line machinery but is not "open the command line"). The action ids
/// themselves all come from the registry (`action::ACTIONS`), which a test
/// enforces.
pub const COMMANDS: &[(&str, &str, &str)] = &[
    ("doctor", "app.doctor", "Show the environment report"),
    ("help", "app.help", "Show the help popup"),
    ("keymap", "app.help", "List the active keybindings"),
    (
        "messages",
        "notice.clear",
        "Dismiss the current notification",
    ),
    (
        "clear-filters",
        "filter.clear",
        "Reset the filters and the search",
    ),
    (
        "copy-path",
        "review.copy_path",
        "Copy the current file path",
    ),
    (
        "filter",
        "app.command",
        "Add a filter: :filter author:alice",
    ),
    (
        "load-more",
        "app.load_more",
        "Fetch the next page of pull requests",
    ),
    ("pr", "nav.open", "Open a pull request: :pr 141"),
    ("q", "app.quit", "Quit smart-review"),
    ("qa", "app.quit", "Quit smart-review"),
    ("quit", "app.quit", "Quit smart-review"),
    ("refresh", "app.refresh", "Refresh the current view"),
    (
        "set",
        "app.command",
        "Change an option: :set ui.timeoutlen=250",
    ),
    (
        "sort",
        "app.command",
        "Change the order: :sort updated desc",
    ),
    (
        "theme",
        "app.theme_picker",
        "Choose a theme, or :theme <name>|reload",
    ),
    (
        "model",
        "app.model_picker",
        "Choose the provider, model and thinking",
    ),
    (
        "key",
        "app.command",
        "Clear a stored key: :key clear <provider>",
    ),
    (
        "catalog",
        "app.command",
        "Refresh the model catalog: :catalog refresh",
    ),
    (
        "workspace",
        "app.command",
        "Manage worktrees: :workspace clean [--all]",
    ),
    (
        "context",
        "app.command",
        "Show, extend or narrow what is sent: :context [add|remove <path>|0-1000]",
    ),
    (
        "edit",
        "app.command",
        "Edit the open comment composer in $EDITOR",
    ),
    (
        "chat",
        "chat.open",
        "Talk about this pull request: :chat [new|list|open <id>|export [md|json]|retry]",
    ),
    (
        "analyze",
        "app.analyze_panel",
        "Analyse the open pull request: :analyze [--force|raw]",
    ),
    (
        "plan",
        "app.command",
        "Review order: :plan move <file> <group> | reset | path",
    ),
    ("version", "app.version", "Show the version"),
];

/// Runs an action.
///
/// Long by construction: it is the one table mapping an action id to what it does, and
/// every arm is a line or two. Splitting it would separate the ids from their
/// behaviour, which is the pairing the tests and the help popup both read.
///
/// The returned [`Effect`] tells the event loop what it has to do; only the
/// leader menu asks to keep the pending key sequence so the next key can
/// complete it (FR-7.3).
#[allow(clippy::too_many_lines)]
pub fn dispatch(app: &mut App, id: &str) -> Effect {
    match id {
        "app.quit" => {
            app.quit();
            Effect::None
        }
        "app.help" => {
            app.open_overlay(Overlay::Help);
            Effect::None
        }
        "app.doctor" => {
            app.start_doctor();
            Effect::RunDoctor
        }
        "app.load_more" => load_more(app),
        "app.theme_picker" => {
            app.open_overlay(Overlay::ThemePicker);
            Effect::None
        }
        "app.model_picker" => app.open_model_picker(),
        "review.comment_line" => app.start_comment(),
        "review.approve" => app.set_draft_decision("approve"),
        "review.request_changes" => app.set_draft_decision("request-changes"),
        "review.comment_only" => app.set_draft_decision("comment"),
        "review.discard" => app.ask_clear_draft(),
        "review.range" => app.start_selection(),
        "review.drafts" => app.open_drafts(),
        "review.reply" => app.start_reply(),
        "review.edit_composer" => edit_composer_command(app, ""),
        "review.toggle_resolved" => app.ask_toggle_thread(),
        "review.conversation" => app.open_conversation(),
        "review.comment_conversation" => app.start_conversation_comment(),
        "review.publish" => app.open_publish(),
        "review.remove" => app.remove_staged(),
        "app.analyze_panel" => {
            // `<leader>a` opens what is there and runs what is not: a user who has an
            // analysis wants to read it, and a user who has none wants one.
            if app.analysis_panel().is_some()
                || app.analysis_state().is_running()
                || app.raw_answer().is_some()
            {
                app.open_overlay(Overlay::Analysis);
                Effect::None
            } else {
                start_analysis(app, "")
            }
        }
        "app.leader_menu" => {
            app.open_overlay(Overlay::Leader);
            Effect::KeepPending
        }
        "app.command" => {
            // Leaving a popup open behind the command line would break the
            // mode/overlay pairing and leave its keys live (FR-7.1).
            app.cancel_overlay();
            app.command.clear();
            app.mode = Mode::Command;
            Effect::None
        }
        "app.cancel" => {
            // An open popup is what `Esc` closes first; a run in flight is what it
            // cancels next (FR-4.4). Doing both at once would make the key feel like
            // it did nothing.
            if app.overlay() != Overlay::None {
                app.cancel_overlay();
                return Effect::None;
            }
            if app.analysis_state().is_running() {
                return Effect::CancelAnalysis;
            }
            if app.mode != crate::tui::keymap::Mode::Normal {
                app.mode = crate::tui::keymap::Mode::Normal;
            }
            Effect::None
        }
        "app.refresh" => {
            // `R` means "ask again for whatever I am looking at".
            if app.review.is_some() {
                Effect::ReloadDiff
            } else if app.environment.is_none() {
                Effect::DetectEnvironment
            } else {
                app.list.stale = None;
                Effect::LoadPullRequests
            }
        }
        "pane.next" => switch_pane(app, true),
        "pane.prev" => switch_pane(app, false),
        // The groups live in their own functions so that no single match has to hold
        // the whole command surface.
        other if other.starts_with("chat.") => dispatch_chat(app, other),
        other
            if other.starts_with("app.")
                || other.starts_with("notice.")
                || other.starts_with("theme.")
                || other.starts_with("plan.") =>
        {
            dispatch_app(app, other)
        }
        other if other.starts_with("diff.") || other == "review.copy_path" => {
            dispatch_diff(app, other)
        }
        other
            if other.starts_with("nav.")
                || other.starts_with("search.")
                || other.starts_with("filter.")
                || other == "sort.menu" =>
        {
            dispatch_list(app, other)
        }
        other => {
            app.notice(
                NoticeLevel::Warn,
                format!("`{other}` is not implemented in this build"),
            );
            Effect::None
        }
    }
}

/// Changes the focused pane and remembers it (FR-8.5).
/// The application lifecycle and view actions.
/// `o`: switches between the recommended and path orders (FR-3.5).
///
/// Both positions are reported, because the point of the toggle is comparison: the
/// user wants to know where the file they are reading sits in each order, not merely
/// that something changed.
fn toggle_order(app: &mut App) -> Effect {
    let Some(view) = app.review_mut() else {
        app.notice(NoticeLevel::Warn, "open a pull request first".to_owned());
        return Effect::None;
    };
    if view.plan.is_none() {
        app.notice(
            NoticeLevel::Info,
            "only one order so far: <leader>a analyses the pull request, or the diff's own \
             path order is kept"
                .to_owned(),
        );
        return Effect::None;
    }
    view.toggle_order();
    let order = view.order;
    let positions = view
        .order_positions()
        .map(|positions| format!(" · {positions}"))
        .unwrap_or_default();
    let source = view
        .plan
        .as_ref()
        .map(|plan| plan.source.label())
        .unwrap_or_default();
    app.notice(
        NoticeLevel::Info,
        format!("{} ({source}){positions}", order.label()),
    );
    Effect::None
}

/// The chat actions (FR-5.1–5.3).
fn dispatch_chat(app: &mut App, id: &str) -> Effect {
    match id {
        "chat.open" => {
            // `<leader>c` opens what is there and loads what is not: a user with a
            // conversation wants to read it, and a user with none wants somewhere to
            // type. The load is what finds the conversation from the last run (FR-5.1).
            if app.chat_state().is_some() {
                app.chat.close();
                app.mode = crate::tui::keymap::Mode::Normal;
                return Effect::None;
            }
            app.show_chat();
            let effect = set_focus(app, crate::tui::app::Pane::Chat);
            if app.chat.session.is_some() {
                effect
            } else {
                Effect::LoadChat
            }
        }
        "chat.send" => Effect::AskChat,
        "chat.newline" => {
            // Only meaningful while the compose box owns the keys, which is the mode
            // this action is scoped to.
            app.chat.input.newline();
            Effect::None
        }
        "chat.retry" => Effect::RetryChat,
        "chat.cancel" => {
            if app.chat.status.is_running() {
                return Effect::CancelChat;
            }
            if app.chat.is_confirming() {
                app.chat.awaiting_confirmation = None;
                app.notice(NoticeLevel::Info, "nothing was sent".to_owned());
                return Effect::None;
            }
            // Otherwise `Esc` leaves the pane, which is what it means everywhere else.
            app.chat.close();
            set_focus(app, crate::tui::app::Pane::Diff);
            Effect::None
        }
        "chat.list" => {
            app.list_chats();
            Effect::None
        }
        "chat.export" => Effect::ExportChat("md".to_owned()),
        other => unimplemented_action(app, other),
    }
}

fn dispatch_app(app: &mut App, id: &str) -> Effect {
    match id {
        "plan.move_up" | "plan.move_down" => move_plan_group(app, id == "plan.move_down"),
        "plan.reset" => reset_plan(app),
        "app.version" => {
            app.notice(
                NoticeLevel::Info,
                format!("smart-review {}", env!("CARGO_PKG_VERSION")),
            );
            Effect::None
        }
        "notice.clear" => {
            app.dismiss_notices();
            Effect::None
        }
        "theme.toggle" => app.toggle_theme(),
        other => unimplemented_action(app, other),
    }
}

/// Reports an action that reached a sub-dispatcher with no arm for it.
///
/// Silently returning `Effect::None` would make a missing arm look like a working
/// no-op, and it is what the "every registered action is dispatched" test looks for.
fn unimplemented_action(app: &mut App, id: &str) -> Effect {
    app.notice(
        NoticeLevel::Warn,
        format!("`{id}` is not implemented in this build"),
    );
    Effect::None
}

/// The list, search and filter actions.
fn dispatch_list(app: &mut App, id: &str) -> Effect {
    match id {
        "nav.up" | "nav.down" | "nav.top" | "nav.bottom" => {
            // `move_current` routes to whichever pane has the cursor: the list, or the
            // tree/diff of an open review. Calling the list directly here is what left
            // `j`/`k` moving a cursor nothing drew.
            match id {
                "nav.up" => move_current(app, Movement::Rows(-1)),
                "nav.down" => move_current(app, Movement::Rows(1)),
                "nav.top" => move_to_end(app, false),
                _ => move_to_end(app, true),
            }
            Effect::None
        }
        "nav.open" => open_selected(app),
        "nav.back" => go_back(app),
        "nav.half_down" | "nav.page_down" | "nav.half_up" | "nav.page_up" => {
            let forward = matches!(id, "nav.half_down" | "nav.page_down");
            let direction = if forward { 1 } else { -1 };
            let movement = match id {
                "nav.half_down" | "nav.half_up" => Movement::Half(direction),
                _ => Movement::Page(direction),
            };
            move_current(app, movement);
            Effect::None
        }
        "search.open" => {
            app.cancel_overlay();
            app.mode = Mode::Search;
            Effect::None
        }
        "search.close" => {
            // Leaving the search box keeps what was typed: it is a filter, not a
            // half-finished command, and `Esc` again on the list clears it.
            app.mode = Mode::Normal;
            Effect::None
        }
        "search.next" => {
            move_current(app, Movement::Rows(1));
            Effect::None
        }
        "search.prev" => {
            move_current(app, Movement::Rows(-1));
            Effect::None
        }
        "filter.menu" => {
            app.open_command("filter ");
            Effect::None
        }
        "sort.menu" => {
            app.open_command("sort ");
            Effect::None
        }
        "filter.clear" => {
            app.list.clear_filters();
            app.list.stale = None;
            if app.environment.is_some() {
                Effect::LoadPullRequests
            } else {
                Effect::None
            }
        }
        other => unimplemented_action(app, other),
    }
}

/// The diff actions, which all need the review screen to be open.
fn dispatch_diff(app: &mut App, id: &str) -> Effect {
    match id {
        "diff.next_hunk" => {
            with_diff(app, |view| view.move_hunk(true));
            Effect::None
        }
        "diff.prev_hunk" => {
            with_diff(app, |view| view.move_hunk(false));
            Effect::None
        }
        "diff.next_file" => {
            with_diff(app, |view| {
                view.tree_focused = false;
                view.move_file(true);
            });
            Effect::None
        }
        "diff.prev_file" => {
            with_diff(app, |view| {
                view.tree_focused = false;
                view.move_file(false);
            });
            Effect::None
        }
        "diff.toggle_hunk" => {
            with_diff(app, crate::tui::diff_view::DiffView::toggle_hunk);
            Effect::None
        }
        "diff.toggle_split" => toggle_split(app),
        "diff.toggle_order" => toggle_order(app),
        "diff.cycle_context" => cycle_context(app),
        "diff.toggle_whitespace" => toggle_whitespace(app),
        "review.copy_path" => copy_path(app),
        other => unimplemented_action(app, other),
    }
}

/// Puts the path of the file under the cursor on the clipboard (FR-3.4).
fn copy_path(app: &mut App) -> Effect {
    let path = app
        .review
        .as_ref()
        .and_then(crate::tui::diff_view::DiffView::current_path)
        .map(ToString::to_string);
    if let Some(path) = path {
        Effect::CopyPath(path)
    } else {
        app.notice(NoticeLevel::Warn, "there is no file under the cursor");
        Effect::None
    }
}

/// `:load-more`, or the `app.load_more` action.
///
/// Three different situations, three different sentences: everything is already
/// shown, the configured cap has been reached (which is *not* the same as having
/// everything), or there is another page to fetch (FR-2.1).
fn load_more(app: &mut App) -> Effect {
    if app.review.is_some() {
        app.notice(
            NoticeLevel::Warn,
            "loading more applies to the list; press Esc to go back",
        );
        return Effect::None;
    }
    if app.list.holds_everything() {
        app.notice(
            NoticeLevel::Info,
            format!(
                "all {} matching pull requests are shown",
                app.list.items.len()
            ),
        );
        return Effect::None;
    }
    if !app.list.can_load_more() {
        app.notice(
            NoticeLevel::Warn,
            format!(
                "the {}-pull-request cap is reached; raise [review].page_size or max_pages to see more",
                app.list.cap
            ),
        );
        return Effect::None;
    }
    Effect::LoadMore
}

/// `:filter-remove 2` removes one chip (FR-2.2).
fn remove_filter(app: &mut App, argument: &str) -> Effect {
    // The chips are the state chip plus the filters; the state chip is index one.
    match argument.trim().parse::<usize>() {
        Ok(index) if index >= 1 => {
            if app.list.remove_chip(index) {
                Effect::LoadPullRequests
            } else {
                app.command_error(format!("there is no chip {index}"));
                Effect::None
            }
        }
        _ => {
            app.command_error(format!(
                ":filter-remove needs a chip number; the chips are {}",
                app.list.chips().join(" ")
            ));
            Effect::None
        }
    }
}

/// `:pr N` opens that pull request (FR-7.4).
fn open_pr(app: &mut App, argument: &str) -> Effect {
    let trimmed = argument.trim().trim_start_matches('#');
    match trimmed.parse::<u64>() {
        Ok(number) => Effect::OpenPullRequest(number),
        Err(_) if trimmed.is_empty() => {
            app.command_error(":pr needs a number, e.g. :pr 141");
            Effect::None
        }
        Err(_) => {
            app.command_error(format!("`{trimmed}` is not a pull request number"));
            Effect::None
        }
    }
}

/// `:filter author:alice` adds a chip and re-asks GitHub (FR-2.2).
fn add_filter(app: &mut App, argument: &str) -> Effect {
    if argument.is_empty() {
        app.command_error(":filter needs a qualifier, e.g. :filter author:alice");
        return Effect::None;
    }
    match crate::domain::query::Filter::parse_line(argument) {
        Ok(filter) => {
            app.list.push_filter(filter);
            app.list.stale = None;
            Effect::LoadPullRequests
        }
        Err(error) => {
            // Refused rather than sent: a qualifier GitHub silently ignores looks
            // like a bug in this app.
            app.command_error(error.to_string());
            Effect::None
        }
    }
}

/// `:sort updated desc` changes the order GitHub is asked for (FR-7.4).
fn set_sort(app: &mut App, argument: &str) -> Effect {
    let mut parts = argument.split_whitespace();
    let field = parts.next().unwrap_or_default();
    let direction = parts.next().unwrap_or("desc");
    if field.is_empty() {
        app.command_error(":sort needs a field: created or updated, then asc or desc");
        return Effect::None;
    }
    let ascending = match direction {
        "asc" => true,
        "desc" => false,
        other => {
            app.command_error(format!("`{other}` is not a direction; use asc or desc"));
            return Effect::None;
        }
    };
    if let Some(sort) = crate::domain::query::PrSort::parse(field, ascending) {
        app.list.sort = sort;
        app.list.stale = None;
        return Effect::LoadPullRequests;
    }
    app.command_error(format!("`{field}` is not sortable; use created or updated"));
    Effect::None
}

/// Opens whatever the cursor is on: a pull request in the list, a file in the tree.
fn open_selected(app: &mut App) -> Effect {
    if let Some(view) = app.review.as_mut() {
        if view.tree_focused {
            view.activate_tree();
        }
        return Effect::None;
    }
    if let Some(number) = app.list.selected().map(|pr| pr.number) {
        Effect::OpenPullRequest(number)
    } else {
        app.notice(NoticeLevel::Warn, "there is nothing to open");
        Effect::None
    }
}

/// `Esc`: closes the review, or clears what is narrowing the list (FR-3.4).
///
/// A back press while something is loading cancels that instead: `Esc` means "stop
/// what you are doing" first, and "go back" when there is nothing to stop (NFR-1.4).
fn go_back(app: &mut App) -> Effect {
    if app.loading_something() {
        return Effect::CancelInFlight;
    }
    if app.review.is_some() {
        app.close_review();
        return Effect::None;
    }
    if app.list.is_filtered() {
        let was_loading = app.list.loading;
        app.list.clear_filters();
        app.list.stale = None;
        // Only re-ask GitHub if something it was asked for changed.
        return if was_loading || app.environment.is_none() {
            Effect::None
        } else {
            Effect::LoadPullRequests
        };
    }
    Effect::None
}

/// How far the cursor should move.
///
/// Three named distances rather than a `delta` and a boolean: the boolean version
/// conflated "half a screen" with "a whole one" and silently turned `<C-f>` into a
/// single row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Movement {
    /// A number of rows, signed.
    Rows(i32),
    /// Half a screen, signed.
    Half(i32),
    /// A whole screen, signed.
    Page(i32),
}

/// Moves whatever pane has the cursor.
fn move_current(app: &mut App, movement: Movement) {
    if let Some(view) = app.review.as_mut() {
        if view.tree_focused {
            // The tree is a list of files: a screen and a row mean the same thing to
            // it, and paging it is not worth a second notion of position.
            view.move_tree(movement.delta());
            return;
        }
        match movement {
            Movement::Rows(delta) => view.move_by(delta),
            Movement::Half(delta) => view.move_page(delta, true),
            Movement::Page(delta) => view.move_page(delta, false),
        }
        return;
    }
    match movement {
        Movement::Rows(delta) => app.list.move_cursor(delta),
        Movement::Half(delta) => app.list.move_page(delta, true),
        Movement::Page(delta) => app.list.move_page(delta, false),
    }
}

impl Movement {
    /// The signed distance, for the panes that treat every movement as rows.
    const fn delta(self) -> i32 {
        match self {
            Self::Rows(delta) | Self::Half(delta) | Self::Page(delta) => delta,
        }
    }
}

/// Jumps the cursor of whichever pane has it to the first or last row.
fn move_to_end(app: &mut App, last: bool) {
    if let Some(view) = app.review.as_mut() {
        if view.tree_focused {
            view.tree_cursor = if last {
                view.tree.len().saturating_sub(1)
            } else {
                0
            };
        } else {
            view.move_to(last);
        }
        return;
    }
    app.list.move_cursor_to(last);
}

/// Runs a closure against the open review view, if there is one.
fn with_diff(app: &mut App, action: impl FnOnce(&mut crate::tui::diff_view::DiffView)) {
    if let Some(view) = app.review.as_mut() {
        action(view);
    }
}

/// Turns the side-by-side view on or off, explaining when the terminal is too
/// narrow to honour it (DEC-4).
fn toggle_split(app: &mut App) -> Effect {
    let width = app.terminal_width();
    let Some(view) = app.review.as_mut() else {
        app.notice(NoticeLevel::Warn, "open a pull request first");
        return Effect::None;
    };
    view.split = !view.split;
    let split = view.split;

    if split && width > 0 && width < crate::tui::components::review::SPLIT_MIN_WIDTH {
        app.notice(
            NoticeLevel::Warn,
            format!(
                "side-by-side needs {} columns; this terminal has {width}, so the unified view is shown",
                crate::tui::components::review::SPLIT_MIN_WIDTH
            ),
        );
    } else if split {
        app.notice(
            NoticeLevel::Info,
            "side-by-side view on (it needs 140 columns)",
        );
    } else {
        app.notice(NoticeLevel::Info, "unified view on");
    }
    Effect::None
}

/// Explains that the context size needs the local workspace, without pretending to
/// have changed anything (FR-3.2).
///
/// A remote diff comes from `gh pr diff`, which always emits three lines of context
/// and never filters whitespace. Flipping the label would tell the user the pane is
/// showing something it is not, so in M1 these keys explain rather than lie; M2's
/// workspace re-diffs locally, where both settings are real.
/// Cycles the diff context: 3 → 10 → 0 (FR-3.2).
///
/// The change is a *re-read*, not a re-render: the context lines are produced by git,
/// so the diff has to be asked for again. That is why the toggle returns an effect
/// rather than redrawing, and why it is instant only once the worktree exists.
fn cycle_context(app: &mut App) -> Effect {
    if app.review.is_none() {
        app.notice(NoticeLevel::Warn, "open a pull request first");
        return Effect::None;
    }
    app.diff_options.context = match app.diff_options.context {
        3 => 10,
        10 => 0,
        _ => 3,
    };
    let context = app.diff_options.context;
    app.notice(NoticeLevel::Info, format!("diff context: {context} lines"));
    if !app.workspace_ready() {
        app.notice(
            NoticeLevel::Warn,
            "without the local workspace the diff comes from GitHub, which always uses 3 lines",
        );
        return Effect::None;
    }
    Effect::ReloadDiff
}

/// Explains that ignoring whitespace needs the local workspace (FR-3.2).
/// Toggles whitespace-ignoring diffs (FR-3.2).
fn toggle_whitespace(app: &mut App) -> Effect {
    if app.review.is_none() {
        app.notice(NoticeLevel::Warn, "open a pull request first");
        return Effect::None;
    }
    app.diff_options.ignore_whitespace = !app.diff_options.ignore_whitespace;
    let ignoring = app.diff_options.ignore_whitespace;
    app.notice(
        NoticeLevel::Info,
        if ignoring {
            "ignoring whitespace-only changes"
        } else {
            "showing whitespace-only changes"
        },
    );
    if !app.workspace_ready() {
        app.notice(
            NoticeLevel::Warn,
            "whitespace-ignoring diffs need the local workspace; it is being prepared",
        );
        return Effect::None;
    }
    Effect::ReloadDiff
}

/// Moves the focus on. Inside a review that means the tree and the diff in turn
/// (FR-3.3: the two panes keep independent cursors and `Tab` moves between them).
/// The three places `Tab` can stop inside a review screen.
///
/// A cycle of three rather than two `Pane`s, because the file tree and the diff text are
/// two stops inside *one* pane: modelling them as a pair of booleans alongside a separate
/// chat pane is what made the first version of this skip the chat pane entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// The file tree.
    Tree,
    /// The diff text.
    Diff,
    /// The conversation.
    Chat,
}

impl Stop {
    /// The next stop, which is the order the screen reads in.
    const fn next(self) -> Self {
        match self {
            Self::Tree => Self::Diff,
            Self::Diff => Self::Chat,
            Self::Chat => Self::Tree,
        }
    }

    /// The previous stop.
    const fn prev(self) -> Self {
        match self {
            Self::Tree => Self::Chat,
            Self::Diff => Self::Tree,
            Self::Chat => Self::Diff,
        }
    }
}

fn switch_pane(app: &mut App, forward: bool) -> Effect {
    if let Some(view) = app.review.as_mut() {
        let current = match (app.focus, view.tree_focused) {
            (crate::tui::app::Pane::Chat, _) => Stop::Chat,
            (_, true) => Stop::Tree,
            _ => Stop::Diff,
        };
        let stop = if forward {
            current.next()
        } else {
            current.prev()
        };
        return focus_stop(app, stop);
    }
    let pane = if forward {
        app.focus.next()
    } else {
        app.focus.prev()
    };
    if pane == crate::tui::app::Pane::Chat {
        // Outside a review screen there is nothing to talk about, so the cycle stays
        // between the list and the (empty) diff pane.
        return set_focus(app, crate::tui::app::Pane::PullRequests);
    }
    set_focus(app, pane)
}

/// Focuses one of the three review stops, opening the chat pane if that is where the
/// cycle landed (FR-5.2).
fn focus_stop(app: &mut App, stop: Stop) -> Effect {
    match stop {
        Stop::Tree => {
            if let Some(view) = app.review.as_mut() {
                view.tree_focused = true;
            }
            app.notice(NoticeLevel::Info, "file tree: Enter opens, j/k moves");
            set_focus(app, crate::tui::app::Pane::Diff)
        }
        Stop::Diff => {
            if let Some(view) = app.review.as_mut() {
                view.tree_focused = false;
            }
            set_focus(app, crate::tui::app::Pane::Diff)
        }
        Stop::Chat => {
            // A focus that lands on a pane which is not drawn would leave the keyboard
            // in a place with nothing to type into.
            let was_open = app.chat.open;
            app.show_chat();
            let effect = set_focus(app, crate::tui::app::Pane::Chat);
            if was_open || app.chat.session.is_some() {
                effect
            } else {
                Effect::LoadChat
            }
        }
    }
}

fn set_focus(app: &mut App, pane: crate::tui::app::Pane) -> Effect {
    app.focus = pane;
    app.state.focus = Some(pane.label().to_owned());
    // The chat compose box is a text-entry surface, so focusing it switches the app to
    // insert mode and leaving it switches back: the status line then says which of the
    // two the keyboard is doing (FR-7.1).
    app.sync_mode_to_focus();
    Effect::SaveState
}

/// The commands that are exactly one action, so the dispatcher below is only about
/// the ones that need an argument (FR-7.3).
const DIRECT: &[(&str, &str)] = &[
    ("q", "app.quit"),
    ("qa", "app.quit"),
    ("quit", "app.quit"),
    ("help", "app.help"),
    ("doctor", "app.doctor"),
    ("refresh", "app.refresh"),
    ("version", "app.version"),
    ("messages", "notice.clear"),
    ("load-more", "app.load_more"),
    ("clear-filters", "filter.clear"),
    ("copy-path", "review.copy_path"),
];

/// Runs a `:` command.
pub fn command(app: &mut App, input: &str) -> Effect {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Effect::None;
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default();
    let argument = parts.next().unwrap_or_default().trim();

    if let Some((_, action)) = DIRECT.iter().find(|(alias, _)| *alias == name) {
        return dispatch(app, action);
    }

    match name {
        "keymap" => keymap_command(app, argument),
        "pr" => open_pr(app, argument),
        "filter" => add_filter(app, argument),
        "filter-remove" => remove_filter(app, argument),
        "sort" => set_sort(app, argument),
        "model" => model_command(app, argument),
        "key" => key_command(app, argument),
        "catalog" => catalog_command(app, argument),
        "workspace" => workspace_command(app, argument),
        "draft" => draft_command(app, argument),
        "context" => context_command(app, argument),
        "edit" => edit_composer_command(app, argument),
        "chat" => chat_command(app, argument),
        "analyze" => start_analysis(app, argument),
        "plan" => plan_command(app, argument),
        "theme" => match argument {
            "" => dispatch(app, "app.theme_picker"),
            "reload" => app.reload_theme(),
            "next" => dispatch(app, "theme.toggle"),
            name => app.set_theme(name),
        },
        "set" => set_option(app, argument),
        other => unknown_command(app, other),
    }
}

/// Gives the open comment composer to `$EDITOR` (FR-6.2).
fn edit_composer_command(app: &mut App, argument: &str) -> Effect {
    if !argument.is_empty() {
        app.command_error("usage: :edit (while a comment composer is open)");
        return Effect::None;
    }
    let Some(composer) = app.drafts.composer.as_ref() else {
        app.notice(
            NoticeLevel::Warn,
            "$EDITOR edits comment composers, not this input",
        );
        return Effect::None;
    };
    Effect::EditComposer(composer.input.text().to_owned())
}

/// Reports a command that does not exist, suggesting the nearest one (FR-7.3).
fn unknown_command(app: &mut App, name: &str) -> Effect {
    let mut message = format!("`{name}` is not a command");
    if let Some(suggestion) = closest_command(name) {
        let _ = std::fmt::Write::write_fmt(
            &mut message,
            format_args!("; did you mean `{suggestion}`?"),
        );
    }
    app.command_error(message);
    Effect::None
}

/// `<leader>a` and `:analyze [--force|raw]` (FR-4.1, FR-4.4).
///
/// The sequence a user meets is: no model means the picker, nothing open means a
/// message, a second press confirms the first send for a repository, and after that
/// one press runs it (FR-4.6).
fn start_analysis(app: &mut App, argument: &str) -> Effect {
    match argument {
        "raw" => {
            if app.raw_answer().is_some() {
                app.open_overlay(Overlay::RawAnswer);
            } else if app.panel.analysis.is_some() {
                app.notice(
                    NoticeLevel::Info,
                    "the stored analysis came from a usable answer; its text is not kept"
                        .to_owned(),
                );
            } else {
                app.notice(
                    NoticeLevel::Warn,
                    "there is no answer to show; <leader>a runs one".to_owned(),
                );
            }
            return Effect::None;
        }
        "" | "--force" => {}
        other => {
            app.command_error(format!(
                "`:analyze {other}` is not an option; try `:analyze`, `:analyze --force` or \
                 `:analyze raw`"
            ));
            return Effect::None;
        }
    }

    if !app.has_model() {
        // FR-4.5: with no model the feature is inert with a call to action, not
        // broken.
        let problem = app
            .model_problem()
            .unwrap_or("no model is selected")
            .to_owned();
        app.notice(
            NoticeLevel::Info,
            format!("{problem}; <leader>m chooses one"),
        );
        return dispatch(app, "app.model_picker");
    }
    if app.detail.is_none() {
        app.notice(NoticeLevel::Warn, "open a pull request first".to_owned());
        return Effect::None;
    }
    if app.analysis_state().is_running() {
        // Already running: the panel is where the answer arrives, and `Esc` is the
        // key that stops it (FR-4.4). Re-running here would throw away the tokens
        // already paid for.
        app.open_overlay(Overlay::Analysis);
        return Effect::None;
    }

    let bundle = app.take_context_bundle_for_current_head();
    if let Some(bundle) = bundle {
        // The estimate has been shown and agreed to; send it (FR-4.6).
        if app.panel.confirmed || app.analysis_opt_in_recorded() {
            app.record_analysis_opt_in();
            app.begin_analysis();
            app.panel.bundle = Some(Box::new(bundle));
            return Effect::RunAnalysis {
                force: argument == "--force",
            };
        }
    } else if app.panel.confirmed && app.analysis_opt_in_recorded() {
        // The confirmation was for a bundle that is no longer current: gather again.
        return Effect::GatherContext(AnalysisIntent::Estimate);
    }
    Effect::GatherContext(AnalysisIntent::Estimate)
}

/// `J`/`K`: moves the selected review-plan group (FR-4.2).
fn move_plan_group(app: &mut App, down: bool) -> Effect {
    let Some(group) = app.selected_plan_group() else {
        app.notice(
            NoticeLevel::Info,
            "put the cursor on a group heading in the Files pane, then J or K moves it".to_owned(),
        );
        return Effect::None;
    };
    let Some(plan) = app.plan_mut() else {
        return Effect::None;
    };
    let delta = if down { 1 } else { -1 };
    if plan.move_group(&group, delta) {
        let updated = plan.clone();
        app.after_plan_change();
        return Effect::SavePlan(Box::new(updated));
    }
    app.notice(
        NoticeLevel::Info,
        format!(
            "`{group}` is already as far {} as it goes",
            if down { "down" } else { "up" }
        ),
    );
    Effect::None
}

/// `:plan reset|path|recommended|move <file> <group>` (FR-4.2).
fn plan_command(app: &mut App, argument: &str) -> Effect {
    let mut parts = argument.split_whitespace();
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (None, ..) => {
            app.notice(NoticeLevel::Info, app.plan_summary());
            Effect::None
        }
        (Some("reset"), None, _, _) => reset_plan(app),
        (Some("path"), None, _, _) => {
            if let Some(view) = app.review_mut() {
                view.set_order(crate::domain::plan::OrderMode::Path);
            }
            app.notice(NoticeLevel::Info, "path order".to_owned());
            Effect::None
        }
        (Some("recommended"), None, _, _) => {
            if app.plan().is_none() {
                app.notice(
                    NoticeLevel::Warn,
                    "there is no plan yet; <leader>a analyses the pull request".to_owned(),
                );
                return Effect::None;
            }
            if let Some(view) = app.review_mut() {
                view.set_order(crate::domain::plan::OrderMode::Recommended);
            }
            app.notice(NoticeLevel::Info, "recommended order".to_owned());
            Effect::None
        }
        (Some("move"), Some(file), Some(group), None) => {
            let Some(plan) = app.plan_mut() else {
                app.command_error("there is no plan to change; <leader>a analyses first");
                return Effect::None;
            };
            if !plan.pin_file(file, group) {
                app.command_error(format!("`{file}` is already in `{group}`"));
                return Effect::None;
            }
            let updated = plan.clone();
            app.after_plan_change();
            Effect::SavePlan(Box::new(updated))
        }
        (Some(other), ..) => {
            app.command_error(format!(
                "`:plan {other}` is not an option; try `:plan`, `:plan reset`, `:plan path`, \
                 `:plan recommended` or `:plan move <file> <group>`"
            ));
            Effect::None
        }
    }
}

/// `:plan reset`: back to the order the analysis asked for (FR-4.2).
fn reset_plan(app: &mut App) -> Effect {
    let Some(stored) = app.panel.analysis.as_deref() else {
        app.notice(
            NoticeLevel::Warn,
            "there is no analysed plan to reset to".to_owned(),
        );
        return Effect::None;
    };
    let plan = crate::domain::plan::Plan::from_analysis(&stored.analysis);
    app.set_plan(plan.clone());
    app.notice(
        NoticeLevel::Info,
        "the review order is the analysis's again".to_owned(),
    );
    Effect::SavePlan(Box::new(plan))
}

/// `:model`, `:model show`, `:model pick` (FR-4.5).
fn model_command(app: &mut App, argument: &str) -> Effect {
    match argument {
        "" | "pick" => dispatch(app, "app.model_picker"),
        "show" => {
            if let Some(resolved) = app.active_model() {
                let source = if resolved.from_file {
                    "credentials.toml".to_owned()
                } else {
                    resolved
                        .env_source
                        .clone()
                        .unwrap_or_else(|| "environment".to_owned())
                };
                app.notice(
                    NoticeLevel::Info,
                    format!(
                        "{} · {} · base {} · key from {} · thinking {} · input effective {}/configured {}/window {} · output effective {}/configured {}/catalog {} · temperature effective {}/configured {}",
                        resolved.label(),
                        resolved.route_label(),
                        resolved.base_url.as_deref().unwrap_or("provider default"),
                        source,
                        resolved.thinking_label(),
                        resolved.settings.input_tokens,
                        resolved.settings.configured_input_tokens,
                        resolved.settings.model_window.map_or_else(|| "unknown".to_owned(), |tokens| tokens.to_string()),
                        resolved.settings.max_tokens.map_or_else(|| "provider default".to_owned(), |tokens| tokens.to_string()),
                        resolved.settings.configured_max_tokens.map_or_else(|| "provider default".to_owned(), |tokens| tokens.to_string()),
                        resolved.settings.catalog_output_tokens.map_or_else(|| "unknown".to_owned(), |tokens| tokens.to_string()),
                        resolved.settings.temperature.map_or_else(|| "provider default".to_owned(), |value| value.to_string()),
                        resolved.settings.configured_temperature.map_or_else(|| "provider default".to_owned(), |value| value.to_string())
                    ),
                );
            } else {
                let problem = app
                    .model_problem()
                    .unwrap_or("no model is configured")
                    .to_owned();
                app.notice(NoticeLevel::Warn, format!("{problem}; press <leader>m"));
            }
            Effect::None
        }
        other => {
            app.command_error(format!(
                "`:model {other}` is not an option; try `:model`, `:model show`"
            ));
            Effect::None
        }
    }
}

/// `:key clear <provider>` (NFR-3.1).
fn key_command(app: &mut App, argument: &str) -> Effect {
    let mut parts = argument.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some("clear"), Some(provider), None) => Effect::ClearKey(provider.to_owned()),
        // No argument lists what is stored. A command that only works with an
        // argument is a command you have to remember instead of one you can explore.
        (None, _, _) => {
            match app.secret_store().status() {
                Ok(status) if status.is_empty() => app.notice(
                    NoticeLevel::Info,
                    "no keys are stored; <leader>m stores one in credentials.toml",
                ),
                Ok(status) => {
                    let providers: Vec<String> = status
                        .iter()
                        .map(|entry| {
                            let source = entry
                                .source
                                .as_ref()
                                .map_or_else(|| "?".to_owned(), crate::ports::KeySource::label);
                            format!("{} ({source})", entry.provider)
                        })
                        .collect();
                    app.notice(
                        NoticeLevel::Info,
                        format!("stored keys: {}", providers.join(", ")),
                    );
                }
                Err(error) => app.notice(NoticeLevel::Warn, format!("keys: {error}")),
            }
            Effect::None
        }
        _ => {
            app.command_error("usage: `:key` or `:key clear <provider>`");
            Effect::None
        }
    }
}

/// `:catalog refresh` (FR-4.7).
fn catalog_command(app: &mut App, argument: &str) -> Effect {
    match argument {
        "refresh" => Effect::LoadCatalog(crate::ports::catalog::CatalogPolicy::Refresh),
        "" => Effect::LoadCatalog(crate::ports::catalog::CatalogPolicy::CacheFirst),
        other => {
            app.command_error(format!(
                "`:catalog {other}` is not an option; try `:catalog refresh`"
            ));
            Effect::None
        }
    }
}

/// `:workspace clean [--all]` (FR-3.1).
fn workspace_command(app: &mut App, argument: &str) -> Effect {
    match argument {
        "clean" => app.ask_clean_workspaces(false),
        "clean --all" | "clean all" => app.ask_clean_workspaces(true),
        // With no argument, say what the worktrees are and what the options do. The
        // listing happens in the loop: the reducer does no file system work.
        "" => Effect::ListWorktrees,
        other => {
            app.command_error(format!(
                "`:workspace {other}` is not an option; try `:workspace clean [--all]`"
            ));
            Effect::None
        }
    }
}

/// `:draft` — the staged review (FR-6.1–FR-6.3).
fn draft_command(app: &mut App, argument: &str) -> Effect {
    let argument = argument.trim();
    let (word, rest) = match argument.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (argument, ""),
    };
    match word {
        // With no argument, show what is staged: the panel is where the comments are
        // read, and a list of them in the status line would not fit (FR-6.1).
        "" | "show" => app.open_drafts(),
        "list" => app.list_drafts(),
        "remove" | "rm" => {
            let Ok(number) = rest.parse::<usize>() else {
                app.command_error(format!("`:draft remove {rest}` needs a number"));
                return Effect::None;
            };
            app.remove_draft_number(number)
        }
        "clear" => app.ask_clear_draft(),
        "decision" => app.set_draft_decision(rest),
        "body" => app.set_draft_body(rest),
        "publish" | "push" => app.open_publish(),
        "export" => app.export_draft(rest),
        "path" => {
            app.notice(NoticeLevel::Info, app.draft_path_label());
            Effect::None
        }
        other => {
            app.command_error(format!(
                "`:draft {other}` is not an option; try `:draft [list|remove <n>|clear|\
                 decision <d>|body <text>|export [md|json]]`"
            ));
            Effect::None
        }
    }
}

/// `:context <n>` (FR-3.2).
/// `:chat` — the conversation commands (FR-5.1, FR-5.2).
///
/// Every branch prints something rather than staying quiet, because a command that
/// appears to do nothing is worse than one that refuses: the palette test requires each
/// listed command to work with no argument, and "what can I do with `:chat`" is the
/// question a bare `:chat` should answer.
fn chat_command(app: &mut App, argument: &str) -> Effect {
    let mut words = argument.split_whitespace();
    let subcommand = words.next().unwrap_or_default();
    let rest = words.collect::<Vec<_>>().join(" ");
    match subcommand {
        "" | "show" => {
            app.show_chat();
            Effect::None
        }
        "new" => {
            app.notice(
                NoticeLevel::Info,
                "starting a new conversation; the old ones stay in `:chat list`".to_owned(),
            );
            Effect::NewChat
        }
        "list" => {
            app.list_chats();
            Effect::None
        }
        "open" => {
            if rest.is_empty() {
                app.list_chats();
                return Effect::None;
            }
            Effect::OpenChat(rest)
        }
        "export" => Effect::ExportChat(if rest.is_empty() {
            "md".to_owned()
        } else {
            rest
        }),
        "retry" => Effect::RetryChat,
        other => {
            app.command_error(format!(
                "{other} is not a chat command; use new, list, open <id>, export [md|json] or retry"
            ));
            Effect::None
        }
    }
}

/// `:context` — the inspector, the added files, or the diff's context lines (FR-4.6).
fn context_command(app: &mut App, argument: &str) -> Effect {
    let mut words = argument.split_whitespace();
    match (words.next(), words.next()) {
        (Some("add"), Some(path)) => {
            match app.add_context_file(path) {
                Ok(message) => {
                    app.notice(NoticeLevel::Info, message);
                    return Effect::SaveContextFiles;
                }
                Err(message) => app.command_error(message),
            }
            return Effect::None;
        }
        (Some("add"), None) => {
            app.command_error("which file? `:context add src/domain/money.rs`");
            return Effect::None;
        }
        (Some("remove"), Some(path)) => {
            match app.remove_context_file(path) {
                Ok(message) => {
                    app.notice(NoticeLevel::Info, message);
                    return Effect::SaveContextFiles;
                }
                Err(message) => app.command_error(message),
            }
            return Effect::None;
        }
        (Some("remove"), None) => {
            app.command_error("which file? `:context remove src/domain/money.rs`");
            return Effect::None;
        }
        _ => {}
    }
    if argument.is_empty() {
        // FR-4.6's inspector: what an analysis would send. A number still means the
        // diff's own context, which is the M1 meaning of the same word (FR-3.2) — and
        // the difference is stated rather than guessed at, because `:context 6` and
        // `:context` doing two different things is worth one sentence.
        if app.context_bundle().is_some() {
            app.open_overlay(Overlay::Context);
            return Effect::None;
        }
        app.notice(
            NoticeLevel::Info,
            format!(
                "gathering what an analysis would send ({} diff context lines; `:context <0-1000>` \
                 changes that)",
                app.diff_options.context
            ),
        );
        return Effect::GatherContext(AnalysisIntent::Inspect);
    }
    match argument.parse::<u32>() {
        Ok(lines) if lines <= 1000 => {
            app.diff_options.context = lines;
            app.notice(NoticeLevel::Info, format!("diff context: {lines} lines"));
            if app.workspace_ready() {
                Effect::ReloadDiff
            } else {
                app.notice(
                    NoticeLevel::Warn,
                    "the diff comes from GitHub until the workspace is ready, so the context is still 3",
                );
                Effect::None
            }
        }
        _ => {
            app.command_error("usage: `:context <0-1000>`");
            Effect::None
        }
    }
}

/// `:keymap` and `:keymap <action>` (FR-7.2).
fn keymap_command(app: &mut App, argument: &str) -> Effect {
    if argument.is_empty() {
        return dispatch(app, "app.help");
    }

    if let Some(definition) = action::find(argument) {
        app.open_overlay(Overlay::Help);
        // `open_overlay` resets the filter, so set it afterwards.
        app.help_filter = Some(definition.id.to_owned());
        return Effect::None;
    }

    let mut message = format!("`{argument}` is not an action");
    if let Some(suggestion) = action::suggest(argument) {
        let _ = std::fmt::Write::write_fmt(
            &mut message,
            format_args!("; did you mean `{}`?", suggestion.id),
        );
    }
    app.command_error(message);
    Effect::None
}

/// Applies `:set <key>=<value>` for the options that can change at runtime.
fn set_option(app: &mut App, spec: &str) -> Effect {
    let Some((key, value)) = spec.split_once('=') else {
        app.command_error("usage: :set <key>=<value>, for example :set ui.timeoutlen=250");
        return Effect::None;
    };
    let key = key.trim();
    let value = value.trim();

    match key {
        "ui.theme" => app.set_theme(value),
        "mouse" | "ui.mouse" => {
            match value {
                "true" | "on" | "yes" => {
                    app.show_mouse(true);
                    app.notice(NoticeLevel::Info, "mouse capture on");
                }
                "false" | "off" | "no" => {
                    app.show_mouse(false);
                    app.notice(NoticeLevel::Info, "mouse capture off");
                }
                other => {
                    app.command_error(format!("`{other}` is not a boolean; use true or false"));
                    return Effect::None;
                }
            }
            // The loop owns the terminal, so the change is applied there.
            Effect::SetMouse(app.mouse_enabled())
        }
        "ui.timeoutlen" => {
            if let Ok(milliseconds) = value.parse::<u64>() {
                app.keymap.set_timeout(Duration::from_millis(milliseconds));
                app.notice(NoticeLevel::Info, format!("timeoutlen = {milliseconds} ms"));
                return Effect::None;
            }
            app.command_error(format!("`{value}` is not a number of milliseconds"));
            Effect::None
        }
        "ui.leader" => match keymap::parse_keys(value, &KeyCombo::char(' ')) {
            Ok(keys) if keys.len() == 1 => {
                value.clone_into(&mut app.config.ui.leader);
                app.reload_keymap();
                app.notice(
                    NoticeLevel::Info,
                    format!("leader = {}", keys[0].describe()),
                );
                Effect::None
            }
            Ok(_) => {
                app.command_error(format!(
                    "`{value}` must be a single key, for example <Space> or ,"
                ));
                Effect::None
            }
            Err(error) => {
                app.command_error(error.to_string());
                Effect::None
            }
        },
        _ => {
            app.command_error(format!(
                "`{key}` cannot be changed at runtime yet; edit {}",
                app.config_path.display()
            ));
            Effect::None
        }
    }
}

/// Commands matching what has been typed so far, best match first, with the
/// description to display.
///
/// Matching is fuzzy: the typed characters must appear in order, and contiguous
/// or earlier matches rank higher (FR-7.3).
#[must_use]
pub fn candidates(input: &str) -> Vec<(&'static str, &'static str)> {
    let typed = input.trim();

    let mut scored: Vec<(i32, &'static str, &'static str)> = COMMANDS
        .iter()
        .filter_map(|(name, _action, description)| {
            let score = if typed.is_empty() {
                0
            } else {
                crate::fuzzy::score(typed, name)?
            };
            Some((score, *name, *description))
        })
        .collect();

    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(right.1)));
    scored
        .into_iter()
        .map(|(_, name, description)| (name, description))
        .collect()
}

/// Completes a partially typed command name.
#[must_use]
pub fn complete_command(input: &str) -> String {
    let typed = input.trim_start();
    if typed.contains(char::is_whitespace) {
        return input.to_owned();
    }

    let names: Vec<&str> = candidates(typed)
        .into_iter()
        .map(|(name, _)| name)
        .collect();

    match names.as_slice() {
        [] => input.to_owned(),
        [single] => (*single).to_owned(),
        several => {
            // Never hand back something shorter than what was typed: the shared
            // prefix of `help` and `theme` is empty, and completing to it would
            // wipe the buffer (FR-7.4).
            let prefix = common_prefix(several);
            if prefix.chars().count() > typed.chars().count() {
                prefix
            } else {
                input.to_owned()
            }
        }
    }
}

fn common_prefix(names: &[&str]) -> String {
    let Some(first) = names.first() else {
        return String::new();
    };
    // Counted in characters, not bytes: the lengths of two names are only comparable
    // per character, and slicing a `&str` inside one would panic.
    let mut length = first.chars().count();
    for name in names.iter().skip(1) {
        length = length.min(name.chars().count());
        while length > 0 {
            let candidate: String = first.chars().take(length).collect();
            if name.starts_with(&candidate) {
                break;
            }
            length -= 1;
        }
    }
    first.chars().take(length).collect()
}

fn closest_command(typed: &str) -> Option<&'static str> {
    COMMANDS
        .iter()
        .map(|(name, _, _)| (action::levenshtein(typed, name), *name))
        .filter(|(distance, _)| *distance <= 3)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An app with a fixed, short home, for tests that only care about state.
    fn test_app() -> (crate::test_support::TempHome, App) {
        let dir = crate::test_support::temp_home();
        let cli = crate::cli::Cli {
            repo: None,
            pr: None,
            path: None,
            remote: None,
            config: None,
            theme: None,
            home: Some(dir.path().to_path_buf()),
            log_level: None,
            check: false,
            dry_run: false,
        };
        let startup = crate::Startup::load(&cli).unwrap();
        (dir, App::new(startup).unwrap())
    }

    #[test]
    fn every_command_maps_to_a_registered_action() {
        for (name, id, description) in COMMANDS {
            assert!(
                action::is_known(id),
                "command `{name}` maps to unknown action `{id}`"
            );
            assert!(!description.is_empty(), "`{name}` needs a description");
        }
    }

    #[test]
    fn completion_fills_in_a_unique_command() {
        assert_eq!(complete_command("doc"), "doctor");
        assert_eq!(complete_command(""), "");
        assert_eq!(complete_command("bogus"), "bogus");
    }

    #[test]
    fn completion_uses_the_common_prefix_for_ambiguous_input() {
        // "q", "qa" and "quit" all match, so only the shared prefix is filled in.
        assert_eq!(complete_command("q"), "q");
        assert_eq!(common_prefix(&["theme", "the"]), "the");
    }

    #[test]
    fn completion_stops_at_arguments() {
        assert_eq!(complete_command("theme li"), "theme li");
    }

    #[test]
    fn completion_is_fuzzy() {
        // Not a prefix, but a subsequence: t-h-m-e.
        assert_eq!(complete_command("thm"), "theme");
        assert_eq!(complete_command("msg"), "messages");
    }

    #[test]
    fn fuzzy_scoring_prefers_contiguous_and_shorter_matches() {
        let contiguous = crate::fuzzy::score("doc", "doctor").unwrap();
        let scattered = crate::fuzzy::score("doc", "d-o-c-nonsense").unwrap();
        assert!(contiguous > scattered, "{contiguous} vs {scattered}");
        assert!(crate::fuzzy::score("z", "doctor").is_none());
    }

    #[test]
    fn candidates_are_ranked_and_bounded() {
        let all = candidates("");
        assert_eq!(all.len(), COMMANDS.len());
        let typed = candidates("thm");
        assert_eq!(typed.first().map(|(name, _)| *name), Some("theme"));
    }

    #[test]
    fn tab_never_deletes_what_was_typed() {
        // `help`, `theme` and `refresh` all match `h`, and their shared prefix is
        // empty; completing to it would clear the buffer.
        for typed in ["h", "he", "r", "se"] {
            let completed = complete_command(typed);
            assert!(
                completed.starts_with(typed),
                "completing `{typed}` produced `{completed}`"
            );
        }
    }

    #[test]
    fn near_miss_commands_are_suggested() {
        assert_eq!(closest_command("docotr"), Some("doctor"));
        assert_eq!(closest_command("zzzzzz"), None);
    }

    #[test]
    fn every_listed_command_is_actually_handled() {
        // The palette comes from `COMMANDS`, so a name listed there but missing from
        // `command` would be offered to the user and then refused — which is exactly
        // what happened to `:filter` before this test existed.
        for (name, _, _) in super::COMMANDS {
            let (_dir, mut app) = test_app();
            // Commands that take an argument are given a valid one: the point is
            // that the name is handled, not that it needs no argument.
            let argument = match *name {
                "filter" => " author:alice",
                "sort" => " updated desc",
                "pr" => " 141",
                "set" => " ui.timeoutlen=250",
                _ => "",
            };
            if *name == "edit" {
                app.drafts.compose_conversation();
            }
            super::command(&mut app, &format!("{name}{argument}"));
            let error = app.command_error_text();
            assert!(
                error.is_none(),
                "`:{name}` is listed in the palette but refused: {error:?}",
            );
        }
    }

    #[test]
    fn filter_and_sort_arguments_are_validated_before_anything_is_sent() {
        let (_dir, mut app) = test_app();

        // A bad qualifier is refused with the name of the problem.
        let effect = super::command(&mut app, "filter nonsense:x");
        assert_eq!(effect, Effect::None);
        assert!(
            app.command_error_text()
                .is_some_and(|text| text.contains("nonsense")),
            "{:?}",
            app.command_error_text()
        );

        // A good one re-asks GitHub.
        let effect = super::command(&mut app, "filter author:alice");
        assert_eq!(effect, Effect::LoadPullRequests);
        assert_eq!(app.list.chips(), vec!["is:open", "author:alice"]);

        // Sorting validates both halves.
        assert_eq!(
            super::command(&mut app, "sort updated asc"),
            Effect::LoadPullRequests
        );
        assert_eq!(app.list.sort, crate::domain::query::PrSort::UpdatedAsc);
        assert_eq!(super::command(&mut app, "sort nonsense"), Effect::None);
        assert!(app.command_error_text().is_some());
        assert_eq!(
            super::command(&mut app, "sort created sideways"),
            Effect::None
        );
        assert!(
            app.command_error_text()
                .is_some_and(|text| text.contains("sideways")),
            "{:?}",
            app.command_error_text()
        );
    }

    #[test]
    fn copy_path_offers_the_file_under_the_cursor() {
        use crate::tui::diff_view::DiffView;

        let (_dir, mut app) = test_app();
        // Without a review there is nothing to copy, and the user is told so.
        assert_eq!(super::command(&mut app, "copy-path"), Effect::None);
        assert!(
            app.latest_notice()
                .is_some_and(|notice| notice.text.contains("no file")),
            "a warning should say why nothing was copied"
        );

        let patch = crate::domain::diff::parse_patch(
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-a\n+b\n",
        );
        app.set_review(DiffView::new(patch));
        assert_eq!(
            super::command(&mut app, "copy-path"),
            Effect::CopyPath("src/a.rs".to_owned())
        );
    }

    #[test]
    fn pr_opens_the_number_it_is_given() {
        let (_dir, mut app) = test_app();
        assert_eq!(
            super::command(&mut app, "pr 141"),
            Effect::OpenPullRequest(141)
        );
        assert_eq!(
            super::command(&mut app, "pr #141"),
            Effect::OpenPullRequest(141),
            "a leading hash is how a user writes it"
        );
        assert_eq!(super::command(&mut app, "pr"), Effect::None);
        assert!(app.command_error_text().is_some());
    }

    /// An app with a pull request open, a repository and a model: everything the
    /// analysis path needs (FR-4.1).
    fn app_ready_to_analyse() -> (crate::test_support::TempHome, App) {
        let (dir, mut app) = test_app();
        app.set_environment(crate::test_support::environment());
        app.open_review(
            crate::test_support::analysis_detail(),
            crate::tui::diff_view::DiffView::new(crate::test_support::analysis_patch()),
        );
        app.active_model = Some(crate::test_support::resolved_model());
        (dir, app)
    }

    #[test]
    fn analysing_without_a_model_opens_the_picker_and_says_why() {
        let (_dir, mut app) = test_app();
        app.open_review(
            crate::test_support::analysis_detail(),
            crate::tui::diff_view::DiffView::new(crate::test_support::analysis_patch()),
        );
        // FR-4.5: the feature is inert with a call to action, never broken. What the
        // effect asks for is the catalog the picker needs; what matters here is that
        // the picker opened and the reason was said.
        let _ = dispatch(&mut app, "app.analyze_panel");
        assert!(app.picker().is_some(), "the picker opens");
        assert!(
            app.latest_notice()
                .is_some_and(|notice| notice.text.contains("no model")),
            "{:?}",
            app.latest_notice()
        );
    }

    #[test]
    fn the_first_press_gathers_the_context_and_the_second_one_confirms_it() {
        let (_dir, mut app) = app_ready_to_analyse();
        // FR-4.6: before anything is sent, the user is told what would be and asked.
        let effect = dispatch(&mut app, "app.analyze_panel");
        assert!(
            matches!(effect, Effect::GatherContext(AnalysisIntent::Estimate)),
            "{effect:?}"
        );

        // The gather arrives: the notice names the size, and nothing has been sent.
        let bundle = crate::domain::context::build(
            &crate::domain::context::BundleInputs {
                metadata: "PR #141",
                commits: "abc",
                ..crate::domain::context::BundleInputs::default()
            },
            &crate::domain::context::BundlePolicy::default(),
        );
        let effect = app.apply_context(bundle, AnalysisIntent::Estimate);
        assert!(effect.is_none(), "nothing is sent on the first press");
        assert!(app.panel.confirmed);
        let notice = app.latest_notice().expect("a notice").text.clone();
        assert!(notice.contains("press <leader>a again"), "{notice}");
        assert!(notice.contains("tokens"), "{notice}");

        // The second press sends it.
        let effect = dispatch(&mut app, "app.analyze_panel");
        assert!(
            matches!(effect, Effect::RunAnalysis { force: false }),
            "{effect:?}"
        );
        assert!(app.analysis_opt_in_recorded(), "the opt-in is recorded");
    }

    #[test]
    fn once_the_repository_has_agreed_the_next_analysis_needs_one_press() {
        let (_dir, mut app) = app_ready_to_analyse();
        app.record_analysis_opt_in();
        app.panel.bundle = Some(Box::new(crate::domain::context::build(
            &crate::domain::context::BundleInputs {
                metadata: "PR #141",
                ..crate::domain::context::BundleInputs::default()
            },
            &crate::domain::context::BundlePolicy::default(),
        )));
        app.panel.bundle_for = Some((141, "abc123".to_owned()));
        let effect = dispatch(&mut app, "app.analyze_panel");
        assert!(
            matches!(effect, Effect::RunAnalysis { force: false }),
            "{effect:?}"
        );
    }

    #[test]
    fn a_bundle_gathered_for_another_commit_is_not_sent() {
        let (_dir, mut app) = app_ready_to_analyse();
        app.panel.bundle = Some(Box::new(crate::domain::context::build(
            &crate::domain::context::BundleInputs {
                metadata: "PR #141",
                ..crate::domain::context::BundleInputs::default()
            },
            &crate::domain::context::BundlePolicy::default(),
        )));
        // The head moved since the bundle was gathered (FR-4.3).
        app.panel.bundle_for = Some((141, "anedotheraaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned()));
        let effect = dispatch(&mut app, "app.analyze_panel");
        assert!(
            matches!(effect, Effect::GatherContext(AnalysisIntent::Estimate)),
            "{effect:?}"
        );
        assert!(app.panel.bundle.is_none(), "the stale bundle was dropped");
    }

    #[test]
    fn the_key_watches_a_run_rather_than_restarting_it() {
        let (_dir, mut app) = app_ready_to_analyse();
        app.panel.state = crate::tui::app::AnalysisState::Streaming {
            stage: "asking".to_owned(),
        };
        let effect = dispatch(&mut app, "app.analyze_panel");
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert_eq!(app.overlay(), Overlay::Analysis);
        // And `Esc` is what stops it (FR-4.4).
        assert!(matches!(dispatch(&mut app, "app.cancel"), Effect::None));
        assert!(matches!(
            dispatch(&mut app, "app.cancel"),
            Effect::CancelAnalysis
        ));
    }

    #[test]
    fn escaped_cancels_a_run_after_closing_what_is_open() {
        let (_dir, mut app) = app_ready_to_analyse();
        app.panel.state = crate::tui::app::AnalysisState::Streaming {
            stage: "asking".to_owned(),
        };
        // A popup is closed first, so one press does not do two things.
        app.open_overlay(Overlay::Analysis);
        assert!(matches!(dispatch(&mut app, "app.cancel"), Effect::None));
        assert_eq!(app.overlay(), Overlay::None);
        assert!(matches!(
            dispatch(&mut app, "app.cancel"),
            Effect::CancelAnalysis
        ));
    }

    #[test]
    fn a_stale_stream_is_dropped_by_job_id() {
        let (_dir, mut app) = app_ready_to_analyse();
        app.record_analysis_job(7);
        app.apply_progress(crate::tui::jobs::Progress {
            job: 6,
            update: crate::tui::jobs::ProgressUpdate::Analysis(
                crate::application::analysis::Progress::Delta("old".to_owned()),
            ),
        });
        assert!(
            app.analysis_stream().is_empty(),
            "a superseded run is dropped"
        );
        app.apply_progress(crate::tui::jobs::Progress {
            job: 7,
            update: crate::tui::jobs::ProgressUpdate::Analysis(
                crate::application::analysis::Progress::Delta("new".to_owned()),
            ),
        });
        assert_eq!(app.analysis_stream(), "new");
        assert!(matches!(
            app.analysis_state(),
            crate::tui::app::AnalysisState::Streaming { .. }
        ));
    }

    #[test]
    fn ir_02_a_repair_preview_replaces_the_rejected_attempt() {
        let (_dir, mut app) = app_ready_to_analyse();
        app.record_analysis_job(7);
        for update in [
            crate::application::analysis::Progress::Delta("REJECTED_SENTINEL".to_owned()),
            crate::application::analysis::Progress::Reset("repairing".to_owned()),
            crate::application::analysis::Progress::Delta("REPAIRED_SENTINEL".to_owned()),
        ] {
            app.apply_progress(crate::tui::jobs::Progress {
                job: 7,
                update: crate::tui::jobs::ProgressUpdate::Analysis(update),
            });
        }
        assert_eq!(app.analysis_stream(), "REPAIRED_SENTINEL");

        app.apply_analysis(crate::application::analysis::AnalysisRun::Ready(Box::new(
            crate::application::analysis::Analyzed {
                analysis: Box::new(crate::test_support::stored_analysis("abc123").analysis),
                warnings: Vec::new(),
                repaired: true,
                usage: None,
            },
        )));
        assert_eq!(app.stored_analysis_raw(), Some("REPAIRED_SENTINEL"));
    }

    #[test]
    fn a_ready_analysis_orders_the_review_and_fills_the_panel() {
        let (_dir, mut app) = app_ready_to_analyse();
        app.record_analysis_job(3);
        app.apply_analysis(crate::application::analysis::AnalysisRun::Ready(Box::new(
            crate::application::analysis::Analyzed {
                analysis: Box::new(crate::test_support::stored_analysis("abc123").analysis),
                warnings: vec!["one file was unclassified".to_owned()],
                repaired: false,
                usage: None,
            },
        )));

        // FR-4.2: the tree is grouped by the plan, in the recommended order.
        let view = app.review.as_ref().expect("the review is open");
        assert_eq!(view.order, crate::domain::plan::OrderMode::Recommended);
        let groups: Vec<&str> = view
            .tree
            .iter()
            .filter_map(|row| match &row.kind {
                crate::tui::diff_view::TreeKind::Group { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(groups, ["domain", "tests"]);
        // The panel has the document, and the corrections are kept.
        let panel = app.analysis_panel().expect("a panel");
        assert_eq!(panel.summary, "Billing rounds half up.");
        assert_eq!(panel.risks.len(), 1);
        assert_eq!(app.analysis_warnings(), ["one file was unclassified"]);
        assert_eq!(app.analysis_state(), &crate::tui::app::AnalysisState::Ready);
    }

    #[test]
    fn an_unusable_answer_keeps_its_text_and_says_why() {
        let (_dir, mut app) = app_ready_to_analyse();
        app.record_analysis_job(3);
        app.apply_analysis(crate::application::analysis::AnalysisRun::Unparsed(
            Box::new(crate::application::analysis::Unparsed {
                raw: "I could not do that.".to_owned(),
                reason: "the answer contained no JSON object".to_owned(),
                repaired: true,
                usage: None,
            }),
        ));
        // FR-4.1: the text is kept and shown, never silently dropped.
        let (reason, raw) = app.raw_answer().expect("the raw text");
        assert!(reason.contains("no JSON object"), "{reason}");
        assert_eq!(raw, "I could not do that.");
        assert!(matches!(
            app.analysis_state(),
            crate::tui::app::AnalysisState::Unusable { .. }
        ));
        assert_eq!(app.overlay(), Overlay::RawAnswer);
    }

    #[test]
    fn a_cancelled_run_says_so_without_pretending_it_failed() {
        let (_dir, mut app) = app_ready_to_analyse();
        app.record_analysis_job(3);
        app.apply_analysis(crate::application::analysis::AnalysisRun::Cancelled);
        assert_eq!(
            app.analysis_state(),
            &crate::tui::app::AnalysisState::Cancelled
        );
        assert!(app.analysis_panel().is_none());
    }

    #[test]
    fn the_order_toggles_between_the_plan_and_the_paths() {
        let (_dir, mut app) = app_ready_to_analyse();
        app.set_plan(crate::domain::plan::Plan::from_analysis(
            &crate::test_support::stored_analysis("abc123").analysis,
        ));
        assert_eq!(
            app.review.as_ref().expect("open").order,
            crate::domain::plan::OrderMode::Recommended
        );
        assert!(matches!(
            dispatch(&mut app, "diff.toggle_order"),
            Effect::None
        ));
        let view = app.review.as_ref().expect("open");
        assert_eq!(view.order, crate::domain::plan::OrderMode::Path);
        // The notice names both positions, which is what makes the toggle comparable.
        let notice = app.latest_notice().expect("a notice").text.clone();
        assert!(notice.contains("path order"), "{notice}");
        assert!(notice.contains("plan ·"), "{notice}");
    }

    #[test]
    fn moving_a_group_with_j_and_k_persists_the_order() {
        let (_dir, mut app) = app_ready_to_analyse();
        app.set_plan(crate::domain::plan::Plan::from_analysis(
            &crate::test_support::stored_analysis("abc123").analysis,
        ));
        // The cursor has to be on a group heading, which is where the user's is when
        // they press the key.
        {
            let view = app.review.as_mut().expect("open");
            view.tree_focused = true;
            view.tree_cursor = 0;
        }
        assert_eq!(app.selected_plan_group().as_deref(), Some("domain"));
        let effect = dispatch(&mut app, "plan.move_down");
        let Effect::SavePlan(plan) = effect else {
            panic!("expected the order to be saved, got {effect:?}");
        };
        assert!(plan.overridden);
        assert_eq!(plan.groups[0].group, "tests", "the groups swapped");
        assert_eq!(app.plan().expect("a plan").groups[0].group, "tests");
    }

    #[test]
    fn moving_a_group_without_one_selected_says_what_to_do() {
        let (_dir, mut app) = app_ready_to_analyse();
        app.set_plan(crate::domain::plan::Plan::from_analysis(
            &crate::test_support::stored_analysis("abc123").analysis,
        ));
        let effect = dispatch(&mut app, "plan.move_up");
        assert!(matches!(effect, Effect::None));
        let notice = app.latest_notice().expect("a notice").text.clone();
        assert!(notice.contains("group heading"), "{notice}");
    }

    #[test]
    fn the_plan_command_pins_a_file_and_resets() {
        let (_dir, mut app) = app_ready_to_analyse();
        // Reset needs the analysis to reset *to*, which is what a user has when they
        // are looking at a plan derived from one (FR-4.2).
        app.panel.analysis = Some(Box::new(crate::test_support::stored_analysis("abc123")));
        app.set_plan(crate::domain::plan::Plan::from_analysis(
            &app.panel.analysis.as_ref().expect("set").analysis,
        ));
        let effect = command(&mut app, "plan move tests/money.rs domain");
        let Effect::SavePlan(plan) = effect else {
            panic!("expected a save, got {effect:?}");
        };
        assert_eq!(plan.group_of("tests/money.rs"), Some("domain"));
        assert!(matches!(
            command(&mut app, "plan reset"),
            Effect::SavePlan(_)
        ));
        assert!(!app.plan().expect("a plan").overridden, "reset clears it");
        // `:plan` with no argument explains the current order.
        assert!(matches!(command(&mut app, "plan"), Effect::None));
        let notice = app.latest_notice().expect("a notice").text.clone();
        assert!(notice.contains("1.domain"), "{notice}");
    }

    #[test]
    fn the_context_command_gathers_and_then_shows_what_would_be_sent() {
        let (_dir, mut app) = app_ready_to_analyse();
        let effect = command(&mut app, "context");
        assert!(
            matches!(effect, Effect::GatherContext(AnalysisIntent::Inspect)),
            "{effect:?}"
        );
        // Once it is gathered, the same command opens the inspector rather than
        // gathering again (FR-4.6).
        let bundle = crate::domain::context::build(
            &crate::domain::context::BundleInputs {
                metadata: "PR #141",
                ..crate::domain::context::BundleInputs::default()
            },
            &crate::domain::context::BundlePolicy::default(),
        );
        app.panel.bundle = Some(Box::new(bundle));
        assert!(matches!(command(&mut app, "context"), Effect::None));
        assert_eq!(app.overlay(), Overlay::Context);
        // A number still means the diff's own context (FR-3.2).
        let _ = command(&mut app, "context 10");
        assert_eq!(app.diff_options.context, 10);
    }

    #[test]
    fn the_raw_command_shows_the_answer_only_when_there_is_one() {
        let (_dir, mut app) = app_ready_to_analyse();
        assert!(matches!(command(&mut app, "analyze raw"), Effect::None));
        assert!(
            app.latest_notice()
                .is_some_and(|notice| notice.text.contains("no answer to show")),
            "{:?}",
            app.latest_notice()
        );
        app.panel.raw = Some(("bad json".to_owned(), "the text".to_owned()));
        assert!(matches!(command(&mut app, "analyze raw"), Effect::None));
        assert_eq!(app.overlay(), Overlay::RawAnswer);
    }

    #[test]
    fn an_unknown_analyze_option_is_refused_with_the_options_that_exist() {
        let (_dir, mut app) = app_ready_to_analyse();
        assert!(matches!(command(&mut app, "analyze --now"), Effect::None));
        let error = app.command_error_text().expect("an error").to_owned();
        assert!(error.contains("--force"), "{error}");
    }
}
