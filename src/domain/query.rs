//! PR queries: the structured filters behind `gh --search`, and the honest
//! rejection of anything we cannot express (FR-2.2).
//!
//! Two mechanisms, deliberately kept apart:
//!
//! - the [`PrQuery`] here is *server side*: it becomes a `gh pr list --search`
//!   string, and anything that would not be understood is refused rather than
//!   sent to GitHub, because a query GitHub silently ignores looks like a bug in
//!   the app;
//! - the `/` search box is *client side* and fuzzy over the cached list
//!   ([`PullRequestSummary::matches`](crate::domain::pr::PullRequestSummary::matches)),
//!   so filtering 300 cached PRs costs no round trip.
//!
//! The state filter is a field rather than a chip in the list so that there is a
//! single source of truth for `is:open`, `--state` and the header. It is still
//! presented and removed as a chip.

use serde::{Deserialize, Serialize};

/// Which PRs the list asks GitHub for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrStateFilter {
    /// Open PRs, the default.
    #[default]
    Open,
    /// Closed without merging.
    Closed,
    /// Merged.
    Merged,
    /// Everything.
    All,
}

impl PrStateFilter {
    /// The value for `gh --state`.
    #[must_use]
    pub fn as_gh_state(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Merged => "merged",
            Self::All => "all",
        }
    }

    /// The GitHub search qualifier, or `None` for "all", which has no qualifier.
    #[must_use]
    pub fn qualifier(self) -> Option<&'static str> {
        match self {
            Self::Open => Some("is:open"),
            Self::Closed => Some("is:closed"),
            Self::Merged => Some("is:merged"),
            Self::All => None,
        }
    }

    /// Parses `:filter is:...`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            "merged" => Some(Self::Merged),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    /// A label for the header and the chip.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Merged => "merged",
            Self::All => "all",
        }
    }
}

/// The review-state filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewFilter {
    /// Waiting on a reviewer.
    Required,
    /// Approved.
    Approved,
    /// A reviewer asked for changes.
    ChangesRequested,
    /// Nobody is assigned.
    None,
}

impl ReviewFilter {
    /// The GitHub search qualifier.
    #[must_use]
    pub fn qualifier(self) -> &'static str {
        match self {
            Self::Required => "review:required",
            Self::Approved => "review:approved",
            Self::ChangesRequested => "review:changes_requested",
            Self::None => "review:none",
        }
    }

    /// Parses `review:...`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "required" => Some(Self::Required),
            "approved" => Some(Self::Approved),
            "changes_requested" | "changes-requested" => Some(Self::ChangesRequested),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// A label for the chip.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Approved => "approved",
            Self::ChangesRequested => "changes requested",
            Self::None => "none",
        }
    }
}

/// The order GitHub returns PRs in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrSort {
    /// Newest first, the default (FR-2.1).
    #[default]
    CreatedDesc,
    /// Oldest first.
    CreatedAsc,
    /// Most recently updated first.
    UpdatedDesc,
    /// Least recently updated first.
    UpdatedAsc,
}

impl PrSort {
    /// The GitHub search qualifier.
    #[must_use]
    pub fn qualifier(self) -> &'static str {
        match self {
            Self::CreatedDesc => "sort:created-desc",
            Self::CreatedAsc => "sort:created-asc",
            Self::UpdatedDesc => "sort:updated-desc",
            Self::UpdatedAsc => "sort:updated-asc",
        }
    }

    /// Parses `:sort <field> [asc|desc]`.
    #[must_use]
    pub fn parse(field: &str, ascending: bool) -> Option<Self> {
        match (field.trim().to_ascii_lowercase().as_str(), ascending) {
            ("created", false) => Some(Self::CreatedDesc),
            ("created", true) => Some(Self::CreatedAsc),
            ("updated", false) => Some(Self::UpdatedDesc),
            ("updated", true) => Some(Self::UpdatedAsc),
            _ => None,
        }
    }

    /// A label for the chip and the status line.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::CreatedDesc => "newest",
            Self::CreatedAsc => "oldest",
            Self::UpdatedDesc => "recently updated",
            Self::UpdatedAsc => "least recently updated",
        }
    }
}

/// One structured filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    /// Free text matched against titles (`in:title`).
    Title(String),
    /// Free text matched against bodies (`in:body`).
    Body(String),
    /// `author:LOGIN`.
    Author(String),
    /// `#N`, resolved by fetching that PR directly rather than by searching.
    Number(u64),
    /// `label:NAME`.
    Label(String),
    /// `base:BRANCH`.
    Base(String),
    /// `draft:true|false`.
    Draft(bool),
    /// `review:...`.
    Review(ReviewFilter),
}

/// Why a filter line was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilterError {
    #[error("unknown filter '{0}:'")]
    UnknownQualifier(String),

    #[error("'{0}:' needs a value")]
    MissingValue(String),

    #[error("'{value}' is not a valid value for '{name}:' (expected {expected})")]
    InvalidValue {
        name: String,
        value: String,
        expected: &'static str,
    },

    #[error("'{0}' is not a number")]
    InvalidNumber(String),
}

/// How a filter's value is quoted for GitHub.
///
/// A value containing a space must be quoted or GitHub applies the qualifier to
/// the first word only, which silently returns the wrong PRs.
fn quoted(value: &str) -> String {
    if value.contains(' ') || value.contains('"') {
        format!("\"{}\"", value.replace('"', ""))
    } else {
        value.to_owned()
    }
}

impl Filter {
    /// Parses one chip or `:filter` argument.
    ///
    /// Accepts the GitHub qualifiers a reviewer actually types, plus the
    /// `title:`/`body:` shorthands for `in:title`/`in:body`.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] for an unknown qualifier, a missing value, or a
    /// value outside the accepted set — never silently ignoring the input, so a
    /// typo cannot look like an empty result (FR-2.2).
    pub fn parse_line(line: &str) -> Result<Self, FilterError> {
        let line = line.trim();
        if let Some(number) = line.strip_prefix('#') {
            return number
                .parse::<u64>()
                .map(Self::Number)
                .map_err(|_| FilterError::InvalidNumber(line.to_owned()));
        }
        if let Ok(number) = line.parse::<u64>() {
            return Ok(Self::Number(number));
        }

        let (name, value) = line
            .split_once(':')
            .map_or((line, ""), |(name, value)| (name.trim(), value.trim()));
        let name = name.to_ascii_lowercase();

        match name.as_str() {
            // Free text: an empty value is meaningless, so refuse it rather than
            // searching for everything.
            "in" => Self::in_qualifier(value),
            "title" => require_value("title", value).map(Self::Title),
            "body" => require_value("body", value).map(Self::Body),
            "author" | "user" => require_value(&name, value).map(Self::Author),
            "label" => require_value("label", value).map(Self::Label),
            "base" => require_value("base", value).map(Self::Base),
            "draft" => match value.to_ascii_lowercase().as_str() {
                "true" => Ok(Self::Draft(true)),
                "false" => Ok(Self::Draft(false)),
                other => Err(FilterError::InvalidValue {
                    name: "draft".to_owned(),
                    value: other.to_owned(),
                    expected: "true or false",
                }),
            },
            "review" => ReviewFilter::parse(value).map(Self::Review).ok_or({
                FilterError::InvalidValue {
                    name: "review".to_owned(),
                    value: value.to_owned(),
                    expected: "required, approved, changes_requested or none",
                }
            }),
            other => Err(FilterError::UnknownQualifier(other.to_owned())),
        }
    }

    /// Splits `in:title free text` into its parts.
    fn in_qualifier(value: &str) -> Result<Self, FilterError> {
        let (field, text) = value
            .split_once(char::is_whitespace)
            .map_or((value, ""), |(field, text)| (field, text.trim()));
        match field.to_ascii_lowercase().as_str() {
            "title" => require_value("in:title", text).map(Self::Title),
            "body" => require_value("in:body", text).map(Self::Body),
            other => Err(FilterError::InvalidValue {
                name: "in".to_owned(),
                value: other.to_owned(),
                expected: "in:title or in:body",
            }),
        }
    }

    /// The chip text, which is what the filter bar shows.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Title(text) => format!("in:title {text}"),
            Self::Body(text) => format!("in:body {text}"),
            Self::Author(author) => format!("author:{author}"),
            Self::Number(number) => format!("#{number}"),
            Self::Label(label) => format!("label:{}", quoted(label)),
            Self::Base(base) => format!("base:{base}"),
            Self::Draft(true) => "draft:true".to_owned(),
            Self::Draft(false) => "draft:false".to_owned(),
            Self::Review(review) => review.qualifier().to_owned(),
        }
    }

    /// The GitHub search qualifier this filter contributes.
    ///
    /// [`Filter::Number`] contributes nothing: it is resolved by fetching the PR
    /// directly, not by searching for it (FR-2.2).
    #[must_use]
    pub fn qualifier(&self) -> Option<String> {
        Some(match self {
            Self::Title(text) => format!("in:title {}", quoted(text)),
            Self::Body(text) => format!("in:body {}", quoted(text)),
            Self::Author(author) => format!("author:{}", quoted(author)),
            Self::Number(_) => return None,
            Self::Label(label) => format!("label:{}", quoted(label)),
            Self::Base(base) => format!("base:{}", quoted(base)),
            Self::Draft(true) => "draft:true".to_owned(),
            Self::Draft(false) => "draft:false".to_owned(),
            Self::Review(review) => review.qualifier().to_owned(),
        })
    }
}

/// Rejects an empty value, naming the qualifier that needed one.
fn require_value(qualifier: &str, value: &str) -> Result<String, FilterError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(FilterError::MissingValue(qualifier.to_owned()));
    }
    Ok(value.to_owned())
}

/// What the list is currently showing, and everything needed to reproduce it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrQuery {
    /// Which PRs to include by state.
    pub state: PrStateFilter,
    /// The structured filters, in the order the user added them.
    pub filters: Vec<Filter>,
    /// The order to ask for.
    pub sort: PrSort,
    /// How many to ask for; `:load-more` raises it up to the configured cap.
    pub limit: u32,
}

impl Default for PrQuery {
    fn default() -> Self {
        Self {
            state: PrStateFilter::default(),
            filters: Vec::new(),
            sort: PrSort::default(),
            limit: 50,
        }
    }
}

impl PrQuery {
    /// A query with an explicit page size.
    #[must_use]
    pub fn with_limit(limit: u32) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }

    /// Adds a filter.
    ///
    /// A state chip is folded into [`Self::state`] rather than kept as a separate
    /// filter, so `is:closed` and the header can never disagree. A second
    /// [`Filter::Number`] replaces the first, because a query for two PRs is not a
    /// list.
    pub fn push(&mut self, filter: Filter) {
        match filter {
            Filter::Number(number) => {
                self.filters
                    .retain(|existing| !matches!(existing, Filter::Number(_)));
                self.filters.push(Filter::Number(number));
            }
            other => {
                if !self.filters.contains(&other) {
                    self.filters.push(other);
                }
            }
        }
    }

    /// Removes the filter at `index`, if there is one.
    pub fn remove(&mut self, index: usize) {
        if index < self.filters.len() {
            self.filters.remove(index);
        }
    }

    /// Forgets every filter and goes back to open PRs.
    pub fn clear(&mut self) {
        self.filters.clear();
        self.state = PrStateFilter::Open;
        self.sort = PrSort::default();
    }

    /// Whether anything narrows the query.
    #[must_use]
    pub fn is_filtered(&self) -> bool {
        !self.filters.is_empty() || self.state != PrStateFilter::Open
    }

    /// The PR number to open directly, when the query names exactly one.
    #[must_use]
    pub fn number(&self) -> Option<u64> {
        self.filters.iter().find_map(|filter| match filter {
            Filter::Number(number) => Some(*number),
            _ => None,
        })
    }

    /// Sets the state filter.
    pub fn set_state(&mut self, state: PrStateFilter) {
        self.state = state;
    }

    /// The `--search` string, always including an explicit sort so the order does
    /// not depend on a server default (FR-2.1).
    #[must_use]
    pub fn server_search(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(qualifier) = self.state.qualifier() {
            parts.push(qualifier.to_owned());
        }
        for filter in &self.filters {
            if let Some(qualifier) = filter.qualifier() {
                parts.push(qualifier);
            }
        }
        parts.push(self.sort.qualifier().to_owned());
        parts.join(" ")
    }

    /// A stable string identifying this query, used as the cache key (FR-2.3).
    ///
    /// It must change whenever the *results* would change and never because of
    /// incidental state, so the parts are written in a fixed order with their
    /// labels rather than as a debug dump of the struct.
    #[must_use]
    pub fn canonical(&self) -> String {
        let mut parts = vec![
            format!("state={}", self.state.label()),
            format!("sort={}", self.sort.label()),
        ];
        for filter in &self.filters {
            parts.push(format!("f={}", filter.label()));
        }
        parts.push(format!("limit={}", self.limit));
        parts.join(";")
    }

    /// The same query for a different page size, which is what `:load-more` does.
    #[must_use]
    pub fn with_more(&self, limit: u32) -> Self {
        Self {
            limit,
            ..self.clone()
        }
    }

    /// A query for everything updated since `since`, used by the cache to decide
    /// whether its copy can still be trusted.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(qualifier) = self.state.qualifier() {
            parts.push(qualifier.to_owned());
        }
        for filter in &self.filters {
            if let Some(qualifier) = filter.qualifier() {
                parts.push(qualifier);
            }
        }
        if parts.is_empty() {
            return "all pull requests".to_owned();
        }
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_qualifier_parses() {
        let cases = [
            (
                "in:title retry webhook",
                Filter::Title("retry webhook".into()),
            ),
            ("in:body migration", Filter::Body("migration".into())),
            ("title:retry", Filter::Title("retry".into())),
            ("author:alice", Filter::Author("alice".into())),
            ("user:alice", Filter::Author("alice".into())),
            ("label:bug", Filter::Label("bug".into())),
            ("base:main", Filter::Base("main".into())),
            ("draft:true", Filter::Draft(true)),
            ("draft:false", Filter::Draft(false)),
            ("review:required", Filter::Review(ReviewFilter::Required)),
            ("review:approved", Filter::Review(ReviewFilter::Approved)),
            (
                "review:changes_requested",
                Filter::Review(ReviewFilter::ChangesRequested),
            ),
            ("#141", Filter::Number(141)),
            ("141", Filter::Number(141)),
        ];
        for (input, expected) in cases {
            assert_eq!(Filter::parse_line(input).unwrap(), expected, "{input}");
        }
    }

    #[test]
    fn an_unknown_qualifier_is_refused_with_its_name() {
        let error = Filter::parse_line("foo:bar").unwrap_err();
        assert_eq!(error, FilterError::UnknownQualifier("foo".to_owned()));
        assert!(error.to_string().contains("foo:"), "{error}");
    }

    #[test]
    fn a_qualifier_without_a_value_is_refused() {
        for input in ["author:", "author:   ", "label:", "in:title", "body:"] {
            let error = Filter::parse_line(input).unwrap_err();
            assert!(
                matches!(error, FilterError::MissingValue(_)),
                "{input} gave {error:?}"
            );
        }
    }

    #[test]
    fn a_bad_enumeration_value_lists_what_was_expected() {
        let error = Filter::parse_line("review:maybe").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("changes_requested"), "{text}");
        assert!(text.contains("maybe"), "{text}");

        let error = Filter::parse_line("draft:yes").unwrap_err();
        assert!(error.to_string().contains("true or false"), "{error}");

        let error = Filter::parse_line("in:comments x").unwrap_err();
        assert!(error.to_string().contains("in:title"), "{error}");

        let error = Filter::parse_line("#abc").unwrap_err();
        assert!(matches!(error, FilterError::InvalidNumber(_)), "{error:?}");
    }

    #[test]
    fn qualifiers_with_spaces_are_quoted_for_github() {
        // Without quoting, GitHub applies `in:title` to `retry` only and returns
        // PRs whose title says "retry" but whose body says "webhook".
        let filter = Filter::parse_line("in:title retry webhook").unwrap();
        assert_eq!(
            filter.qualifier().unwrap(),
            "in:title \"retry webhook\"",
            "a multi-word value must stay one term"
        );

        let filter = Filter::parse_line("label:needs review").unwrap();
        assert_eq!(filter.qualifier().unwrap(), "label:\"needs review\"");
    }

    #[test]
    fn a_number_filter_is_resolved_directly_not_searched() {
        let filter = Filter::parse_line("#141").unwrap();
        assert_eq!(filter.qualifier(), None, "there is nothing to search for");
        assert_eq!(filter.label(), "#141");
    }

    #[test]
    fn the_server_search_always_states_the_order() {
        let query = PrQuery::default();
        assert_eq!(query.server_search(), "is:open sort:created-desc");

        let mut query = PrQuery::default();
        query.push(Filter::parse_line("author:alice").unwrap());
        query.push(Filter::parse_line("label:bug").unwrap());
        assert_eq!(
            query.server_search(),
            "is:open author:alice label:bug sort:created-desc"
        );

        // "all" has no qualifier at all.
        let mut query = PrQuery::default();
        query.set_state(PrStateFilter::All);
        assert_eq!(query.server_search(), "sort:created-desc");
    }

    #[test]
    fn adding_the_same_filter_twice_changes_nothing() {
        let mut query = PrQuery::default();
        query.push(Filter::Author("alice".into()));
        query.push(Filter::Author("alice".into()));
        assert_eq!(query.filters.len(), 1);
    }

    #[test]
    fn the_last_number_filter_wins() {
        let mut query = PrQuery::default();
        query.push(Filter::Number(1));
        query.push(Filter::Number(2));
        assert_eq!(query.filters.len(), 1);
        assert_eq!(query.number(), Some(2));
    }

    #[test]
    fn clearing_goes_back_to_open_and_the_default_order() {
        let mut query = PrQuery::default();
        query.push(Filter::Author("alice".into()));
        query.set_state(PrStateFilter::Merged);
        query.sort = PrSort::UpdatedAsc;
        assert!(query.is_filtered());

        query.clear();
        assert!(!query.is_filtered());
        assert_eq!(query.state, PrStateFilter::Open);
        assert_eq!(query.sort, PrSort::CreatedDesc);
    }

    #[test]
    fn removing_a_filter_only_removes_that_one() {
        let mut query = PrQuery::default();
        query.push(Filter::Author("alice".into()));
        query.push(Filter::Label("bug".into()));
        query.remove(0);
        assert_eq!(query.filters, vec![Filter::Label("bug".into())]);

        query.remove(9);
        assert_eq!(query.filters.len(), 1, "an out-of-range removal is a no-op");
    }

    #[test]
    fn the_canonical_form_separates_queries_that_differ() {
        let base = PrQuery::default().canonical();
        assert!(base.contains("limit=50"), "{base}");

        let mut other = PrQuery::default();
        other.push(Filter::Author("alice".into()));
        assert_ne!(base, other.canonical());

        // The page size is part of the key: 50 PRs cached under a 100-PR key
        // would show a truncated list as if it were complete.
        assert_ne!(base, PrQuery::default().with_more(100).canonical());

        // Same content, same key, repeatedly.
        assert_eq!(other.canonical(), other.canonical());
    }

    #[test]
    fn loading_more_keeps_every_other_part_of_the_query() {
        let mut query = PrQuery::default();
        query.push(Filter::Author("alice".into()));
        let larger = query.with_more(200);
        assert_eq!(larger.limit, 200);
        assert_eq!(larger.filters, query.filters);
        assert_eq!(larger.state, query.state);
    }

    #[test]
    fn state_filters_round_trip_through_their_labels() {
        for state in [
            PrStateFilter::Open,
            PrStateFilter::Closed,
            PrStateFilter::Merged,
            PrStateFilter::All,
        ] {
            assert_eq!(PrStateFilter::parse(state.label()), Some(state));
            assert_eq!(PrStateFilter::parse(state.as_gh_state()), Some(state));
        }
    }

    #[test]
    fn sorts_round_trip_and_reject_nonsense() {
        for sort in [
            PrSort::CreatedDesc,
            PrSort::CreatedAsc,
            PrSort::UpdatedDesc,
            PrSort::UpdatedAsc,
        ] {
            assert!(sort.qualifier().starts_with("sort:"));
        }
        assert_eq!(PrSort::parse("created", false), Some(PrSort::CreatedDesc));
        assert_eq!(PrSort::parse("UPDATED", true), Some(PrSort::UpdatedAsc));
        assert_eq!(PrSort::parse("comments", false), None);
    }

    #[test]
    fn a_description_of_the_query_is_readable() {
        let mut query = PrQuery::default();
        assert_eq!(query.describe(), "is:open");
        query.push(Filter::Author("alice".into()));
        assert_eq!(query.describe(), "is:open author:alice");
    }
}
