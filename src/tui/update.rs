//! Actions and the `:` command line (FR-7.2, FR-7.3, FR-7.4).
//!
//! Every action in the registry is dispatched from here, so adding a feature
//! means adding a registry entry, a default binding and a match arm. Dispatch
//! returns an [`Effect`] rather than performing IO, so the reducer stays pure.

use std::time::Duration;

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
    ("version", "app.version", "Show the version"),
];

/// Runs an action.
///
/// The returned [`Effect`] tells the event loop what it has to do; only the
/// leader menu asks to keep the pending key sequence so the next key can
/// complete it (FR-7.3).
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
            app.cancel_overlay();
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
        other
            if other.starts_with("app.")
                || other.starts_with("notice.")
                || other.starts_with("theme.") =>
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
fn dispatch_app(app: &mut App, id: &str) -> Effect {
    match id {
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
fn cycle_context(app: &mut App) -> Effect {
    if app.review.is_none() {
        app.notice(NoticeLevel::Warn, "open a pull request first");
        return Effect::None;
    }
    app.notice(
        NoticeLevel::Warn,
        format!(
            "context is fixed at {} lines in remote mode; the local workspace in M2 makes it adjustable",
            app.review.as_ref().map_or(3, |view| view.context)
        ),
    );
    Effect::None
}

/// Explains that ignoring whitespace needs the local workspace (FR-3.2).
fn toggle_whitespace(app: &mut App) -> Effect {
    if app.review.is_none() {
        app.notice(NoticeLevel::Warn, "open a pull request first");
        return Effect::None;
    }
    app.notice(
        NoticeLevel::Warn,
        "whitespace-ignoring diffs need the local workspace, which arrives in M2",
    );
    Effect::None
}

/// Moves the focus on. Inside a review that means the tree and the diff in turn
/// (FR-3.3: the two panes keep independent cursors and `Tab` moves between them).
fn switch_pane(app: &mut App, forward: bool) -> Effect {
    if let Some(view) = app.review.as_mut() {
        view.tree_focused = !view.tree_focused;
        if view.tree_focused {
            app.notice(NoticeLevel::Info, "file tree: Enter opens, j/k moves");
        }
        return Effect::None;
    }
    set_focus(
        app,
        if forward {
            app.focus.next()
        } else {
            app.focus.prev()
        },
    )
}

fn set_focus(app: &mut App, pane: crate::tui::app::Pane) -> Effect {
    app.focus = pane;
    app.state.focus = Some(pane.label().to_owned());
    Effect::SaveState
}

/// Runs a `:` command.
pub fn command(app: &mut App, input: &str) -> Effect {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Effect::None;
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default();
    let argument = parts.next().unwrap_or_default().trim();

    match name {
        "q" | "qa" | "quit" => dispatch(app, "app.quit"),
        "help" => dispatch(app, "app.help"),
        "doctor" => dispatch(app, "app.doctor"),
        "refresh" => dispatch(app, "app.refresh"),
        "version" => dispatch(app, "app.version"),
        "messages" => dispatch(app, "notice.clear"),
        "keymap" => keymap_command(app, argument),
        "load-more" => dispatch(app, "app.load_more"),
        "clear-filters" => dispatch(app, "filter.clear"),
        "copy-path" => dispatch(app, "review.copy_path"),
        "pr" => open_pr(app, argument),
        "filter" => add_filter(app, argument),
        "filter-remove" => remove_filter(app, argument),
        "sort" => set_sort(app, argument),
        "theme" => match argument {
            "" => dispatch(app, "app.theme_picker"),
            "reload" => app.reload_theme(),
            "next" => dispatch(app, "theme.toggle"),
            name => app.set_theme(name),
        },
        "set" => set_option(app, argument),
        other => {
            let mut message = format!("`{other}` is not a command");
            if let Some(suggestion) = closest_command(other) {
                let _ = std::fmt::Write::write_fmt(
                    &mut message,
                    format_args!("; did you mean `{suggestion}`?"),
                );
            }
            app.command_error(message);
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
}
