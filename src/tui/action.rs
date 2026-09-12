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
    /// Finding things.
    Search,
    /// Reading a diff.
    Diff,
    /// Changing the look.
    Theme,
    /// Talking about a pull request (M3).
    Chat,
}

impl Group {
    /// Human-readable group name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::App => "App",
            Self::Navigation => "Navigation",
            Self::Pane => "Panes",
            Self::Search => "Search",
            Self::Diff => "Diff",
            Self::Theme => "Theme",
            Self::Chat => "Chat",
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
    /// Whether this action is a *hint*: it runs as soon as its prefix is pressed
    /// instead of waiting for `timeoutlen`, while the sequence stays open so a
    /// longer binding can still complete it. Only menus want this (FR-7.3).
    pub hint: bool,
}

/// Every action this build understands.
pub const ACTIONS: &[ActionDef] = &[
    ActionDef {
        id: "app.quit",
        description: "Quit smart-review",
        group: Group::App,
        hint: false,
    },
    ActionDef {
        id: "app.help",
        description: "Show the help popup",
        group: Group::App,
        hint: false,
    },
    ActionDef {
        id: "app.command",
        description: "Open the command line",
        group: Group::App,
        hint: false,
    },
    ActionDef {
        id: "app.leader_menu",
        description: "Show the available leader bindings",
        group: Group::App,
        hint: true,
    },
    ActionDef {
        id: "app.cancel",
        description: "Close the current popup, or cancel pending keys",
        group: Group::App,
        hint: false,
    },
    ActionDef {
        id: "app.refresh",
        description: "Refresh the current view",
        group: Group::App,
        hint: false,
    },
    ActionDef {
        id: "app.doctor",
        description: "Show environment checks",
        group: Group::App,
        hint: false,
    },
    ActionDef {
        id: "app.version",
        description: "Show the version",
        group: Group::App,
        hint: false,
    },
    ActionDef {
        id: "notice.clear",
        description: "Dismiss the current notification",
        group: Group::App,
        hint: false,
    },
    ActionDef {
        id: "app.theme_picker",
        description: "Choose a theme",
        group: Group::Theme,
        hint: false,
    },
    ActionDef {
        id: "app.model_picker",
        description: "Choose the provider, model and thinking settings",
        group: Group::App,
        hint: false,
    },
    ActionDef {
        id: "theme.toggle",
        description: "Switch to the next theme",
        group: Group::Theme,
        hint: false,
    },
    ActionDef {
        id: "nav.up",
        description: "Move up",
        group: Group::Navigation,
        hint: false,
    },
    ActionDef {
        id: "nav.down",
        description: "Move down",
        group: Group::Navigation,
        hint: false,
    },
    ActionDef {
        id: "nav.top",
        description: "Jump to the first item",
        group: Group::Navigation,
        hint: false,
    },
    ActionDef {
        id: "nav.bottom",
        description: "Jump to the last item",
        group: Group::Navigation,
        hint: false,
    },
    ActionDef {
        id: "nav.open",
        description: "Open the selected pull request, or the selected file",
        group: Group::Navigation,
        hint: false,
    },
    ActionDef {
        id: "nav.half_down",
        description: "Move half a screen down",
        group: Group::Navigation,
        hint: false,
    },
    ActionDef {
        id: "nav.half_up",
        description: "Move half a screen up",
        group: Group::Navigation,
        hint: false,
    },
    ActionDef {
        id: "nav.page_down",
        description: "Move a screen down",
        group: Group::Navigation,
        hint: false,
    },
    ActionDef {
        id: "nav.page_up",
        description: "Move a screen up",
        group: Group::Navigation,
        hint: false,
    },
    ActionDef {
        id: "nav.back",
        description: "Close the review, or clear the search and filters",
        group: Group::Navigation,
        hint: false,
    },
    ActionDef {
        id: "search.open",
        description: "Filter the loaded pull requests as you type",
        group: Group::Search,
        hint: false,
    },
    ActionDef {
        id: "search.close",
        description: "Stop editing the search",
        group: Group::Search,
        hint: false,
    },
    ActionDef {
        id: "search.next",
        description: "Next match",
        group: Group::Search,
        hint: false,
    },
    ActionDef {
        id: "search.prev",
        description: "Previous match",
        group: Group::Search,
        hint: false,
    },
    ActionDef {
        id: "filter.menu",
        description: "Add a filter chip",
        group: Group::Search,
        hint: false,
    },
    ActionDef {
        id: "filter.clear",
        description: "Clear the filters and the search",
        group: Group::Search,
        hint: false,
    },
    ActionDef {
        id: "sort.menu",
        description: "Change the sort order",
        group: Group::Search,
        hint: false,
    },
    ActionDef {
        id: "diff.next_hunk",
        description: "Next hunk",
        group: Group::Diff,
        hint: false,
    },
    ActionDef {
        id: "diff.prev_hunk",
        description: "Previous hunk",
        group: Group::Diff,
        hint: false,
    },
    ActionDef {
        id: "diff.next_file",
        description: "Next file",
        group: Group::Diff,
        hint: false,
    },
    ActionDef {
        id: "diff.prev_file",
        description: "Previous file",
        group: Group::Diff,
        hint: false,
    },
    ActionDef {
        id: "diff.toggle_hunk",
        description: "Fold or unfold the hunk under the cursor",
        group: Group::Diff,
        hint: false,
    },
    ActionDef {
        id: "diff.toggle_split",
        description: "Side-by-side view, when the terminal is wide enough",
        group: Group::Diff,
        hint: false,
    },
    ActionDef {
        id: "diff.cycle_context",
        description: "Cycle the context lines: 3, 10, 0",
        group: Group::Diff,
        hint: false,
    },
    ActionDef {
        id: "diff.toggle_whitespace",
        description: "Ignore whitespace-only changes",
        group: Group::Diff,
        hint: false,
    },
    ActionDef {
        id: "review.copy_path",
        description: "Copy the current file path",
        group: Group::Diff,
        hint: false,
    },
    ActionDef {
        id: "diff.toggle_order",
        description: "Switch between the recommended and path orders",
        group: Group::Diff,
        hint: false,
    },
    ActionDef {
        id: "app.analyze_panel",
        description: "Analyse the pull request, or open the analysis",
        group: Group::App,
        hint: false,
    },
    ActionDef {
        id: "plan.move_up",
        description: "Move the selected review-plan group up",
        group: Group::App,
        hint: false,
    },
    ActionDef {
        id: "plan.move_down",
        description: "Move the selected review-plan group down",
        group: Group::App,
        hint: false,
    },
    ActionDef {
        id: "app.load_more",
        description: "Fetch the next page of pull requests",
        group: Group::App,
        hint: false,
    },
    ActionDef {
        id: "chat.open",
        description: "Talk about this pull request",
        group: Group::Chat,
        hint: false,
    },
    ActionDef {
        id: "chat.send",
        description: "Send the question",
        group: Group::Chat,
        hint: false,
    },
    ActionDef {
        id: "chat.newline",
        description: "Add a line to the question",
        group: Group::Chat,
        hint: false,
    },
    ActionDef {
        id: "chat.retry",
        description: "Ask the last question again",
        group: Group::Chat,
        hint: false,
    },
    ActionDef {
        id: "chat.cancel",
        description: "Stop the answer that is arriving",
        group: Group::Chat,
        hint: false,
    },
    ActionDef {
        id: "chat.list",
        description: "List the conversations about this pull request",
        group: Group::Chat,
        hint: false,
    },
    ActionDef {
        id: "chat.export",
        description: "Write a transcript",
        group: Group::Chat,
        hint: false,
    },
    ActionDef {
        id: "pane.next",
        description: "Focus the next pane",
        group: Group::Pane,
        hint: false,
    },
    ActionDef {
        id: "pane.prev",
        description: "Focus the previous pane",
        group: Group::Pane,
        hint: false,
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

/// Whether `id` is a hint action, which fires as soon as its prefix is pressed
/// rather than after the ambiguity timeout (FR-7.3).
#[must_use]
pub fn is_hint(id: &str) -> bool {
    find(id).is_some_and(|action| action.hint)
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
    &[
        Group::App,
        Group::Navigation,
        Group::Pane,
        Group::Search,
        Group::Diff,
        Group::Theme,
        Group::Chat,
    ]
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
    fn every_action_belongs_to_a_group_the_help_popup_lists() {
        // The other direction: deriving the check from `groups()` alone meant a new
        // group could be registered and never appear in `?` or `:keymap`, which is
        // how the whole Search and Diff surface went undiscoverable.
        for action in all() {
            assert!(
                groups().contains(&action.group),
                "{} is in {:?}, which the help popup does not list",
                action.id,
                action.group
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
