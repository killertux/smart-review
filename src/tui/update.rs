//! Actions and the `:` command line (FR-7.2, FR-7.3, FR-7.4).
//!
//! Every action in the registry is dispatched from here, so adding a feature
//! means adding a registry entry, a default binding, and a match arm.

use std::fmt::Write as _;
use std::time::Duration;

use crate::tui::action;
use crate::tui::app::{App, NoticeLevel, Overlay};
use crate::tui::keymap::{self, KeyCombo, Mode};

/// Commands accepted by the `:` line, with the action each one triggers.
pub const COMMANDS: &[(&str, &str)] = &[
    ("doctor", "app.doctor"),
    ("help", "app.help"),
    ("keymap", "app.help"),
    ("q", "app.quit"),
    ("qa", "app.quit"),
    ("quit", "app.quit"),
    ("refresh", "app.refresh"),
    ("set", "app.command"),
    ("theme", "app.theme_picker"),
    ("version", "app.version"),
];

/// Runs an action.
///
/// Returns whether the pending key sequence should be kept. Only the leader menu
/// returns `true`, because it stays open so the next key can complete the
/// sequence (FR-7.3).
pub fn dispatch(app: &mut App, id: &str) -> bool {
    match id {
        "app.quit" => {
            app.quit();
            false
        }
        "app.help" => {
            app.open_overlay(Overlay::Help);
            false
        }
        "app.doctor" => {
            app.open_overlay(Overlay::Doctor);
            false
        }
        "app.theme_picker" => {
            app.open_overlay(Overlay::ThemePicker);
            false
        }
        "app.leader_menu" => {
            app.open_overlay(Overlay::Leader);
            true
        }
        "app.command" => {
            app.command.clear();
            app.mode = Mode::Command;
            false
        }
        "app.cancel" => {
            app.close_overlay();
            false
        }
        "app.refresh" => {
            app.notice(
                NoticeLevel::Info,
                "nothing to refresh yet: pull requests arrive in M1",
            );
            false
        }
        "app.version" => {
            app.notice(
                NoticeLevel::Info,
                format!("smart-review {}", env!("CARGO_PKG_VERSION")),
            );
            false
        }
        "theme.use_dark" => {
            app.set_theme("dark");
            false
        }
        "theme.use_light" => {
            app.set_theme("light");
            false
        }
        "nav.up" => {
            app.move_cursor(-1);
            false
        }
        "nav.down" => {
            app.move_cursor(1);
            false
        }
        "nav.top" => {
            app.move_cursor_to(false);
            false
        }
        "nav.bottom" => {
            app.move_cursor_to(true);
            false
        }
        "pane.next" | "pane.prev" => {
            app.focus = app.focus.next();
            false
        }
        other => {
            app.notice(
                NoticeLevel::Warn,
                format!("`{other}` is not implemented in this build"),
            );
            false
        }
    }
}

/// Runs a `:` command.
pub fn command(app: &mut App, input: &str) {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return;
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default();
    let argument = parts.next().unwrap_or_default().trim();

    match name {
        "q" | "qa" | "quit" => {
            dispatch(app, "app.quit");
        }
        "help" | "keymap" => {
            dispatch(app, "app.help");
        }
        "doctor" => {
            dispatch(app, "app.doctor");
        }
        "refresh" => {
            dispatch(app, "app.refresh");
        }
        "version" => {
            dispatch(app, "app.version");
        }
        "theme" => {
            if argument.is_empty() {
                dispatch(app, "app.theme_picker");
            } else {
                app.set_theme(argument);
            }
        }
        "set" => set_option(app, argument),
        other => {
            let mut message = format!("`{other}` is not a command");
            if let Some(suggestion) = closest_command(other) {
                let _ = write!(message, "; did you mean `{suggestion}`?");
            }
            app.command_error(message);
        }
    }
}

/// Applies `:set <key>=<value>` for the options that can change at runtime.
fn set_option(app: &mut App, spec: &str) {
    let Some((key, value)) = spec.split_once('=') else {
        app.command_error("usage: :set <key>=<value>, for example :set ui.timeoutlen=250");
        return;
    };
    let key = key.trim();
    let value = value.trim();

    match key {
        "ui.theme" => app.set_theme(value),
        "ui.timeoutlen" => match value.parse::<u64>() {
            Ok(milliseconds) => {
                app.keymap.set_timeout(Duration::from_millis(milliseconds));
                app.notice(NoticeLevel::Info, format!("timeoutlen = {milliseconds} ms"));
            }
            Err(_) => {
                app.command_error(format!("`{value}` is not a number of milliseconds"));
            }
        },
        "ui.leader" => match keymap::parse_keys(value, &KeyCombo::char(' ')) {
            Ok(keys) if keys.len() == 1 => {
                value.clone_into(&mut app.config.ui.leader);
                app.reload_keymap();
                app.notice(
                    NoticeLevel::Info,
                    format!("leader = {}", keys[0].describe()),
                );
            }
            Ok(_) => app.command_error(format!(
                "`{value}` must be a single key, for example <Space> or ,"
            )),
            Err(error) => app.command_error(error.to_string()),
        },
        _ => app.command_error(format!(
            "`{key}` cannot be changed at runtime yet; edit {}",
            app.config_path.display()
        )),
    }
}

/// Completes a partially typed command name.
pub fn complete_command(input: &str) -> String {
    let typed = input.trim_start();
    if typed.is_empty() || typed.contains(char::is_whitespace) {
        return input.to_owned();
    }

    let matches: Vec<&str> = COMMANDS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| name.starts_with(typed))
        .collect();

    match matches.as_slice() {
        [] => input.to_owned(),
        [single] => (*single).to_owned(),
        several => common_prefix(several),
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
        .map(|(name, _)| (action::levenshtein(typed, name), *name))
        .filter(|(distance, _)| *distance <= 3)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_maps_to_a_known_action() {
        for (name, id) in COMMANDS {
            if *id == "app.version" {
                continue;
            }
            assert!(
                action::is_known(id),
                "command `{name}` maps to unknown action `{id}`"
            );
        }
    }

    #[test]
    fn completion_fills_in_a_unique_command() {
        assert_eq!(complete_command("doc"), "doctor");
        assert_eq!(complete_command("q"), "q");
        assert_eq!(complete_command(""), "");
        assert_eq!(complete_command("bogus"), "bogus");
    }

    #[test]
    fn completion_uses_the_common_prefix_for_ambiguous_input() {
        // "q", "qa" and "quit" all start with "q", so the prefix is just "q".
        assert_eq!(complete_command("q"), "q");
        assert_eq!(common_prefix(&["theme", "the"]), "the");
    }

    #[test]
    fn completion_stops_at_arguments() {
        assert_eq!(complete_command("theme li"), "theme li");
    }

    #[test]
    fn near_miss_commands_are_suggested() {
        assert_eq!(closest_command("docotr"), Some("doctor"));
        assert_eq!(closest_command("zzzzzz"), None);
    }
}
