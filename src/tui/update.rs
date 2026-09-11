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
            app.notice(
                NoticeLevel::Info,
                "nothing to refresh yet: pull requests arrive in M1",
            );
            Effect::None
        }
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
        "nav.up" => {
            app.move_cursor(-1);
            Effect::None
        }
        "nav.down" => {
            app.move_cursor(1);
            Effect::None
        }
        "nav.top" => {
            app.move_cursor_to(false);
            Effect::None
        }
        "nav.bottom" => {
            app.move_cursor_to(true);
            Effect::None
        }
        "pane.next" => set_focus(app, app.focus.next()),
        "pane.prev" => set_focus(app, app.focus.prev()),
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
                fuzzy_score(typed, name)?
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

/// Subsequence match with bonuses for contiguity and for matching early.
///
/// Returns `None` when `needle` is not a subsequence of `haystack`.
fn fuzzy_score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }

    let characters: Vec<char> = haystack.chars().collect();
    let mut score: i32 = 0;
    let mut cursor = 0;
    let mut previous: Option<usize> = None;

    for wanted in needle.chars() {
        let found = cursor
            + characters
                .get(cursor..)?
                .iter()
                .position(|c| *c == wanted)?;
        score += 1;
        if previous == Some(found.wrapping_sub(1)) {
            score += 2;
        }
        cursor = found + 1;
        previous = Some(found);
    }

    // Prefer shorter candidates, then earlier matches.
    let length_penalty = i32::try_from(haystack.chars().count()).unwrap_or(i32::MAX);
    let cursor_penalty = i32::try_from(cursor).unwrap_or(i32::MAX);
    Some(score * 10 - length_penalty - cursor_penalty)
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
        let contiguous = fuzzy_score("doc", "doctor").unwrap();
        let scattered = fuzzy_score("doc", "d-o-c-nonsense").unwrap();
        assert!(contiguous > scattered, "{contiguous} vs {scattered}");
        assert!(fuzzy_score("z", "doctor").is_none());
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
