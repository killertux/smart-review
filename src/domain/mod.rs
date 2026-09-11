//! Domain types and invariants (ARCH-1).
//!
//! This layer depends on nothing else in the crate and knows nothing about
//! terminals, processes or HTTP. It will hold `PullRequest`, `FileDiff`, `Hunk`,
//! `DiffLine`, `Draft`, `Analysis`, `ReviewPlan` and the invariants that go with
//! them, arriving with M1 and M2.
//!
//! Nothing lives here yet because M0 is a shell: inventing domain types before
//! the features that need them would only be reworked later.
