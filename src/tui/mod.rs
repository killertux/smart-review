//! Presentation layer (ARCH-1).
//!
//! The UI is a function of application state plus a stream of events. It never
//! performs IO and never calls an adapter directly: keys become actions, actions
//! mutate state, and rendering reads that state.

pub mod action;
pub mod app;
pub mod components;
pub mod event;
pub mod keymap;
pub mod layout;
pub mod terminal;
pub mod theme;
pub mod update;

#[cfg(test)]
pub(crate) mod test_support;

pub use app::{App, Overlay, Pane};
pub use keymap::Mode;

use crate::Startup;
use crate::error::Result;
use crate::logging::{self, Level};

/// Runs the interface until the user quits.
///
/// # Errors
///
/// Returns an error when the terminal cannot be taken over, drawn to, or
/// restored.
pub fn run(startup: Startup) -> Result<()> {
    let mut app = App::new(startup)?;
    let mut terminal = terminal::TerminalGuard::enter(app.mouse_enabled())?;

    while !app.should_quit() {
        terminal.draw(|frame| app.render(frame))?;
        app.tick();

        if event::poll(app.poll_timeout())? {
            match event::read()? {
                event::Event::Key(key) if key.kind != event::KeyEventKind::Release => {
                    app.on_key(key);
                }
                event::Event::Mouse(mouse) => match mouse.kind {
                    event::MouseEventKind::ScrollDown => app.on_scroll(1),
                    event::MouseEventKind::ScrollUp => app.on_scroll(-1),
                    _ => {}
                },
                // Resizing needs no handling: the next draw reads the new size,
                // and the app keeps no cached geometry (FR-7.8).
                _ => {}
            }
        } else {
            app.on_timeout();
        }
    }

    logging::log(Level::Info, "shutting down normally");
    Ok(())
}
