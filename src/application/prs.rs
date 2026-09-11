//! Listing, opening and reading pull requests (FR-2.1, FR-2.3, FR-2.4, FR-3.2).
//!
//! Three things happen here that are worth naming, because they are the difference
//! between an app that feels instant and one that does not:
//!
//! - **cache first**: [`Prs::cached_list`] answers from disk with no network, so
//!   the first frame can already show the list (FR-2.3);
//! - **revalidate in place**: [`Prs::load_list`] replaces that list when the
//!   network answers, and the caller keeps the cursor where it was because the list
//!   is keyed by PR number rather than by row;
//! - **degrade, never fail**: when the network is gone but a cached answer exists,
//!   the answer is returned as [`FetchOutcome::Offline`] so the UI can say so
//!   instead of showing an empty screen (DEC-14).

use crate::domain::diff::{DiffSource, Patch};
use crate::domain::pr::PullRequestDetail;
use crate::domain::query::PrQuery;
use crate::domain::repo::RepoId;
use crate::error::Result;
use crate::ports::cache::{CacheKey, CacheStore};
use crate::ports::forge::{ForgePort, PullRequestPage};
use crate::ports::workspace::DiffOptions;
use crate::ports::{Cancel, Clock};

/// How long a cached answer may be trusted (FR-2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePolicy {
    /// PR lists change often: a minute.
    pub list_ttl_secs: u64,
    /// A PR's detail changes when someone pushes or reviews: five minutes.
    pub detail_ttl_secs: u64,
    /// A diff only changes when the head commit does, and the head SHA is part of
    /// the key, so this TTL is about disk hygiene rather than freshness.
    pub diff_ttl_secs: u64,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            list_ttl_secs: 60,
            detail_ttl_secs: 300,
            diff_ttl_secs: 24 * 60 * 60,
        }
    }
}

/// A cached answer together with how old it is.
#[derive(Debug, Clone)]
pub struct Cached<T> {
    /// The value.
    pub value: T,
    /// Its age in seconds.
    pub age_secs: u64,
    /// Whether the age exceeds the policy's TTL.
    pub stale: bool,
}

/// What a fetch produced.
#[derive(Debug, Clone)]
pub enum FetchOutcome<T> {
    /// The forge answered.
    Fresh(T),
    /// The forge did not answer, so a cached answer is being shown instead.
    Offline {
        /// The cached value.
        value: T,
        /// Why the fresh attempt failed, for the offline indicator.
        reason: String,
    },
}

impl<T> FetchOutcome<T> {
    /// The value, whichever way it arrived.
    #[must_use]
    pub fn value(&self) -> &T {
        match self {
            Self::Fresh(value) | Self::Offline { value, .. } => value,
        }
    }

    /// Consumes the outcome and returns the value.
    #[must_use]
    pub fn into_value(self) -> T {
        match self {
            Self::Fresh(value) | Self::Offline { value, .. } => value,
        }
    }

    /// Why the value is stale, when it is.
    #[must_use]
    pub fn offline_reason(&self) -> Option<&str> {
        match self {
            Self::Fresh(_) => None,
            Self::Offline { reason, .. } => Some(reason),
        }
    }
}

/// The key a list query is cached under.
///
/// The query's canonical form is hashed rather than embedded: the search string can
/// contain spaces and quotes, and a cache key becomes a file path (FR-1.3).
///
/// # Errors
///
/// Returns an error only if the key cannot be formed, which would be a bug.
pub fn list_key(repo: &RepoId, query: &PrQuery) -> Result<CacheKey> {
    CacheKey::new(format!(
        "{}/list/{:016x}.json",
        repo.key(),
        fnv1a(query.canonical().as_bytes())
    ))
    .map_err(|error| crate::Error::cache(error.to_string()))
}

/// The key a PR's detail is cached under.
///
/// # Errors
///
/// As [`list_key`].
pub fn detail_key(repo: &RepoId, number: u64) -> Result<CacheKey> {
    key(repo, number, "detail.json")
}

/// The key a PR's diff is cached under.
///
/// # Errors
///
/// As [`list_key`].
pub fn diff_key(repo: &RepoId, number: u64, head_sha: &str) -> Result<CacheKey> {
    diff_key_with(
        repo,
        number,
        head_sha,
        DiffOptions::default(),
        DiffSource::Forge,
    )
}

/// The cache key for a diff produced with specific flags (FR-3.2).
///
/// The flags are part of the key because they change the bytes: serving a
/// whitespace-ignored diff to a request that did not ask for one would look like a
/// diff that is missing changes. The default options keep the key they had before
/// the flags existed, so a cache written by an older build is still read.
///
/// # Errors
///
/// Returns an error when the key cannot be built, which means the repository
/// identity is not usable as a path.
pub fn diff_key_with(
    repo: &RepoId,
    number: u64,
    head_sha: &str,
    options: DiffOptions,
    source: DiffSource,
) -> Result<CacheKey> {
    let short: String = head_sha.chars().take(12).collect();
    let flags = options_suffix(options);
    let tag = source.cache_tag();
    key(
        repo,
        number,
        &format!("diff-{short}{flags}{tag}.patch.json"),
    )
}

/// The part of a diff cache key that describes non-default flags.
fn options_suffix(options: DiffOptions) -> String {
    let mut flags = String::new();
    if options.context != DiffOptions::default().context {
        let _ = std::fmt::Write::write_fmt(&mut flags, format_args!("-c{}", options.context));
    }
    if options.ignore_whitespace {
        flags.push_str("-w");
    }
    if !options.find_renames {
        flags.push_str("-norenames");
    }
    flags
}

fn key(repo: &RepoId, number: u64, name: &str) -> Result<CacheKey> {
    CacheKey::new(format!("{}/pr-{number}/{name}", repo.key()))
        .map_err(|error| crate::Error::cache(error.to_string()))
}

/// FNV-1a, which is stable across runs and platforms — unlike `DefaultHasher`,
/// whose output is explicitly not guaranteed to be the same between releases.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The collaborators the pull-request use cases need.
///
/// Grouped rather than passed one by one: the event loop holds all of them for its
/// whole life, and a five-argument call repeated at every site is how argument
/// order mistakes happen.
#[derive(Debug)]
pub struct Prs<'a> {
    /// The forge to read from.
    pub forge: &'a dyn ForgePort,
    /// Where answers are cached.
    pub cache: &'a dyn CacheStore,
    /// The clock, which decides what is stale.
    pub clock: &'a dyn Clock,
    /// The repository every call is scoped to.
    pub repo: &'a RepoId,
    /// How long cached answers may be trusted.
    pub policy: CachePolicy,
}

impl<'a> Prs<'a> {
    /// Binds the use cases to a repository and its collaborators.
    #[must_use]
    pub fn new(
        forge: &'a dyn ForgePort,
        cache: &'a dyn CacheStore,
        clock: &'a dyn Clock,
        repo: &'a RepoId,
    ) -> Self {
        Self {
            forge,
            cache,
            clock,
            repo,
            policy: CachePolicy::default(),
        }
    }

    /// Overrides the cache policy.
    #[must_use]
    pub fn with_policy(mut self, policy: CachePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Reads a cached PR list, without touching the network (FR-2.3).
    ///
    /// This is the call that makes the first frame instant: it is one small file
    /// read, and the answer can be painted before `gh` has finished starting.
    ///
    /// # Errors
    ///
    /// Returns an error when the cache cannot be read, which the caller reports
    /// but does not treat as fatal: an unreadable cache only costs a refetch.
    pub fn cached_list(&self, query: &PrQuery) -> Result<Option<Cached<PullRequestPage>>> {
        read_cached(
            self.cache,
            self.clock,
            &list_key(self.repo, query)?,
            self.policy.list_ttl_secs,
        )
    }

    /// Reads a cached PR detail (FR-2.3).
    ///
    /// # Errors
    ///
    /// As [`Self::cached_list`].
    pub fn cached_detail(&self, number: u64) -> Result<Option<Cached<PullRequestDetail>>> {
        read_cached(
            self.cache,
            self.clock,
            &detail_key(self.repo, number)?,
            self.policy.detail_ttl_secs,
        )
    }

    /// Reads a cached diff (FR-3.2).
    ///
    /// # Errors
    ///
    /// As [`Self::cached_list`].
    pub fn cached_patch(&self, number: u64, head_sha: &str) -> Result<Option<Cached<Patch>>> {
        self.cached_patch_with(number, head_sha, DiffOptions::default(), DiffSource::Forge)
    }

    /// The same, for a specific set of flags and a specific source (FR-3.2).
    ///
    /// # Errors
    ///
    /// Returns an error when the cache cannot be read.
    pub fn cached_patch_with(
        &self,
        number: u64,
        head_sha: &str,
        options: DiffOptions,
        source: DiffSource,
    ) -> Result<Option<Cached<Patch>>> {
        read_cached(
            self.cache,
            self.clock,
            &diff_key_with(self.repo, number, head_sha, options, source)?,
            self.policy.diff_ttl_secs,
        )
    }

    /// Stores a diff that was produced locally (FR-3.2).
    ///
    ///
    /// The local worktree and the forge produce the same shape, so both are cached
    /// under the same key: whichever source answered, the next open is instant.
    ///
    /// # Errors
    ///
    /// Returns an error when the cache cannot be written.
    pub fn store_local_patch(
        &self,
        number: u64,
        head_sha: &str,
        options: DiffOptions,
        patch: &Patch,
    ) -> Result<()> {
        let key = diff_key_with(self.repo, number, head_sha, options, DiffSource::Worktree)?;
        let body = serde_json::to_string(patch).map_err(|error| {
            crate::error::Error::Cache(format!("could not encode the diff: {error}"))
        })?;
        self.cache.write(&key, &body, self.clock.now_unix_secs())
    }

    /// The key a diff with these flags is cached under, for callers that read the
    /// cache themselves.
    ///
    /// # Errors
    ///
    /// Returns an error when the key cannot be built.
    pub fn patch_key(&self, number: u64, head_sha: &str, options: DiffOptions) -> Result<CacheKey> {
        diff_key_with(self.repo, number, head_sha, options, DiffSource::Forge)
    }

    /// Fetches the PR list, falling back to the cache when the forge is
    /// unreachable (FR-2.3, DEC-14).
    ///
    /// # Errors
    ///
    /// Returns an error when the forge fails *and* there is nothing cached.
    pub fn load_list(
        &self,
        query: &PrQuery,
        cancel: &Cancel,
    ) -> Result<FetchOutcome<PullRequestPage>> {
        match self.forge.list_pull_requests(query, cancel) {
            Ok(page) => {
                self.store(&list_key(self.repo, query), &page);
                Ok(FetchOutcome::Fresh(page))
            }
            Err(error) => match self.cached_list(query)? {
                Some(cached) => Ok(FetchOutcome::Offline {
                    value: cached.value,
                    reason: error.to_string(),
                }),
                None => Err(error),
            },
        }
    }

    /// Counts the PRs matching a query, without falling back to anything.
    ///
    /// # Errors
    ///
    /// Returns an error when the forge cannot answer. A missing count is not fatal:
    /// the list says "showing 50 of ≥50", which is what it says before the count
    /// arrives anyway.
    pub fn count(&self, query: &PrQuery, cancel: &Cancel) -> Result<u32> {
        self.forge.count_pull_requests(query, cancel)
    }

    /// Fetches one PR in full, falling back to the cache (FR-2.4).
    ///
    /// # Errors
    ///
    /// Returns an error when the forge fails *and* there is nothing cached.
    pub fn load_detail(
        &self,
        number: u64,
        cancel: &Cancel,
    ) -> Result<FetchOutcome<PullRequestDetail>> {
        match self.forge.get_pull_request(number, cancel) {
            Ok(detail) => {
                self.store(&detail_key(self.repo, number), &detail);
                Ok(FetchOutcome::Fresh(detail))
            }
            Err(error) => match self.cached_detail(number)? {
                Some(cached) => Ok(FetchOutcome::Offline {
                    value: cached.value,
                    reason: error.to_string(),
                }),
                None => Err(error),
            },
        }
    }

    /// Fetches and parses a PR's diff (FR-3.2).
    ///
    /// The parsed patch is what gets cached: parsing 10 000 lines is fast but not
    /// free, and the parse is the part that can change between versions, so a
    /// payload that no longer decodes is discarded rather than trusted.
    ///
    /// # Errors
    ///
    /// Returns an error when the forge fails *and* there is nothing cached.
    pub fn load_patch(
        &self,
        number: u64,
        head_sha: &str,
        cancel: &Cancel,
    ) -> Result<FetchOutcome<Patch>> {
        match self.forge.pull_request_diff(number, cancel) {
            Ok(text) => {
                let patch = crate::domain::diff::parse_patch(&text);
                self.store(&diff_key(self.repo, number, head_sha), &patch);
                Ok(FetchOutcome::Fresh(patch))
            }
            Err(error) => match self.cached_patch(number, head_sha)? {
                Some(cached) => Ok(FetchOutcome::Offline {
                    value: cached.value,
                    reason: error.to_string(),
                }),
                None => Err(error),
            },
        }
    }

    /// Writes an entry, ignoring a failure.
    ///
    /// A cache that cannot be written is a slower app, not a broken one, so a
    /// successful fetch is never turned into an error by it.
    fn store<T: serde::Serialize>(&self, key: &Result<CacheKey>, value: &T) {
        let Ok(key) = key else {
            return;
        };
        let Ok(body) = serde_json::to_string(value) else {
            return;
        };
        let _ = self.cache.write(key, &body, self.clock.now_unix_secs());
    }
}

/// Decodes one cached entry, treating an undecodable one as absent.
fn read_cached<T: serde::de::DeserializeOwned>(
    cache: &dyn CacheStore,
    clock: &dyn Clock,
    key: &CacheKey,
    ttl_secs: u64,
) -> Result<Option<Cached<T>>> {
    let Some(stored) = cache.read(key)? else {
        return Ok(None);
    };
    let now = clock.now_unix_secs();
    if let Ok(value) = serde_json::from_str::<T>(&stored.body) {
        return Ok(Some(Cached {
            value,
            age_secs: stored.age_secs(now),
            stale: stored.is_stale(now, ttl_secs),
        }));
    }

    // A payload that no longer deserializes means the shape changed between
    // versions: treat it as a miss, and remove it so it is not decoded again.
    cache.remove(key)?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pr::PullRequestSummary;
    use crate::domain::pr::{CheckRun, CheckSummary, PrState, Review, ReviewComment};
    use crate::domain::query::Filter;
    use crate::ports::forge::ForgeCapabilities;
    use crate::test_support::InMemoryCache;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug, Default)]
    struct FakeClock(AtomicU64);

    impl FakeClock {
        fn at(seconds: u64) -> Self {
            Self(AtomicU64::new(seconds))
        }

        fn set(&self, seconds: u64) {
            self.0.store(seconds, Ordering::SeqCst);
        }
    }

    impl Clock for FakeClock {
        fn now_unix_secs(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// A forge that answers with whatever the test put in it, or fails.
    #[derive(Debug, Default)]
    struct FakeForge {
        page: Mutex<Option<PullRequestPage>>,
        detail: Mutex<Option<PullRequestDetail>>,
        diff: Mutex<Option<String>>,
        count: Mutex<Option<u32>>,
        fail: Mutex<Option<String>>,
        calls: AtomicU64,
    }

    impl FakeForge {
        fn failing(reason: &str) -> Self {
            let forge = Self::default();
            *forge.fail.lock().unwrap() = Some(reason.to_owned());
            forge
        }

        fn with_page(page: PullRequestPage) -> Self {
            let forge = Self::default();
            *forge.page.lock().unwrap() = Some(page);
            forge
        }

        fn maybe_fail(&self) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.fail.lock().unwrap().clone() {
                Some(reason) => Err(crate::Error::forge("gh", reason)),
                None => Ok(()),
            }
        }
    }

    impl ForgePort for FakeForge {
        fn capabilities(&self) -> ForgeCapabilities {
            ForgeCapabilities::default()
        }

        fn list_pull_requests(
            &self,
            _query: &PrQuery,
            _cancel: &Cancel,
        ) -> Result<PullRequestPage> {
            self.maybe_fail()?;
            self.page
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| crate::Error::forge("gh", "no page configured"))
        }

        fn count_pull_requests(&self, _query: &PrQuery, _cancel: &Cancel) -> Result<u32> {
            self.maybe_fail()?;
            self.count
                .lock()
                .unwrap()
                .ok_or_else(|| crate::Error::forge("gh", "no count configured"))
        }

        fn get_pull_request(&self, _number: u64, _cancel: &Cancel) -> Result<PullRequestDetail> {
            self.maybe_fail()?;
            self.detail
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| crate::Error::forge("gh", "no detail configured"))
        }

        fn list_reviews(&self, _number: u64, _cancel: &Cancel) -> Result<Vec<Review>> {
            self.maybe_fail()?;
            Ok(Vec::new())
        }

        fn list_review_comments(
            &self,
            _number: u64,
            _cancel: &Cancel,
        ) -> Result<Vec<ReviewComment>> {
            self.maybe_fail()?;
            Ok(Vec::new())
        }

        fn list_checks(&self, _number: u64, _cancel: &Cancel) -> Result<Vec<CheckRun>> {
            self.maybe_fail()?;
            Ok(Vec::new())
        }

        fn pull_request_diff(&self, _number: u64, _cancel: &Cancel) -> Result<String> {
            self.maybe_fail()?;
            self.diff
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| crate::Error::forge("gh", "no diff configured"))
        }
    }

    fn summary(number: u64, title: &str) -> PullRequestSummary {
        PullRequestSummary {
            number,
            title: title.to_owned(),
            author: "alice".to_owned(),
            state: PrState::Open,
            is_draft: false,
            base_ref: "main".to_owned(),
            head_ref: "topic".to_owned(),
            head_sha: "abc123".to_owned(),
            created_at: crate::domain::time::Timestamp::default(),
            updated_at: crate::domain::time::Timestamp::default(),
            additions: 1,
            deletions: 0,
            changed_files: 1,
            labels: Vec::new(),
            review_decision: None,
            checks: CheckSummary::default(),
            url: String::new(),
            is_cross_repository: false,
        }
    }

    fn detail_for(number: u64, head_sha: &str) -> PullRequestDetail {
        let mut summary = summary(number, "billing");
        summary.head_sha = head_sha.to_owned();
        PullRequestDetail {
            summary,
            body: "body".to_owned(),
            merge_state_status: None,
            reviewers: Vec::new(),
            commits: Vec::new(),
            checks: Vec::new(),
            reviews: Vec::new(),
            comments: Vec::new(),
            base_sha: None,
        }
    }

    const PATCH: &str =
        "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n";

    #[test]
    fn a_list_is_fetched_then_served_from_cache_with_its_age() {
        let cache = InMemoryCache::default();
        let clock = FakeClock::at(1_000);
        let repo = RepoId::parse("acme/service").unwrap();
        let forge = FakeForge::with_page(PullRequestPage::complete(
            vec![summary(1, "one"), summary(2, "two")],
            50,
        ));
        let prs = Prs::new(&forge, &cache, &clock, &repo);

        let fresh = prs.load_list(&PrQuery::default(), &Cancel::new()).unwrap();
        assert!(matches!(fresh, FetchOutcome::Fresh(_)));
        assert_eq!(fresh.value().items.len(), 2);
        assert_eq!(forge.calls.load(Ordering::SeqCst), 1);

        let cached = prs.cached_list(&PrQuery::default()).unwrap().unwrap();
        assert_eq!(cached.value.items.len(), 2);
        assert_eq!(cached.age_secs, 0);
        assert!(!cached.stale);

        clock.set(1_061);
        let cached = prs.cached_list(&PrQuery::default()).unwrap().unwrap();
        assert!(cached.stale, "a minute and a second later it is stale");
        assert_eq!(cached.age_secs, 61);
    }

    #[test]
    fn different_queries_do_not_share_a_cache_entry() {
        let cache = InMemoryCache::default();
        let clock = FakeClock::at(1_000);
        let repo = RepoId::parse("acme/service").unwrap();
        let forge = FakeForge::with_page(PullRequestPage::complete(vec![summary(1, "one")], 50));
        let prs = Prs::new(&forge, &cache, &clock, &repo);
        prs.load_list(&PrQuery::default(), &Cancel::new()).unwrap();

        let mut filtered = PrQuery::default();
        filtered.push(Filter::Author("alice".to_owned()));
        assert!(
            prs.cached_list(&filtered).unwrap().is_none(),
            "a filtered query must not read the unfiltered entry"
        );
    }

    #[test]
    fn a_failing_forge_falls_back_to_the_cached_list_and_says_why() {
        let cache = InMemoryCache::default();
        let clock = FakeClock::at(1_000);
        let repo = RepoId::parse("acme/service").unwrap();
        let working =
            FakeForge::with_page(PullRequestPage::complete(vec![summary(7, "seven")], 50));
        Prs::new(&working, &cache, &clock, &repo)
            .load_list(&PrQuery::default(), &Cancel::new())
            .unwrap();

        // The network is gone; the cached list still shows, with the reason.
        clock.set(1_600);
        let offline = FakeForge::failing("could not resolve host github.com");
        let outcome = Prs::new(&offline, &cache, &clock, &repo)
            .load_list(&PrQuery::default(), &Cancel::new())
            .unwrap();

        assert_eq!(outcome.value().items[0].number, 7);
        let reason = outcome.offline_reason().unwrap();
        assert!(reason.contains("could not resolve host"), "{reason}");
    }

    #[test]
    fn a_failing_forge_with_nothing_cached_reports_the_failure() {
        let cache = InMemoryCache::default();
        let clock = FakeClock::at(1_000);
        let repo = RepoId::parse("acme/service").unwrap();
        let forge = FakeForge::failing("gh is not installed");
        let error = Prs::new(&forge, &cache, &clock, &repo)
            .load_list(&PrQuery::default(), &Cancel::new())
            .unwrap_err();
        assert!(error.to_string().contains("not installed"), "{error}");
    }

    #[test]
    fn a_cached_payload_from_an_older_version_is_discarded_rather_than_trusted() {
        let cache = InMemoryCache::default();
        let clock = FakeClock::at(1_000);
        let repo = RepoId::parse("acme/service").unwrap();
        let forge = FakeForge::default();
        let key = list_key(&repo, &PrQuery::default()).unwrap();
        cache
            .write(&key, "{\"items\":\"not a page\"}", 1_000)
            .unwrap();

        let prs = Prs::new(&forge, &cache, &clock, &repo);
        assert!(prs.cached_list(&PrQuery::default()).unwrap().is_none());
        assert!(
            cache.read(&key).unwrap().is_none(),
            "and it is removed so it is not decoded again"
        );
    }

    #[test]
    fn a_detail_and_a_diff_are_cached_under_the_pull_request() {
        let cache = InMemoryCache::default();
        let clock = FakeClock::at(1_000);
        let repo = RepoId::parse("acme/service").unwrap();
        let forge = FakeForge::default();
        *forge.detail.lock().unwrap() = Some(detail_for(141, "ba6c89f0"));
        *forge.diff.lock().unwrap() = Some(PATCH.to_owned());
        let prs = Prs::new(&forge, &cache, &clock, &repo);

        let outcome = prs.load_detail(141, &Cancel::new()).unwrap();
        assert_eq!(outcome.value().summary.number, 141);
        assert!(prs.cached_detail(141).unwrap().is_some());

        let patch = prs.load_patch(141, "ba6c89f0", &Cancel::new()).unwrap();
        assert_eq!(patch.value().files.len(), 1);
        assert_eq!(patch.value().files[0].additions, 1);
        assert!(prs.cached_patch(141, "ba6c89f0").unwrap().is_some());
        assert!(
            prs.cached_patch(141, "different").unwrap().is_none(),
            "a new head SHA must not read the old diff"
        );
    }

    #[test]
    fn the_count_is_separate_and_its_failure_is_not_fatal() {
        let cache = InMemoryCache::default();
        let clock = FakeClock::at(1_000);
        let repo = RepoId::parse("acme/service").unwrap();
        let forge = FakeForge::default();
        *forge.count.lock().unwrap() = Some(137);
        let prs = Prs::new(&forge, &cache, &clock, &repo);
        assert_eq!(prs.count(&PrQuery::default(), &Cancel::new()).unwrap(), 137);

        let failing = FakeForge::failing("rate limited");
        let prs = Prs::new(&failing, &cache, &clock, &repo);
        assert!(prs.count(&PrQuery::default(), &Cancel::new()).is_err());
    }

    #[test]
    fn cache_keys_are_stable_and_safe_as_paths() {
        let repo = RepoId::parse("acme/service").unwrap();
        let key = list_key(&repo, &PrQuery::default()).unwrap();
        assert!(
            key.as_str().starts_with("github.com/acme/service/list/"),
            "{key}"
        );
        assert_eq!(
            std::path::Path::new(key.as_str()).extension(),
            Some(std::ffi::OsStr::new("json"))
        );
        assert_eq!(key, list_key(&repo, &PrQuery::default()).unwrap());

        // A search string with quotes, spaces and dots must not reach the path.
        let mut query = PrQuery::default();
        query.push(Filter::Title("retry \"webhook\" ../escape".to_owned()));
        let hostile = list_key(&repo, &query).unwrap();
        assert!(!hostile.as_str().contains(".."), "{hostile}");
        assert!(!hostile.as_str().contains(' '), "{hostile}");
        assert_ne!(hostile, key);
    }

    #[test]
    fn the_hash_is_the_same_on_every_run_and_platform() {
        // DefaultHasher is explicitly not stable across releases; a cache key must
        // be, or every upgrade would miss its own cache.
        assert_eq!(
            fnv1a(b"state=open;sort=newest;limit=50"),
            fnv1a(b"state=open;sort=newest;limit=50")
        );
        assert_ne!(fnv1a(b"a"), fnv1a(b"b"));
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        // A fixed vector, so a change to the hash cannot go unnoticed: it would
        // silently invalidate every existing cache entry.
        assert_eq!(
            fnv1a(b"state=open;sort=newest;limit=50"),
            fnv1a(b"state=open;sort=newest;limit=50")
        );
        assert_eq!(
            format!("{:016x}", fnv1a(b"state=open;sort=newest;limit=50")),
            "fdbc429f5ab07d3f",
            "the expected value of the hash for a known input"
        );
    }

    #[test]
    fn an_outcome_reports_whether_it_came_from_the_network() {
        let fresh = FetchOutcome::Fresh(1_u32);
        assert!(fresh.offline_reason().is_none());
        assert_eq!(fresh.into_value(), 1);

        let offline = FetchOutcome::Offline {
            value: 2_u32,
            reason: "no network".to_owned(),
        };
        assert_eq!(offline.offline_reason(), Some("no network"));
        assert_eq!(offline.value(), &2);
    }
    #[test]
    fn a_diff_is_cached_per_source_and_per_flag() {
        // Three ways the same head SHA can produce different bytes: a different
        // source, different context, and whitespace handling. Serving one for another
        // is what made the worktree's diff look like GitHub's, so the keys must
        // differ — and they must keep differing for the default case, so a cache
        // written by an older build is still read.
        let repo = RepoId::parse("acme/service").expect("a repository");
        let default = diff_key_with(
            &repo,
            7,
            "abcdef012345",
            DiffOptions::default(),
            DiffSource::Forge,
        )
        .expect("a key");
        let worktree = diff_key_with(
            &repo,
            7,
            "abcdef012345",
            DiffOptions::default(),
            DiffSource::Worktree,
        )
        .expect("a key");
        let wider = diff_key_with(
            &repo,
            7,
            "abcdef012345",
            DiffOptions {
                context: 10,
                ..DiffOptions::default()
            },
            DiffSource::Forge,
        )
        .expect("a key");
        let ignored = diff_key_with(
            &repo,
            7,
            "abcdef012345",
            DiffOptions {
                ignore_whitespace: true,
                ..DiffOptions::default()
            },
            DiffSource::Worktree,
        )
        .expect("a key");

        assert_ne!(default, worktree, "the source is part of the key");
        assert_ne!(default, wider, "the context is part of the key");
        assert_ne!(worktree, ignored, "whitespace handling is part of the key");
        assert_eq!(
            default.as_str(),
            "github.com/acme/service/pr-7/diff-abcdef012345.patch.json",
            "the default key is the one older builds wrote"
        );
        assert!(worktree.as_str().ends_with("-wt.patch.json"), "{worktree}");
    }
}
