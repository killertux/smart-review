//! The pull request list: what it shows, and what it is showing it for (FR-2.1,
//! FR-2.2, FR-2.3).
//!
//! The list keeps the *full* fetched set and a separate cursor, so a client-side
//! search can narrow what is drawn without throwing away what has been fetched, and
//! so replacing the set with a refreshed one can keep the cursor on the same pull
//! request rather than on the same row (FR-2.3).

use crate::domain::pr::PullRequestSummary;
use crate::domain::query::{Filter, PrQuery, PrSort, PrStateFilter};
use crate::ports::forge::PullRequestPage;

/// The state of the PR list pane.
#[derive(Debug, Clone, Default)]
pub struct PrListState {
    /// Everything that has been fetched, newest first.
    pub items: Vec<PullRequestSummary>,
    /// The client-side search text, matched fuzzily (FR-2.2).
    pub search: String,
    /// The server-side filters, which are part of the query (FR-2.2).
    pub filters: Vec<Filter>,
    /// Which states to ask for.
    pub state_filter: PrStateFilter,
    /// The order to ask for.
    pub sort: PrSort,
    /// How many the current query asks for.
    pub limit: u32,
    /// How many exist in total, when GitHub has told us.
    pub total: Option<u32>,
    /// The cursor, as an index into [`Self::items`].
    cursor: usize,
    /// The first visible row, as an index into the *visible* rows.
    pub scroll: usize,
    /// The viewport height, learned from the last frame so paging can use it.
    pub viewport: u16,
    /// Whether a fetch is in flight.
    pub loading: bool,
    /// Why the shown data is stale, when the forge could not be reached (DEC-14).
    pub offline: Option<String>,
    /// The last failure, shown in the pane until something replaces it.
    pub error: Option<String>,
    /// Whether the count job is still outstanding.
    pub counting: bool,
}

impl PrListState {
    /// A list that asks for `limit` pull requests.
    #[must_use]
    pub fn new(limit: u32) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }

    /// The query the list currently describes.
    #[must_use]
    pub fn query(&self) -> PrQuery {
        PrQuery {
            state: self.state_filter,
            filters: self.filters.clone(),
            sort: self.sort,
            limit: self.limit,
        }
    }

    /// Whether anything narrows what is shown, client side or server side.
    #[must_use]
    pub fn is_filtered(&self) -> bool {
        self.query().is_filtered() || !self.search.trim().is_empty()
    }

    /// The indices of the items the search matches, in list order.
    ///
    /// Recomputed on demand rather than cached: it is a fuzzy match over a few
    /// hundred short strings, which is far cheaper than the bookkeeping needed to
    /// keep a cache correct through every change to the list.
    #[must_use]
    pub fn visible(&self) -> Vec<usize> {
        let search = self.search.trim();
        if search.is_empty() {
            return (0..self.items.len()).collect();
        }
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.matches(search))
            .map(|(index, _)| index)
            .collect()
    }

    /// How many rows are shown.
    #[must_use]
    pub fn visible_len(&self) -> usize {
        self.visible().len()
    }

    /// Where the cursor is among the visible rows, or `None` when nothing matches.
    #[must_use]
    pub fn cursor_position(&self) -> Option<usize> {
        let visible = self.visible();
        visible.iter().position(|index| *index == self.cursor)
    }

    /// The selected pull request.
    #[must_use]
    pub fn selected(&self) -> Option<&PullRequestSummary> {
        let visible = self.visible();
        visible
            .get(self.cursor_position()?)
            .and_then(|index| self.items.get(*index))
    }

    /// Moves the cursor by `delta` rows, stopping at the ends.
    pub fn move_cursor(&mut self, delta: i32) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let position = self
            .cursor_position()
            .unwrap_or(0)
            .saturating_add_signed(delta as isize)
            .min(visible.len() - 1);
        self.cursor = visible[position];
    }

    /// Moves the cursor to the first or last visible row.
    pub fn move_cursor_to(&mut self, last: bool) {
        let visible = self.visible();
        if let Some(index) = if last {
            visible.last()
        } else {
            visible.first()
        } {
            self.cursor = *index;
        }
    }

    /// Moves by whole screens, which is what `<C-d>`/`<C-u>` and `<C-f>`/`<C-b>` do.
    pub fn move_page(&mut self, direction: i32, half: bool) {
        let height = i32::from(self.viewport.max(1));
        let step = if half { (height / 2).max(1) } else { height };
        self.move_cursor(direction * step);
    }

    /// Sets the client-side search text, keeping the cursor on something visible.
    pub fn set_search(&mut self, search: &str) {
        search.clone_into(&mut self.search);
        self.clamp_cursor();
    }

    /// Keeps the cursor on a visible row after the set of visible rows changes.
    fn clamp_cursor(&mut self) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        if !visible.contains(&self.cursor) {
            self.cursor = visible[0];
            self.scroll = 0;
        }
    }

    /// Adds a server-side filter and re-asks (FR-2.2).
    pub fn push_filter(&mut self, filter: Filter) {
        let mut query = self.query();
        query.push(filter);
        self.filters = query.filters;
        self.state_filter = query.state;
    }

    /// Removes the filter at `index`.
    pub fn remove_filter(&mut self, index: usize) {
        let mut query = self.query();
        query.remove(index);
        self.filters = query.filters;
    }

    /// Forgets every filter and the search text.
    pub fn clear_filters(&mut self) {
        self.filters.clear();
        self.state_filter = PrStateFilter::Open;
        self.search.clear();
        self.scroll = 0;
    }

    /// The chips the filter bar draws, with the state chip first.
    #[must_use]
    pub fn chips(&self) -> Vec<String> {
        let mut chips: Vec<String> = Vec::new();
        chips.push(format!("is:{}", self.state_filter.label()));
        for filter in &self.filters {
            chips.push(filter.label());
        }
        if self.sort != PrSort::default() {
            chips.push(format!("sort:{}", self.sort.label()));
        }
        chips
    }

    /// Replaces the list with a freshly fetched page (FR-2.3).
    ///
    /// The cursor follows the pull request it was on, not the row it was on: a
    /// refreshed list is often a *reordered* list, and landing on a different PR
    /// after pressing `R` would be the kind of small betrayal that makes a tool
    /// tiring to use.
    pub fn replace(&mut self, page: PullRequestPage) {
        let previous = self.selected().map(|item| item.number);
        let more = page.may_have_more();
        let total = page.total;
        self.limit = page.limit;
        self.total = total;
        self.items = page.items;
        self.loading = false;
        self.counting = total.is_none() && more;
        self.apply_cursor(previous);
    }

    /// Appends another page, which is what `:load-more` does (FR-2.1).
    ///
    /// A *short* page means the fetch has reached the end, so the combined length
    /// is the true total and no count query is needed. A full page means there may
    /// be more, so whatever total was already known is kept rather than replaced by
    /// the size of the page just fetched.
    pub fn append(&mut self, page: PullRequestPage) {
        let previous = self.selected().map(|item| item.number);
        let reached_the_end = page.total.is_some();
        let may_have_more = page.may_have_more();

        self.items.extend(page.items);
        self.limit = page.limit;
        self.loading = false;
        if reached_the_end {
            self.total = u32::try_from(self.items.len()).ok();
            self.counting = false;
        } else {
            self.counting = may_have_more && self.total.is_none();
        }
        self.apply_cursor(previous);
    }

    /// Records the exact number of PRs the query matches (FR-2.1).
    pub fn set_total(&mut self, total: u32) {
        self.total = Some(total);
        self.counting = false;
    }

    /// Puts the cursor back on `number`, or at the top when it is gone.
    fn apply_cursor(&mut self, number: Option<u64>) {
        let Some(number) = number else {
            self.move_cursor_to(false);
            return;
        };
        if let Some(index) = self.items.iter().position(|item| item.number == number) {
            self.cursor = index;
            // A row that moved needs its scroll position reconsidered; the frame
            // does that from the cursor, so nothing else is needed here.
        } else {
            self.move_cursor_to(false);
        }
    }

    /// The status line summary: how many are shown and of how many (FR-2.1).
    #[must_use]
    pub fn status_label(&self) -> String {
        let shown = self.visible_len();
        let fetched = self.items.len();

        let counts = match self.total {
            Some(total) => format!("showing {shown} of {total}"),
            None if self.counting => format!("showing {shown} of ≥{fetched}…"),
            // The page was full, so all that is known is that more exist.
            None if fetched >= usize::try_from(self.limit).unwrap_or(usize::MAX) => {
                format!("showing {shown} of ≥{fetched}")
            }
            None => format!("showing {shown}"),
        };

        let filtered = if self.search.trim().is_empty() || shown == fetched {
            String::new()
        } else {
            format!(" ({} hidden by search)", fetched - shown)
        };

        let offline = self
            .offline
            .as_ref()
            .map_or(String::new(), |_| " · offline".to_owned());

        format!("{counts}{filtered}{offline}")
    }

    /// Whether the empty pane should explain that the search matched nothing.
    #[must_use]
    pub fn is_empty_because_of_search(&self) -> bool {
        !self.items.is_empty() && self.visible().is_empty()
    }

    /// The effective query, phrased for an empty state (FR-2.2).
    #[must_use]
    pub fn describe_query(&self) -> String {
        let mut parts = vec![self.query().describe()];
        if !self.search.trim().is_empty() {
            parts.push(format!("search {:?}", self.search.trim()));
        }
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pr::{CheckSummary, PrState};
    use crate::domain::time::Timestamp;

    fn summary(number: u64, title: &str) -> PullRequestSummary {
        PullRequestSummary {
            number,
            title: title.to_owned(),
            author: "alice".to_owned(),
            state: PrState::Open,
            is_draft: false,
            base_ref: "main".to_owned(),
            head_ref: "topic".to_owned(),
            head_sha: "abc".to_owned(),
            created_at: Timestamp::default(),
            updated_at: Timestamp::default(),
            additions: 1,
            deletions: 1,
            changed_files: 1,
            labels: Vec::new(),
            review_decision: None,
            checks: CheckSummary::default(),
            url: String::new(),
            is_cross_repository: false,
        }
    }

    fn list_of(numbers: &[u64]) -> PrListState {
        let mut list = PrListState::new(50);
        list.replace(PullRequestPage::complete(
            numbers
                .iter()
                .map(|number| summary(*number, &format!("PR {number}")))
                .collect(),
            50,
        ));
        list
    }

    #[test]
    fn the_cursor_walks_the_visible_rows_and_stops_at_the_ends() {
        let mut list = list_of(&[3, 2, 1]);
        assert_eq!(list.selected().unwrap().number, 3);

        list.move_cursor(1);
        assert_eq!(list.selected().unwrap().number, 2);
        list.move_cursor(1);
        assert_eq!(list.selected().unwrap().number, 1);
        list.move_cursor(1);
        assert_eq!(
            list.selected().unwrap().number,
            1,
            "the bottom is the bottom"
        );
        list.move_cursor(-5);
        assert_eq!(list.selected().unwrap().number, 3);
    }

    #[test]
    fn an_empty_list_has_no_selection_and_does_not_move() {
        let mut list = PrListState::new(50);
        assert!(list.selected().is_none());
        list.move_cursor(1);
        list.move_cursor_to(true);
        assert!(list.selected().is_none());
        assert_eq!(list.status_label(), "showing 0");
    }

    #[test]
    fn search_narrows_what_is_shown_without_losing_what_was_fetched() {
        let mut list = list_of(&[142, 141, 138]);
        list.items[0].title = "Add retry to the webhook dispatcher".to_owned();
        list.items[1].title = "WIP refactor of billing domain".to_owned();
        list.items[2].title = "Bump tokio to 1.53".to_owned();

        list.set_search("billing");
        assert_eq!(list.visible_len(), 1);
        assert_eq!(list.selected().unwrap().number, 141);
        assert_eq!(list.items.len(), 3, "the fetched set is untouched");
        assert_eq!(list.status_label(), "showing 1 of 3 (2 hidden by search)");

        list.set_search("");
        assert_eq!(list.visible_len(), 3);
        assert_eq!(list.status_label(), "showing 3 of 3");
    }

    #[test]
    fn a_search_that_matches_nothing_keeps_the_cursor_usable() {
        let mut list = list_of(&[3, 2, 1]);
        list.set_search("zzzz");
        assert!(list.selected().is_none());
        assert!(list.is_empty_because_of_search());
        assert_eq!(list.describe_query(), "is:open · search \"zzzz\"");

        list.set_search("");
        assert!(list.selected().is_some(), "the cursor comes back");
    }

    #[test]
    fn moving_within_a_search_stays_within_the_matches() {
        let mut list = list_of(&[142, 141, 138]);
        list.items[0].title = "webhook retry".to_owned();
        list.items[1].title = "billing refactor".to_owned();
        list.items[2].title = "webhook dispatcher".to_owned();

        list.set_search("webhook");
        assert_eq!(list.selected().unwrap().number, 142);
        list.move_cursor(1);
        assert_eq!(
            list.selected().unwrap().number,
            138,
            "the non-matching row is skipped"
        );
        list.move_cursor(1);
        assert_eq!(list.selected().unwrap().number, 138);
    }

    #[test]
    fn refreshing_keeps_the_cursor_on_the_same_pull_request() {
        // The network result is often reordered; the cursor must follow the PR.
        let mut list = list_of(&[3, 2, 1]);
        list.move_cursor(1);
        assert_eq!(list.selected().unwrap().number, 2);

        list.replace(PullRequestPage::complete(
            vec![
                summary(4, "new"),
                summary(3, "three"),
                summary(2, "two"),
                summary(1, "one"),
            ],
            50,
        ));
        assert_eq!(list.selected().unwrap().number, 2);
    }

    #[test]
    fn refreshing_when_the_pull_request_is_gone_moves_to_the_top() {
        let mut list = list_of(&[3, 2, 1]);
        list.move_cursor(2);
        assert_eq!(list.selected().unwrap().number, 1);

        list.replace(PullRequestPage::complete(vec![summary(9, "nine")], 50));
        assert_eq!(list.selected().unwrap().number, 9);
    }

    #[test]
    fn loading_more_appends_and_keeps_the_cursor() {
        let mut list = list_of(&[3, 2]);
        list.set_total(4);
        list.move_cursor(1);
        assert_eq!(list.selected().unwrap().number, 2);

        // A full page means there may be more, so the known total stands.
        list.append(PullRequestPage::possibly_truncated(
            vec![summary(1, "one"), summary(0, "zero")],
            100,
        ));
        assert_eq!(list.items.len(), 4);
        assert_eq!(list.limit, 100);
        assert_eq!(list.selected().unwrap().number, 2);
        assert_eq!(list.total, Some(4));
        assert_eq!(list.status_label(), "showing 4 of 4");
    }

    #[test]
    fn a_short_page_ends_the_fetch_and_answers_the_total_outright() {
        // 50 then 12: the second page was short, so there is nothing after it and
        // the count query is unnecessary (FR-2.1: never silently truncate).
        let mut list = PrListState::new(50);
        list.replace(PullRequestPage::possibly_truncated(
            (1..=50).map(|number| summary(number, "x")).collect(),
            50,
        ));
        assert!(list.counting, "the count is outstanding after a full page");

        list.append(PullRequestPage::complete(
            (51..=62).map(|number| summary(number, "x")).collect(),
            100,
        ));
        assert_eq!(list.total, Some(62));
        assert!(!list.counting);
        assert_eq!(list.status_label(), "showing 62 of 62");
    }

    #[test]
    fn the_status_line_is_honest_before_the_count_arrives() {
        let mut list = PrListState::new(50);
        list.loading = true;
        list.replace(PullRequestPage::possibly_truncated(
            (1..=50).map(|number| summary(number, "x")).collect(),
            50,
        ));
        assert!(list.counting, "the count is now outstanding");
        assert_eq!(list.status_label(), "showing 50 of ≥50…");

        list.set_total(137);
        assert!(!list.counting);
        assert_eq!(list.status_label(), "showing 50 of 137");
    }

    #[test]
    fn a_short_page_needs_no_count() {
        let mut list = PrListState::new(50);
        list.replace(PullRequestPage::complete(vec![summary(1, "one")], 50));
        assert!(!list.counting);
        assert_eq!(
            list.status_label(),
            "1 of 1".replace("1 of 1", "showing 1 of 1")
        );
    }

    #[test]
    fn the_offline_indicator_appears_in_the_status_line() {
        let mut list = list_of(&[1]);
        list.offline = Some("could not resolve host".to_owned());
        assert!(
            list.status_label().ends_with("· offline"),
            "{}",
            list.status_label()
        );
    }

    #[test]
    fn filters_become_chips_with_the_state_first() {
        let mut list = PrListState::new(50);
        assert_eq!(list.chips(), vec!["is:open"]);

        list.push_filter(Filter::Author("alice".to_owned()));
        list.push_filter(Filter::Label("bug".to_owned()));
        assert_eq!(list.chips(), vec!["is:open", "author:alice", "label:bug"]);
        assert!(list.is_filtered());
        assert_eq!(list.query().filters.len(), 2);

        list.remove_filter(0);
        assert_eq!(list.chips(), vec!["is:open", "label:bug"]);
    }

    #[test]
    fn clearing_filters_also_clears_the_search() {
        let mut list = list_of(&[1]);
        list.push_filter(Filter::Author("alice".to_owned()));
        list.set_search("billing");
        list.sort = PrSort::UpdatedAsc;

        list.clear_filters();
        assert_eq!(
            list.chips(),
            vec!["is:open", "sort:least recently updated"],
            "the sort is not a filter, so it survives :clear-filters"
        );
        assert!(list.search.is_empty());
        assert_eq!(list.sort, PrSort::UpdatedAsc);
        assert!(!list.is_filtered());
    }

    #[test]
    fn a_non_default_sort_is_shown_as_a_chip() {
        let mut list = PrListState::new(50);
        list.sort = PrSort::UpdatedDesc;
        assert_eq!(list.chips(), vec!["is:open", "sort:recently updated"]);
    }

    #[test]
    fn the_query_carries_the_filters_the_state_the_sort_and_the_limit() {
        let mut list = PrListState::new(25);
        list.push_filter(Filter::Author("alice".to_owned()));
        list.state_filter = PrStateFilter::Merged;
        list.sort = PrSort::CreatedAsc;

        let query = list.query();
        assert_eq!(query.limit, 25);
        assert_eq!(query.state, PrStateFilter::Merged);
        assert_eq!(query.sort, PrSort::CreatedAsc);
        assert_eq!(query.filters, vec![Filter::Author("alice".to_owned())]);
        assert_eq!(
            query.server_search(),
            "is:merged author:alice sort:created-asc"
        );
    }

    #[test]
    fn paging_uses_the_height_of_the_last_frame() {
        let mut list = list_of(&(1..=30).rev().collect::<Vec<u64>>());
        list.viewport = 10;

        list.move_page(1, true);
        assert_eq!(list.cursor_position(), Some(5), "half a screen down");
        list.move_page(1, false);
        assert_eq!(list.cursor_position(), Some(15), "a whole screen down");
        list.move_page(-1, false);
        assert_eq!(list.cursor_position(), Some(5));
        list.move_page(-1, true);
        assert_eq!(list.cursor_position(), Some(0), "and it stops at the top");
    }

    #[test]
    fn a_page_before_any_frame_has_been_drawn_still_moves() {
        let mut list = list_of(&[3, 2, 1]);
        assert_eq!(list.viewport, 0, "nothing drawn yet");
        list.move_page(1, false);
        assert_eq!(list.selected().unwrap().number, 2);
    }
}
