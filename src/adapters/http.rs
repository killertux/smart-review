//! A tiny async façade so the rest of the app stays synchronous (ARCH-3).
//!
//! Both the LLM client and the catalog fetch are `async` because their crates are.
//! The application is not: jobs run on their own threads and block, which is what
//! makes cancellation, timeouts and "drop the stale result" straightforward.
//!
//! So every async call is driven here, on a **current-thread** runtime built for the
//! call and dropped with it. Two things follow, and both matter:
//!
//! - no runtime is ever held across calls, so a cancelled job cannot leave a
//!   half-driven runtime behind, and a call cannot panic because it was made from
//!   inside another runtime (the failure mode of `reqwest::blocking`);
//! - blocking is honest: this thread is a job thread that exists to wait.

use std::fmt;
use std::future::Future;

/// Why the async work could not be driven.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeError {
    /// The runtime could not be created.
    #[error("could not start the async runtime: {0}")]
    Start(String),
}

/// Runs a future to completion on a fresh current-thread runtime.
///
/// # Errors
///
/// Returns [`RuntimeError::Start`] when the runtime cannot be built, which means the
/// process is in a state nothing else will recover from either.
pub fn block_on<F>(future: F) -> Result<F::Output, RuntimeError>
where
    F: Future,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| RuntimeError::Start(error.to_string()))?;
    Ok(runtime.block_on(future))
}

/// An error message from an HTTP fetch, as the catalog adapter reports it.
pub type FetchError = String;

/// Fetches a URL's body.
///
/// A trait rather than a direct `reqwest` call so the catalog adapter's cache, TTL
/// and fallback rules can be tested — including "the network is down" — without a
/// network (NFR-5.2).
pub trait HttpFetch: fmt::Debug + Send + Sync {
    /// GETs a URL and returns the body as text.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason: it is shown to the user when the catalog is
    /// being refreshed by hand.
    fn get(&self, url: &str) -> Result<String, FetchError>;
}

/// The real thing, over `reqwest` with rustls.
#[derive(Debug)]
pub struct ReqwestFetcher {
    client: reqwest::Client,
}

impl Default for ReqwestFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestFetcher {
    /// A client with a timeout that suits a document fetch, and a user agent that
    /// says who is asking.
    #[must_use]
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("smart-review/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self { client }
    }
}

impl HttpFetch for ReqwestFetcher {
    fn get(&self, url: &str) -> Result<String, FetchError> {
        block_on(async {
            let response = self
                .client
                .get(url)
                .send()
                .await
                .map_err(|error| describe(&error))?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!("the server answered {status}"));
            }
            response.text().await.map_err(|error| describe(&error))
        })
        .map_err(|error| error.to_string())?
    }
}

/// Turns a `reqwest` error into something worth showing a user.
fn describe(error: &reqwest::Error) -> FetchError {
    if error.is_timeout() {
        return "the request timed out".to_owned();
    }
    if error.is_connect() {
        return "could not connect: check your network".to_owned();
    }
    // `reqwest`'s Display for a URL error is long; the source chain's first useful
    // line is what a person needs.
    let text = error.to_string();
    text.lines()
        .next()
        .unwrap_or("the request failed")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_future_is_driven_to_completion() {
        let value = block_on(async { 7 }).expect("the runtime starts");
        assert_eq!(value, 7);
    }

    #[test]
    fn no_runtime_is_left_running() {
        // Two calls in a row on the same thread: if a runtime were kept, the second
        // would fail to build one ("cannot start a runtime from within a runtime").
        let first = block_on(async { 1 }).expect("starts");
        let second = block_on(async { 2 }).expect("starts again");
        assert_eq!((first, second), (1, 2));
    }

    #[test]
    fn errors_are_short_enough_to_show() {
        // A URL `reqwest` cannot parse is the cheapest real error to produce.
        let error = reqwest::Client::new()
            .get("not a url at all")
            .build()
            .expect_err("that is not a URL");
        let message = describe(&error);
        assert!(!message.is_empty());
        assert!(!message.contains('\n'), "{message}");
    }
}
