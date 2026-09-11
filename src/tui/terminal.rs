//! Terminal lifecycle (FR-9.1, NFR-4.2).
//!
//! Entering raw mode, the alternate screen and mouse capture is expressed as one
//! RAII guard, so there is exactly one place that can put the terminal back. A
//! panic hook restores it before the panic message is printed.

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
static ACTIVE: AtomicBool = AtomicBool::new(false);
static HOOK: Once = Once::new();

/// Owns the terminal for its lifetime and restores it when dropped.
pub(crate) struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    /// Takes over the terminal.
    pub(crate) fn enter(mouse: bool) -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;

        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide)?;
        if mouse {
            execute!(stdout, EnableMouseCapture)?;
        }

        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        ACTIVE.store(true, Ordering::SeqCst);
        Ok(Self { terminal })
    }

    /// Draws one frame.
    pub(crate) fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) -> io::Result<()> {
        self.terminal.draw(render).map(|_| ())
    }
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
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut stdout = io::stdout();
    let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
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
    fn restore_is_idempotent_and_safe_without_a_guard() {
        // Nothing was entered, so this must be a no-op rather than an error.
        restore();
        restore();
        assert!(!ACTIVE.load(Ordering::SeqCst));
    }
}
