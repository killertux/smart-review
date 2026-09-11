//! Ports: the traits the rest of the application depends on (ARCH-2).
//!
//! Only the ports M0 actually exercises are declared here. Everything else
//! arrives with the milestone that needs it, so the crate never carries an empty
//! abstraction:
//!
//! | Port | Milestone |
//! |---|---|
//! | [`Clock`] | M0 |
//! | [`ConfigStore`] | M0 |
//! | [`StateStore`] | M0 |
//! | `ForgePort` | M1 |
//! | `WorkspacePort` | M2 |
//! | `ModelCatalogPort` | M2 |
//! | `CredentialsStore` | M2 |
//! | `LlmPort` | M2 |

use crate::config::Loaded;
use crate::error::Result;
use crate::state::AppState;

/// Time, injected so cache lifetimes and relative timestamps are testable
/// (NFR-5.2).
pub trait Clock: std::fmt::Debug + Send + Sync {
    /// Seconds since the Unix epoch.
    fn now_unix_secs(&self) -> u64;
}

/// Reads the user's configuration (FR-8.2).
///
/// Writing configuration back arrives in M2 together with comment-preserving
/// edits (DEC-19); M0 never rewrites the user's file.
pub trait ConfigStore: std::fmt::Debug {
    /// Reads the file, applying built-in defaults for anything missing.
    ///
    /// # Errors
    ///
    /// Returns an error when the configuration cannot be read or parsed.
    fn load(&self) -> Result<Loaded>;

    /// Where the configuration lives.
    fn path(&self) -> &std::path::Path;
}

/// Reads and writes small persisted state (FR-8.5).
pub trait StateStore: std::fmt::Debug {
    /// Reads the state, returning defaults when the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error when the state cannot be read or parsed.
    fn load(&self) -> Result<AppState>;

    /// Writes the state atomically.
    ///
    /// # Errors
    ///
    /// Returns an error when the state cannot be written.
    fn save(&self, state: &AppState) -> Result<()>;
}
