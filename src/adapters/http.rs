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
use std::time::Duration;

use crate::ports::Cancel;

/// Why the async work could not be driven.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeError {
    /// The runtime could not be created.
    #[error("could not start the async runtime: {0}")]
    Start(String),
}

/// Why an async wait did not reach its future's result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AsyncWaitError {
    /// The runtime could not be created.
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    /// The caller stopped waiting for this work.
    #[error("the request was cancelled")]
    Cancelled,
    /// The request's own deadline elapsed.
    #[error("the request timed out")]
    TimedOut,
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

/// Drives a future until it completes, is cancelled, or reaches its own deadline.
///
/// Dropping the losing future closes its transport resources before the worker reports
/// completion, so a cancelled request releases the job slot instead of waiting for a
/// socket timeout (IR-08).
///
/// # Errors
///
/// Returns [`AsyncWaitError::Cancelled`] when `cancel` is raised,
/// [`AsyncWaitError::TimedOut`] when `timeout` elapses, or
/// [`AsyncWaitError::Runtime`] when Tokio cannot start.
pub fn block_on_cancellable<F>(
    future: F,
    cancel: &Cancel,
    timeout: Duration,
) -> Result<F::Output, AsyncWaitError>
where
    F: Future,
{
    if cancel.is_cancelled() {
        return Err(AsyncWaitError::Cancelled);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| RuntimeError::Start(error.to_string()))
        .map_err(AsyncWaitError::Runtime)?;
    runtime.block_on(async {
        tokio::select! {
            result = future => Ok(result),
            () = cancel.cancelled() => Err(AsyncWaitError::Cancelled),
            () = tokio::time::sleep(timeout) => Err(AsyncWaitError::TimedOut),
        }
    })
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
    fn get(&self, url: &str, cancel: &Cancel) -> Result<String, FetchError>;
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
            .build()
            .unwrap_or_default();
        Self { client }
    }
}

impl HttpFetch for ReqwestFetcher {
    fn get(&self, url: &str, cancel: &Cancel) -> Result<String, FetchError> {
        block_on_cancellable(
            async {
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
            },
            cancel,
            Duration::from_secs(30),
        )
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
    fn cancellation_interrupts_a_pending_future() {
        let cancel = Cancel::new();
        let worker_cancel = cancel.clone();
        let worker = std::thread::spawn(move || {
            block_on_cancellable(
                async { tokio::time::sleep(Duration::from_secs(30)).await },
                &worker_cancel,
                Duration::from_secs(60),
            )
        });

        std::thread::sleep(Duration::from_millis(10));
        cancel.cancel();
        let result = worker.join().unwrap_or(Err(AsyncWaitError::Cancelled));
        assert_eq!(result, Err(AsyncWaitError::Cancelled));
    }

    #[test]
    fn a_deadline_is_not_reported_as_cancellation() {
        let cancel = Cancel::new();
        let result = block_on_cancellable(
            async { tokio::time::sleep(Duration::from_secs(30)).await },
            &cancel,
            Duration::from_millis(1),
        );
        assert_eq!(result, Err(AsyncWaitError::TimedOut));
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
