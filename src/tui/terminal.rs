//! Terminal lifecycle (FR-9.1, NFR-4.2).
//!
//! Entering raw mode, the alternate screen and mouse capture is expressed as one
//! RAII guard, so there is exactly one place that can put the terminal back. A
//! panic hook restores it before the panic message is printed.
//!
//! The takeover flag is set as soon as raw mode is on — before any escape
//! sequence — so a failure *during* the takeover still has a way back. That is
//! the difference between a stray error message and a shell that no longer echoes
//! until the user runs `reset`.

use std::io::{self, Stdout};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::{Frame, Terminal};

/// Whether the terminal is currently in raw mode and on the alternate screen.
///
/// Split out from the free functions so the acquire/release contract is testable
/// without taking over a real terminal.
#[derive(Debug, Default)]
struct Takeover {
    active: AtomicBool,
}

impl Takeover {
    const fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
        }
    }

    /// Records that the terminal is now ours.
    fn begin(&self) {
        self.active.store(true, Ordering::SeqCst);
    }

    /// Clears the flag, reporting whether this call is the one that has to undo
    /// the takeover. Every later call gets `false`, which is what makes restoring
    /// idempotent.
    fn finish(&self) -> bool {
        self.active.swap(false, Ordering::SeqCst)
    }

    #[cfg(test)]
    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

static TAKEOVER: Takeover = Takeover::new();
static HOOK: Once = Once::new();

/// Owns the terminal for its lifetime and restores it when dropped.
#[derive(Debug)]
pub(crate) struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    /// Takes over the terminal.
    ///
    /// Any failure after raw mode is enabled restores the terminal before
    /// returning the error (FR-9.1).
    pub(crate) fn enter(mouse: bool) -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        // From here on the terminal must be given back on every path.
        TAKEOVER.begin();

        match setup(mouse) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                restore();
                Err(error)
            }
        }
    }

    /// Draws one frame.
    pub(crate) fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) -> io::Result<()> {
        self.terminal.draw(render).map(|_| ())
    }
}

fn setup(mouse: bool) -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide)?;
    if mouse {
        execute!(stdout, EnableMouseCapture)?;
    }
    Terminal::new(CrosstermBackend::new(io::stdout()))
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Puts the terminal back into a usable state.
///
/// Idempotent: the panic hook and the guard both call it, and whichever runs
/// second does nothing (NFR-4.2). Failures are ignored because there is nothing
/// useful left to do at that point.
pub(crate) fn restore() {
    if !TAKEOVER.finish() {
        return;
    }
    let mut stdout = io::stdout();
    let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}

/// Whether the terminal is currently taken over.
#[cfg(test)]
pub(crate) fn is_active() -> bool {
    TAKEOVER.is_active()
}

fn install_panic_hook() {
    HOOK.call_once(|| {
        // Chain rather than replace: a panic message should still be printed, but
        // only after the terminal has been handed back to the user.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            crate::logging::log(crate::logging::Level::Error, format!("panic: {info}"));
            restore();
            previous(info);
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_takeover_flag_is_a_one_shot_latch() {
        let takeover = Takeover::new();
        assert!(!takeover.is_active());

        // Restoring when nothing was taken over must not try to undo anything.
        assert!(!takeover.finish());
        assert!(!takeover.is_active());

        takeover.begin();
        assert!(takeover.is_active());
        assert!(takeover.finish(), "the first restore has to do the work");
        assert!(!takeover.is_active());

        // A second restore — the guard's Drop after the panic hook already ran,
        // for example — must be a no-op rather than a double restore.
        assert!(!takeover.finish());
        assert!(!TAKEOVER.is_active());
    }

    #[test]
    fn restoring_the_real_flag_is_safe_without_a_guard() {
        // Nothing has been taken over in the test process, so this only proves
        // the early return does not panic. The real restore path is covered by
        // the pty check in scripts/validate/m0.sh.
        restore();
        restore();
        assert!(!is_active());
    }
}
