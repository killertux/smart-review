//! The models.dev catalog adapter (FR-4.7, §7.5, DEC-17).
//!
//! The rules this implements, in the order they matter:
//!
//! 1. **a usable cache always beats an empty picker** — a fetch that fails after a
//!    cache exists returns the cache, marked [`CatalogSource::Stale`] with the reason;
//! 2. **the TTL is about freshness, not correctness** — past it, a refresh is
//!    attempted, but the old copy is kept until the new one parses;
//! 3. **a bad payload never overwrites a good cache** — the document is parsed before
//!    it is written, so a proxy serving an error page cannot destroy the catalog;
//! 4. **the cache says when it was fetched** — the file is the api.json body plus a
//!    timestamp, so age is not inferred from the filesystem's clock.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::adapters::http::HttpFetch;
use crate::domain::model::Catalog;
use crate::logging::{self, Level};
use crate::ports::Cancel;
use crate::ports::Clock;
use crate::ports::catalog::{
    CatalogFetchError, CatalogLoad, CatalogPolicy, CatalogSource, ModelCatalogPort,
};

/// The cached document: the feed plus when it was fetched (§7.5).
#[derive(Debug, Serialize, Deserialize)]
struct CachedCatalog {
    fetched_at: u64,
    /// The api.json body, kept as published so unknown fields survive the cache.
    providers: serde_json::Value,
}

/// Reads and caches `https://models.dev/api.json` (FR-4.7).
pub struct ModelsDevCatalog {
    url: String,
    cache_path: PathBuf,
    ttl_secs: u64,
    fetcher: Arc<dyn HttpFetch>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for ModelsDevCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelsDevCatalog")
            .field("url", &self.url)
            .field("cache_path", &self.cache_path)
            .field("ttl_secs", &self.ttl_secs)
            .finish_non_exhaustive()
    }
}

impl ModelsDevCatalog {
    /// A catalog over the given URL, cached at `cache_path`.
    #[must_use]
    pub fn new(
        url: impl Into<String>,
        cache_path: impl Into<PathBuf>,
        ttl_secs: u64,
        fetcher: Arc<dyn HttpFetch>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            url: url.into(),
            cache_path: cache_path.into(),
            ttl_secs,
            fetcher,
            clock,
        }
    }

    /// The file the catalog is cached in.
    #[must_use]
    pub fn cache_path(&self) -> &Path {
        &self.cache_path
    }

    /// Reads the cache, if there is one.
    fn read_cache(&self) -> Option<(Catalog, u64)> {
        let text = std::fs::read_to_string(&self.cache_path).ok()?;
        let cached: CachedCatalog = serde_json::from_str(&text).ok()?;
        let catalog = Catalog::from_json(&cached.providers.to_string()).ok()?;
        (!catalog.is_empty()).then_some((catalog, cached.fetched_at))
    }

    /// Writes the cache. Best effort: a cache that cannot be written is a slower
    /// next start, not a failure (FR-4.7).
    fn write_cache(&self, providers_json: &str) {
        let Some(parent) = self.cache_path.parent() else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(parent) {
            logging::log(
                Level::Warn,
                format!("could not create {}: {error}", parent.display()),
            );
            return;
        }
        // Re-serialise through `serde_json::Value` so the body that lands in the
        // cache is the document that was fetched, unknown fields included.
        let Ok(providers) = serde_json::from_str::<serde_json::Value>(providers_json) else {
            return;
        };
        let body = CachedCatalog {
            fetched_at: self.clock.now_unix_secs(),
            providers,
        };
        let Ok(text) = serde_json::to_string(&body) else {
            return;
        };
        if let Err(error) = crate::adapters::fs::write_atomic(&self.cache_path, &text) {
            logging::log(
                Level::Warn,
                format!(
                    "could not cache the catalog at {}: {error}",
                    self.cache_path.display()
                ),
            );
        }
    }

    /// Fetches, parses (via the caller) and caches in one step.
    fn fetch_and_cache(&self, cancel: &Cancel) -> Result<Catalog, CatalogFetchError> {
        let body = self
            .fetcher
            .get(&self.url, cancel)
            .map_err(CatalogFetchError::Unavailable)?;
        let catalog = Catalog::from_json(&body)
            .map_err(|error| CatalogFetchError::Malformed(error.to_string()))?;
        if catalog.is_empty() {
            return Err(CatalogFetchError::Malformed(
                "the catalog listed no providers".to_owned(),
            ));
        }
        self.write_cache(&body);
        Ok(catalog)
    }
}

impl ModelCatalogPort for ModelsDevCatalog {
    fn load(
        &self,
        policy: CatalogPolicy,
        cancel: &Cancel,
    ) -> Result<CatalogLoad, CatalogFetchError> {
        let cached = self.read_cache();
        let age_secs = |fetched_at: u64| self.clock.now_unix_secs().saturating_sub(fetched_at);

        // A fresh cache answers without touching the network unless a refresh was
        // asked for (FR-4.7).
        if let Some((catalog, fetched_at)) = &cached {
            let age = age_secs(*fetched_at);
            match policy {
                CatalogPolicy::CacheOnly => {
                    return Ok(CatalogLoad {
                        catalog: catalog.clone(),
                        source: CatalogSource::Cached { age_secs: age },
                    });
                }
                CatalogPolicy::CacheFirst if age <= self.ttl_secs => {
                    logging::log(Level::Debug, format!("using the catalog cached {age}s ago"));
                    return Ok(CatalogLoad {
                        catalog: catalog.clone(),
                        source: CatalogSource::Cached { age_secs: age },
                    });
                }
                CatalogPolicy::CacheFirst | CatalogPolicy::Refresh => {}
            }
        } else if policy == CatalogPolicy::CacheOnly {
            return Err(CatalogFetchError::Unavailable(
                "no catalog is cached yet".to_owned(),
            ));
        }

        match self.fetch_and_cache(cancel) {
            Ok(catalog) => {
                logging::log(
                    Level::Info,
                    format!("fetched the model catalog from {}", self.url),
                );
                Ok(CatalogLoad {
                    catalog,
                    source: CatalogSource::Fetched,
                })
            }
            Err(error) => {
                // Degrade, never fail: a stale catalog is better than no picker, and
                // the reason travels with it so the UI can say what happened.
                match cached {
                    Some((catalog, fetched_at)) => Ok(CatalogLoad {
                        catalog,
                        source: CatalogSource::Stale {
                            age_secs: age_secs(fetched_at),
                            reason: error.to_string(),
                        },
                    }),
                    None => Err(error),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FakeClock, TempHome, temp_home};
    use std::sync::Mutex;

    /// A fetcher that answers with whatever the test says, and counts the calls.
    #[derive(Debug, Default)]
    struct FakeFetch {
        body: Mutex<Option<Result<String, String>>>,
        calls: Mutex<usize>,
    }

    impl FakeFetch {
        fn serving(body: &str) -> Arc<Self> {
            let fake = Self::default();
            *fake.body.lock().unwrap() = Some(Ok(body.to_owned()));
            Arc::new(fake)
        }

        fn failing(reason: &str) -> Arc<Self> {
            let fake = Self::default();
            *fake.body.lock().unwrap() = Some(Err(reason.to_owned()));
            Arc::new(fake)
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }

        fn set(&self, body: &str) {
            *self.body.lock().unwrap() = Some(Ok(body.to_owned()));
        }
    }

    impl HttpFetch for FakeFetch {
        fn get(&self, _url: &str, _cancel: &Cancel) -> Result<String, String> {
            *self.calls.lock().unwrap() += 1;
            self.body
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| Err("no body configured".to_owned()))
        }
    }

    /// A transport that will not answer until its caller stops waiting.
    #[derive(Debug)]
    struct HeldFetch;

    impl HttpFetch for HeldFetch {
        fn get(&self, _url: &str, cancel: &Cancel) -> Result<String, String> {
            while !cancel.is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err("the request was cancelled".to_owned())
        }
    }

    const FEED: &str = r#"{"deepseek": {"id": "deepseek", "name": "DeepSeek",
        "api": "https://api.deepseek.com", "env": ["DEEPSEEK_API_KEY"],
        "models": {"deepseek-chat": {"id": "deepseek-chat", "name": "Chat"}}}}"#;

    const OTHER_FEED: &str = r#"{"openai": {"id": "openai", "name": "OpenAI",
        "env": ["OPENAI_API_KEY"], "models": {"gpt-5": {"id": "gpt-5", "name": "GPT-5"}}}}"#;

    struct Harness {
        _home: TempHome,
        catalog: ModelsDevCatalog,
        clock: Arc<FakeClock>,
    }

    fn harness(fetch: Arc<FakeFetch>, ttl_secs: u64) -> Harness {
        let home = temp_home();
        let clock = Arc::new(FakeClock::new(1_000));
        let catalog = ModelsDevCatalog::new(
            "https://models.dev/api.json",
            home.path().join("cache/models.json"),
            ttl_secs,
            fetch,
            clock.clone(),
        );
        Harness {
            _home: home,
            catalog,
            clock,
        }
    }

    #[test]
    fn a_pending_catalog_fetch_stops_when_cancelled() {
        let home = temp_home();
        let catalog = ModelsDevCatalog::new(
            "https://models.dev/api.json",
            home.path().join("cache/models.json"),
            86_400,
            Arc::new(HeldFetch),
            Arc::new(FakeClock::new(1_000)),
        );
        let cancel = Cancel::new();
        let signal = cancel.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            signal.cancel();
        });
        let started = std::time::Instant::now();
        let error = catalog
            .load(CatalogPolicy::Refresh, &cancel)
            .expect_err("the cancelled fetch has no cache to return");
        assert!(canceller.join().is_ok());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(error.to_string().contains("cancelled"), "{error}");
    }

    #[test]
    fn a_first_run_fetches_and_caches() {
        let fetch = FakeFetch::serving(FEED);
        let harness = harness(fetch.clone(), 86_400);

        let load = harness
            .catalog
            .load(CatalogPolicy::CacheFirst, &Cancel::new())
            .expect("fetched");
        assert_eq!(load.source, CatalogSource::Fetched);
        assert!(load.catalog.provider("deepseek").is_some());
        assert_eq!(fetch.calls(), 1);
        assert!(
            harness.catalog.cache_path().exists(),
            "the cache is written for the next run"
        );

        // The cached body keeps what the feed published.
        let text = std::fs::read_to_string(harness.catalog.cache_path()).unwrap();
        assert!(text.contains("DEEPSEEK_API_KEY"), "{text}");
        assert!(text.contains("fetched_at"), "{text}");
    }

    #[test]
    fn a_second_run_inside_the_ttl_does_not_touch_the_network() {
        let fetch = FakeFetch::serving(FEED);
        let harness = harness(fetch.clone(), 86_400);
        harness
            .catalog
            .load(CatalogPolicy::CacheFirst, &Cancel::new())
            .expect("fetched");
        harness.clock.advance(60);

        let load = harness
            .catalog
            .load(CatalogPolicy::CacheFirst, &Cancel::new())
            .expect("cached");
        assert_eq!(load.source, CatalogSource::Cached { age_secs: 60 });
        assert_eq!(fetch.calls(), 1, "the network was not used again");
    }

    #[test]
    fn a_stale_cache_is_refreshed() {
        let fetch = FakeFetch::serving(FEED);
        let harness = harness(fetch.clone(), 60);
        harness
            .catalog
            .load(CatalogPolicy::CacheFirst, &Cancel::new())
            .expect("fetched");
        harness.clock.advance(120);
        fetch.set(OTHER_FEED);

        let load = harness
            .catalog
            .load(CatalogPolicy::CacheFirst, &Cancel::new())
            .expect("refreshed");
        assert_eq!(load.source, CatalogSource::Fetched);
        assert!(
            load.catalog.provider("openai").is_some(),
            "the new feed is used"
        );
        assert_eq!(fetch.calls(), 2);
    }

    #[test]
    fn a_failed_refresh_falls_back_to_the_stale_cache_with_a_reason() {
        let fetch = FakeFetch::serving(FEED);
        let harness = harness(fetch.clone(), 60);
        harness
            .catalog
            .load(CatalogPolicy::CacheFirst, &Cancel::new())
            .expect("fetched");
        harness.clock.advance(120);

        // The network goes away after the first successful fetch.
        let offline = ModelsDevCatalog::new(
            "https://models.dev/api.json",
            harness.catalog.cache_path(),
            60,
            FakeFetch::failing("could not connect: check your network"),
            harness.clock.clone(),
        );
        let load = offline
            .load(CatalogPolicy::CacheFirst, &Cancel::new())
            .expect("the cache answers");
        match load.source {
            CatalogSource::Stale { age_secs, reason } => {
                assert_eq!(age_secs, 120);
                assert!(reason.contains("check your network"), "{reason}");
            }
            other => panic!("expected a stale cache, got {other:?}"),
        }
        assert!(load.catalog.provider("deepseek").is_some(), "still usable");
    }

    #[test]
    fn a_first_run_with_no_network_reports_that_it_could_not_fetch() {
        let harness = harness(FakeFetch::failing("could not connect"), 60);
        let error = harness
            .catalog
            .load(CatalogPolicy::CacheFirst, &Cancel::new())
            .expect_err("nothing to fall back on");
        assert!(
            matches!(error, CatalogFetchError::Unavailable(_)),
            "{error:?}"
        );
        assert!(
            error.to_string().contains("could not be fetched"),
            "{error}"
        );
    }

    #[test]
    fn an_unparseable_payload_does_not_destroy_the_cache() {
        let fetch = FakeFetch::serving(FEED);
        let harness = harness(fetch.clone(), 60);
        harness
            .catalog
            .load(CatalogPolicy::CacheFirst, &Cancel::new())
            .expect("fetched");
        let good = std::fs::read_to_string(harness.catalog.cache_path()).unwrap();
        harness.clock.advance(120);

        // A proxy serving an error page after the token expires: valid JSON, wrong
        // shape. It must not replace a good cache.
        fetch.set(r#"{"message": "rate limited"}"#);
        let load = harness
            .catalog
            .load(CatalogPolicy::Refresh, &Cancel::new())
            .expect("degraded");
        assert!(
            matches!(load.source, CatalogSource::Stale { .. }),
            "{:?}",
            load.source
        );
        assert_eq!(
            std::fs::read_to_string(harness.catalog.cache_path()).unwrap(),
            good,
            "the cache is untouched"
        );
    }

    #[test]
    fn a_non_json_payload_is_reported_as_malformed() {
        let fetch = FakeFetch::serving("<html>maintenance</html>");
        let harness = harness(fetch, 60);
        let error = harness
            .catalog
            .load(CatalogPolicy::CacheFirst, &Cancel::new())
            .expect_err("not a catalog");
        assert!(
            matches!(error, CatalogFetchError::Malformed(_)),
            "{error:?}"
        );
    }

    #[test]
    fn a_refresh_ignores_a_fresh_cache() {
        let fetch = FakeFetch::serving(FEED);
        let harness = harness(fetch.clone(), 86_400);
        harness
            .catalog
            .load(CatalogPolicy::CacheFirst, &Cancel::new())
            .expect("fetched");
        fetch.set(OTHER_FEED);

        let load = harness
            .catalog
            .load(CatalogPolicy::Refresh, &Cancel::new())
            .expect("refreshed");
        assert_eq!(load.source, CatalogSource::Fetched);
        assert_eq!(fetch.calls(), 2, ":catalog refresh must really fetch");
    }

    #[test]
    fn cache_only_never_calls_the_network() {
        let fetch = FakeFetch::serving(FEED);
        let harness = harness(fetch.clone(), 1);
        harness
            .catalog
            .load(CatalogPolicy::CacheFirst, &Cancel::new())
            .expect("fetched");
        harness.clock.advance(999_999);

        let load = harness
            .catalog
            .load(CatalogPolicy::CacheOnly, &Cancel::new())
            .expect("cached");
        assert!(matches!(load.source, CatalogSource::Cached { .. }));
        assert_eq!(fetch.calls(), 1, "no fetch, however stale the cache is");
    }

    #[test]
    fn cache_only_without_a_cache_says_so() {
        let harness = harness(FakeFetch::serving(FEED), 60);
        let error = harness
            .catalog
            .load(CatalogPolicy::CacheOnly, &Cancel::new())
            .expect_err("nothing cached");
        assert!(
            error.to_string().contains("no catalog is cached"),
            "{error}"
        );
    }

    #[test]
    fn a_cache_of_unmappable_providers_is_still_a_cache() {
        // The feed has entries, but none this build can reach: the catalog is not
        // empty, and hiding happens later, in the picker (FR-4.7).
        let fetch = FakeFetch::serving(
            r#"{"watsonx": {"id": "watsonx", "name": "watsonx", "env": ["W"], "models": {}}}"#,
        );
        let harness = harness(fetch, 60);
        let load = harness
            .catalog
            .load(CatalogPolicy::CacheFirst, &Cancel::new())
            .expect("loaded");
        assert_eq!(load.catalog.reachable().count(), 0);
        assert_eq!(load.catalog.len(), 1);
    }
}
