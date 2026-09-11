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
        "app.load_more" => {
            if app.review.is_some() {
                app.notice(
                    NoticeLevel::Warn,
                    "loading more applies to the list; press Esc to go back",
                );
                Effect::None
            } else if !app.list.can_load_more() {
                app.notice(
                    NoticeLevel::Warn,
                    "that is every pull request the current filters match",
                );
                Effect::None
            } else {
                Effect::LoadMore
            }
        }
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
                app.list.offline = None;
                Effect::LoadPullRequests
            }
        }
        "pane.next" => set_focus(app, app.focus.next()),
        "pane.prev" => set_focus(app, app.focus.prev()),
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
        _ => Effect::None,
    }
}

/// The list, search and filter actions.
fn dispatch_list(app: &mut App, id: &str) -> Effect {
    match id {
        "nav.up" | "nav.down" | "nav.top" | "nav.bottom" => {
            match id {
                "nav.up" => app.move_cursor(-1),
                "nav.down" => app.move_cursor(1),
                "nav.top" => app.move_cursor_to(false),
                _ => app.move_cursor_to(true),
            }
            Effect::None
        }
        "nav.open" => open_selected(app),
        "nav.back" => go_back(app),
        "nav.half_down" | "nav.page_down" | "nav.half_up" | "nav.page_up" => {
            let forward = matches!(id, "nav.half_down" | "nav.page_down");
            let half = matches!(id, "nav.half_down" | "nav.half_up");
            move_current(app, if forward { 1 } else { -1 }, half);
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
            move_current(app, 1, false);
            Effect::None
        }
        "search.prev" => {
            move_current(app, -1, false);
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
            app.list.offline = None;
            if app.environment.is_some() {
                Effect::LoadPullRequests
            } else {
                Effect::None
            }
        }
        _ => Effect::None,
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
        _ => Effect::None,
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
fn go_back(app: &mut App) -> Effect {
    if app.review.is_some() {
        app.close_review();
        return Effect::None;
    }
    if app.list.is_filtered() {
        let was_loading = app.list.loading;
        app.list.clear_filters();
        app.list.offline = None;
        // Only re-ask GitHub if something it was asked for changed.
        return if was_loading || app.environment.is_none() {
            Effect::None
        } else {
            Effect::LoadPullRequests
        };
    }
    Effect::None
}

/// Moves whatever pane has the cursor.
fn move_current(app: &mut App, direction: i32, half: bool) {
    if let Some(view) = app.review.as_mut() {
        if view.tree_focused {
            view.move_tree(direction);
        } else {
            view.move_page(direction, half);
        }
        return;
    }
    app.list.move_page(direction, half);
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

/// Cycles the diff context between the sizes the requirement names (FR-3.2).
fn cycle_context(app: &mut App) -> Effect {
    let Some(view) = app.review.as_mut() else {
        app.notice(NoticeLevel::Warn, "open a pull request first");
        return Effect::None;
    };
    view.context = match view.context {
        0 => 3,
        3 => 10,
        _ => 0,
    };
    let context = view.context;
    // Only a local workspace can re-diff without refetching; in remote mode the fix
    // is a new `gh pr diff`, which is what `R` does.
    app.notice(
        NoticeLevel::Info,
        format!(
            "context {context} lines; press R to refetch ({})",
            if context == 0 {
                "0 needs the local workspace from M2"
            } else {
                "10 is a preference, 3 is what GitHub sends"
            }
        ),
    );
    Effect::None
}

/// Toggles whitespace-ignoring, which needs the local workspace (FR-3.2).
fn toggle_whitespace(app: &mut App) -> Effect {
    let Some(view) = app.review.as_mut() else {
        app.notice(NoticeLevel::Warn, "open a pull request first");
        return Effect::None;
    };
    view.ignore_whitespace = !view.ignore_whitespace;
    let ignoring = view.ignore_whitespace;
    if ignoring {
        app.notice(
            NoticeLevel::Warn,
            "whitespace-ignoring diffs need the local workspace, which arrives in M2",
        );
    } else {
        app.notice(NoticeLevel::Info, "showing whitespace changes");
    }
    Effect::None
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
    let mut length = first.len();
    for name in names.iter().skip(1) {
        length = length.min(name.len());
        while length > 0 && !name.starts_with(&first[..length]) {
            length -= 1;
        }
    }
    first[..length].to_owned()
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
    fn every_registered_command_action_has_a_dispatch_arm() {
        // `notice.clear` and `app.version` are reached through the command line,
        // so the catch-all must not swallow them.
        for id in ["app.version", "notice.clear"] {
            assert!(action::is_known(id), "{id} should be registered");
        }
    }
}
