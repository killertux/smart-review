//! The action registry (FR-7.2, FR-7.3).
//!
//! Help, the leader menu, the command palette and keybinding validation all read
//! from this single table, so the documented shortcuts can never drift from the
//! behaviour (FR-7.3).
//!
//! Only actions M0 actually implements live here. A binding naming something
//! outside this table is reported as a warning at startup rather than silently
//! doing nothing.

/// How actions are grouped in the help popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Application lifecycle and global commands.
    App,
    /// Moving around.
    Navigation,
    /// Moving between panes.
    Pane,
    /// Changing the look.
    Theme,
}

impl Group {
    /// Human-readable group name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::App => "App",
            Self::Navigation => "Navigation",
            Self::Pane => "Panes",
            Self::Theme => "Theme",
        }
    }
}

/// One user-triggerable action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionDef {
    /// Stable identifier used by keybindings, the command line and tests.
    pub id: &'static str,
    /// One-line description shown in help and the leader menu.
    pub description: &'static str,
    /// Grouping used by the help popup.
    pub group: Group,
}

/// Every action this build understands.
pub const ACTIONS: &[ActionDef] = &[
    ActionDef {
        id: "app.quit",
        description: "Quit smart-review",
        group: Group::App,
    },
    ActionDef {
        id: "app.help",
        description: "Show the help popup",
        group: Group::App,
    },
    ActionDef {
        id: "app.command",
        description: "Open the command line",
        group: Group::App,
    },
    ActionDef {
        id: "app.leader_menu",
        description: "Show the available leader bindings",
        group: Group::App,
    },
    ActionDef {
        id: "app.cancel",
        description: "Close the current popup, or cancel pending keys",
        group: Group::App,
    },
    ActionDef {
        id: "app.refresh",
        description: "Refresh the current view",
        group: Group::App,
    },
    ActionDef {
        id: "app.doctor",
        description: "Show environment checks",
        group: Group::App,
    },
    ActionDef {
        id: "app.theme_picker",
        description: "Choose a theme",
        group: Group::Theme,
    },
    ActionDef {
        id: "theme.use_dark",
        description: "Switch to the dark theme",
        group: Group::Theme,
    },
    ActionDef {
        id: "theme.use_light",
        description: "Switch to the light theme",
        group: Group::Theme,
    },
    ActionDef {
        id: "nav.up",
        description: "Move up",
        group: Group::Navigation,
    },
    ActionDef {
        id: "nav.down",
        description: "Move down",
        group: Group::Navigation,
    },
    ActionDef {
        id: "nav.top",
        description: "Jump to the first item",
        group: Group::Navigation,
    },
    ActionDef {
        id: "nav.bottom",
        description: "Jump to the last item",
        group: Group::Navigation,
    },
    ActionDef {
        id: "pane.next",
        description: "Focus the next pane",
        group: Group::Pane,
    },
    ActionDef {
        id: "pane.prev",
        description: "Focus the previous pane",
        group: Group::Pane,
    },
];

/// Every action, in help order.
#[must_use]
pub fn all() -> &'static [ActionDef] {
    ACTIONS
}

/// Looks up an action by id.
#[must_use]
pub fn find(id: &str) -> Option<&'static ActionDef> {
    ACTIONS.iter().find(|action| action.id == id)
}

/// Whether `id` names an action this build implements.
#[must_use]
pub fn is_known(id: &str) -> bool {
    find(id).is_some()
}

/// The closest known action to `id`, for "did you mean" messages (DEV-7).
#[must_use]
pub fn suggest(id: &str) -> Option<&'static ActionDef> {
    ACTIONS
        .iter()
        .map(|action| (levenshtein(id, action.id), action))
        .filter(|(distance, _)| *distance <= 4)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, action)| action)
}

/// Groups in display order.
#[must_use]
pub fn groups() -> &'static [Group] {
    &[Group::App, Group::Navigation, Group::Pane, Group::Theme]
}

/// Levenshtein distance, used only for suggestions.
pub(crate) fn levenshtein(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0usize; right.len() + 1];

    for (i, a) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, b) in right.iter().enumerate() {
            let cost = usize::from(a != b);
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_described() {
        let mut seen = std::collections::HashSet::new();
        for action in all() {
            assert!(seen.insert(action.id), "duplicate id {}", action.id);
            assert!(action.id.contains('.'), "{} should be dotted", action.id);
            assert!(!action.description.is_empty());
        }
    }

    #[test]
    fn lookup_works() {
        assert!(is_known("app.quit"));
        assert!(!is_known("app.nonsense"));
    }

    #[test]
    fn suggestions_point_at_near_misses() {
        assert_eq!(suggest("app.quit").map(|a| a.id), Some("app.quit"));
        assert_eq!(suggest("app.qut").map(|a| a.id), Some("app.quit"));
        assert_eq!(suggest("nav.dwn").map(|a| a.id), Some("nav.down"));
        assert!(suggest("completely.different").is_none());
    }

    #[test]
    fn every_group_has_actions() {
        for group in groups() {
            assert!(
                all().iter().any(|action| action.group == *group),
                "{group:?} is empty"
            );
        }
    }

    #[test]
    fn distance_is_symmetric_and_zero_on_equal_input() {
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("flaw", "lawn"), 2);
    }
}
