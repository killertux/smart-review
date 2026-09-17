//! Opening user-visible web links outside the terminal (IR-10).
//!
//! The reducer returns an effect and this small boundary performs the platform action.
//! Keeping the argv construction behind [`ExternalLink`] makes the policy testable
//! without starting a browser.

use std::io;
use std::process::Command;

/// Opens a validated web link using an argv-based platform command.
pub trait ExternalLink {
    /// Opens `url` outside the terminal.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system cannot start the link opener.
    fn open(&self, url: &str) -> io::Result<()>;
}

/// The operating system's desktop link opener.
#[derive(Debug)]
pub struct SystemExternalLink;

impl ExternalLink for SystemExternalLink {
    fn open(&self, url: &str) -> io::Result<()> {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        Command::new(program).arg(url).spawn().map(|_| ())
    }
}

/// Applies the supported-scheme policy before calling an external opener.
///
/// # Errors
///
/// Returns an error from `opener` only after accepting an HTTP(S) URL.
pub fn open_web_url(opener: &dyn ExternalLink, url: &str) -> io::Result<bool> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Ok(false);
    }
    opener.open(url).map(|()| true)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::{ExternalLink, open_web_url};

    #[derive(Default)]
    struct FakeLink {
        opened: RefCell<Vec<String>>,
    }

    impl ExternalLink for FakeLink {
        fn open(&self, url: &str) -> std::io::Result<()> {
            self.opened.borrow_mut().push(url.to_owned());
            Ok(())
        }
    }

    #[test]
    fn ir_10_only_web_urls_reach_the_external_link_boundary() {
        let fake = FakeLink::default();
        assert!(open_web_url(&fake, "https://example.test/run/1").expect("fake opens"));
        assert!(!open_web_url(&fake, "file:///etc/passwd").expect("policy rejects"));
        assert_eq!(fake.opened.into_inner(), vec!["https://example.test/run/1"]);
    }
}
