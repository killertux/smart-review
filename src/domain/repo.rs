//! Repository identity (FR-1.3).
//!
//! Everything persisted per repository is keyed by [`RepoId::key`], which is
//! `host/owner/name`. That is what makes a second clone of the same repository
//! share its cache while two different repositories can never collide — two
//! clones have different paths but the same identity.

use std::fmt;

use serde::{Deserialize, Serialize};

/// `host/owner/name`, the identity of a repository rather than of a checkout.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RepoId {
    host: String,
    owner: String,
    name: String,
}

/// Why a repository reference could not be understood.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RepoIdError {
    #[error("'{0}' is not a repository; expected OWNER/NAME or HOST/OWNER/NAME")]
    Malformed(String),

    #[error("'{0}' has no owner and name")]
    MissingParts(String),
}

impl RepoId {
    /// Builds an id, normalising the host to lower case and stripping a `.git`
    /// suffix from the name.
    #[must_use]
    pub fn new(host: &str, owner: &str, name: &str) -> Self {
        let name = name.strip_suffix(".git").unwrap_or(name);
        Self {
            host: host.trim().to_ascii_lowercase(),
            // Keep the durable cache identity stable. Released versions retained
            // owner/name casing, so normalisation belongs only to workspace storage.
            owner: owner.trim().to_owned(),
            name: name.trim().to_owned(),
        }
    }

    /// Parses `OWNER/NAME` (the GitHub shorthand, host assumed to be
    /// `github.com`) or `HOST/OWNER/NAME`.
    ///
    /// # Errors
    ///
    /// Returns [`RepoIdError`] when the input has the wrong number of parts or an
    /// empty part.
    pub fn parse(input: &str) -> Result<Self, RepoIdError> {
        let trimmed = input
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        let parts: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
        match parts.as_slice() {
            [owner, name] => {
                if owner.is_empty() || name.is_empty() {
                    return Err(RepoIdError::MissingParts(input.to_owned()));
                }
                Ok(Self::new("github.com", owner, name))
            }
            [host, owner, name] => {
                if host.is_empty() || owner.is_empty() || name.is_empty() {
                    return Err(RepoIdError::MissingParts(input.to_owned()));
                }
                Ok(Self::new(host, owner, name))
            }
            _ => Err(RepoIdError::Malformed(input.to_owned())),
        }
    }

    /// Parses a git remote URL in any of the shapes git allows.
    ///
    /// Returns `None` when the URL is not parseable as a repository, which is the
    /// normal case for a remote pointing at a local directory or a non-GitHub
    /// forge (FR-1.1 walks the remotes and keeps the first that parses).
    #[must_use]
    pub fn from_remote_url(url: &str) -> Option<Self> {
        let url = url.trim();

        // scp-like syntax: git@github.com:owner/name.git. The host is the part
        // after the `@`, not something that can be inferred later.
        if let Some((head, rest)) = url.split_once(':')
            && head.contains('@')
            && !rest.contains("//")
        {
            let host = head.rsplit('@').next().unwrap_or(head);
            return Self::with_host(host, rest);
        }

        // URL syntax: ssh://git@github.com/owner/name.git, https://host/owner/name
        if let Some((_, rest)) = url.split_once("://") {
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next().unwrap_or(authority);
            // Drop a port: we key by host, not by endpoint.
            let host = host.split(':').next().unwrap_or(host);
            // An empty host means a URL with no authority at all, such as
            // `file:///srv/git/local.git`, which is a directory and not a forge.
            if !host.contains('.') {
                return None;
            }
            return Self::with_host(host, path);
        }
        None
    }

    /// Builds an id from a known host and the path that followed it.
    ///
    /// Exactly `owner/name` is accepted. A deeper path is a GitLab subgroup or a
    /// hosted-Forgejo layout, which v1 cannot address, and guessing which segment
    /// is the repository name would produce a plausible wrong answer.
    fn with_host(host: &str, path: &str) -> Option<Self> {
        let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        match parts.as_slice() {
            [owner, name] => Some(Self::new(host, owner, name)),
            _ => None,
        }
    }

    /// The host, e.g. `github.com`.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The owner or organisation.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The repository name, without `.git`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `owner/name`, what `gh --repo` expects.
    #[must_use]
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// `owner-name`, the directory name managed worktrees live under (FR-3.1).
    ///
    /// Deliberately not the cache key: a worktree directory is something a user
    /// types into `git worktree list` and `cd`, and nested directories there would
    /// be one more thing to explain.
    #[must_use]
    pub fn dir_name(&self) -> String {
        format!("{}-{}", self.owner, self.name)
    }

    /// An unambiguous, filesystem-safe key for app-owned storage (IR-13).
    ///
    /// Each component is hexadecimal UTF-8, so `a-b/c` cannot collide with
    /// `a/b-c`, and callers never need to recover an identity by splitting a
    /// human-friendly directory name.
    #[must_use]
    pub fn storage_key(&self) -> String {
        format!(
            "h-{}--o-{}--r-{}",
            hex_component(&self.host),
            hex_component(&self.owner.to_ascii_lowercase()),
            hex_component(&self.name.to_ascii_lowercase())
        )
    }

    /// Recovers the normalised identity encoded in an app-owned workspace key.
    ///
    /// This key contains no user-controlled separators, so recovery never needs to
    /// infer a repository from a human-friendly directory name.
    #[must_use]
    pub fn from_storage_key(key: &str) -> Option<Self> {
        let parts: Vec<&str> = key.split("--").collect();
        let [host, owner, name] = parts.as_slice() else {
            return None;
        };
        Some(Self::new(
            decode_component(host.strip_prefix("h-")?)?.as_str(),
            decode_component(owner.strip_prefix("o-")?)?.as_str(),
            decode_component(name.strip_prefix("r-")?)?.as_str(),
        ))
    }

    /// `host/owner/name`, the cache partitioning key (FR-1.3).
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}/{}/{}", self.host, self.owner, self.name)
    }

    /// The browser URL of the repository.
    #[must_use]
    pub fn url(&self) -> String {
        format!("https://{}/{}", self.host, self.slug())
    }

    /// Whether this is a GitHub repository, which is all v1 supports.
    #[must_use]
    pub fn is_github(&self) -> bool {
        self.host.eq_ignore_ascii_case("github.com")
    }
}

fn hex_component(component: &str) -> String {
    use std::fmt::Write;

    let mut encoded = String::with_capacity(component.len() * 2);
    for byte in component.bytes() {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn decode_component(component: &str) -> Option<String> {
    if !component.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = component
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        })
        .collect();
    String::from_utf8(bytes?).ok()
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.slug())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorthand_and_full_forms_agree() {
        let short = RepoId::parse("acme/service").unwrap();
        let full = RepoId::parse("github.com/acme/service").unwrap();
        assert_eq!(short, full);
        assert_eq!(short.key(), "github.com/acme/service");
        assert_eq!(short.slug(), "acme/service");
        assert_eq!(short.url(), "https://github.com/acme/service");
        assert!(short.is_github());
    }

    #[test]
    fn the_host_is_normalised_and_the_git_suffix_stripped() {
        let id = RepoId::parse("GitHub.COM/acme/service.git").unwrap();
        assert_eq!(id.host(), "github.com");
        assert_eq!(id.name(), "service");
    }

    #[test]
    fn ir_13_storage_identity_is_case_normalized_and_collision_free() {
        let mixed = RepoId::new("GitHub.COM", "Acme", "Service");
        assert_ne!(mixed, RepoId::new("github.com", "acme", "service"));
        assert_eq!(
            mixed.storage_key(),
            RepoId::new("github.com", "acme", "service").storage_key()
        );
        assert_ne!(
            RepoId::new("github.com", "a-b", "c").storage_key(),
            RepoId::new("github.com", "a", "b-c").storage_key()
        );
        assert_eq!(
            RepoId::from_storage_key(&mixed.storage_key()),
            Some(RepoId::new("github.com", "acme", "service"))
        );
    }

    #[test]
    fn every_remote_url_shape_git_allows_is_understood() {
        let expected = RepoId::parse("acme/service").unwrap();
        for url in [
            "git@github.com:acme/service.git",
            "git@github.com:acme/service",
            "ssh://git@github.com/acme/service.git",
            "https://github.com/acme/service.git",
            "https://github.com/acme/service",
            "http://github.com/acme/service",
            "https://github.com:443/acme/service",
            "git://github.com/acme/service.git",
        ] {
            assert_eq!(
                RepoId::from_remote_url(url).as_ref(),
                Some(&expected),
                "failed for {url}"
            );
        }
    }

    #[test]
    fn a_non_github_remote_keeps_its_host() {
        let id = RepoId::from_remote_url("git@gitlab.com:acme/service.git").unwrap();
        assert_eq!(id.host(), "gitlab.com");
        assert!(!id.is_github(), "v1 only talks to GitHub (FR-1.1)");
    }

    #[test]
    fn urls_that_are_not_repositories_are_rejected() {
        for url in [
            "/srv/git/local.git",
            "file:///srv/git/local.git",
            "git@github.com:onlyowner.git",
            "https://github.com/",
            "",
        ] {
            assert!(
                RepoId::from_remote_url(url).is_none(),
                "{url} should not parse"
            );
        }
    }

    #[test]
    fn malformed_references_are_rejected_with_a_reason() {
        assert!(matches!(
            RepoId::parse("justonename"),
            Err(RepoIdError::Malformed(_))
        ));
        assert!(matches!(
            RepoId::parse("a/b/c/d"),
            Err(RepoIdError::Malformed(_))
        ));
        let error = RepoId::parse("justonename").unwrap_err();
        assert!(error.to_string().contains("OWNER/NAME"), "{error}");
    }

    #[test]
    fn two_clones_of_one_repository_share_a_key_and_two_repositories_do_not() {
        // FR-1.3: the key is the identity, so the clone path is irrelevant.
        let one = RepoId::parse("acme/service").unwrap();
        let two = RepoId::parse("github.com/acme/service").unwrap();
        let other = RepoId::parse("acme/other").unwrap();
        let forked_host = RepoId::parse("ghe.example.com/acme/service").unwrap();

        assert_eq!(one.key(), two.key());
        assert_ne!(one.key(), other.key());
        assert_ne!(one.key(), forked_host.key());
    }
}
