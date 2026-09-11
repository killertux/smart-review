//! Presentation layer (ARCH-1).
//!
//! The UI is a function of application state plus a stream of events. It never
//! performs IO and never calls an adapter directly: keys become actions, actions
//! mutate state and return an [`Effect`], and this loop is the only place that
//! turns an effect into IO.

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

use std::sync::mpsc::{self, Sender};

use crate::Startup;
use crate::doctor::Check;
use crate::error::Result;
use crate::logging::{self, Level};
use crate::ports::{Clock, StateStore};
pub use app::{App, Effect, Overlay, Pane};
pub use keymap::Mode;

/// Runs the interface until the user quits.
///
/// # Errors
///
/// Returns an error when the terminal cannot be taken over, drawn to, or
/// restored.
pub fn run(startup: Startup) -> Result<()> {
    // The loop owns the clock, the state file and the background jobs so the
    // reducer can stay a pure function of state.
    let clock = startup.clock;
    let state_store: Box<dyn StateStore> = Box::new(startup.state_store.clone());

    let mut app = App::new(startup)?;
    let mut terminal = terminal::TerminalGuard::enter(app.mouse_enabled())?;
    // Reports carry the job id they belong to so a superseded one is discarded.
    let (doctor_sender, doctor_receiver) = mpsc::channel::<(u64, Vec<Check>)>();

    while !app.should_quit() {
        app.set_now(clock.now_unix_secs());
        terminal.draw(|frame| app.render(frame))?;
        app.tick();

        // Results from background jobs.
        loop {
            match doctor_receiver.try_recv() {
                Ok((job, checks)) => app.apply_checks(job, checks),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // The worker died without reporting; stop waiting for it.
                    app.abort_doctor_job();
                    break;
                }
            }
        }

        let effect = if event::poll(app.poll_timeout())? {
            match event::read()? {
                event::Event::Key(key) if key.kind != event::KeyEventKind::Release => {
                    app.on_key(key)
                }
                event::Event::Mouse(mouse) => {
                    match mouse.kind {
                        event::MouseEventKind::ScrollDown => app.on_scroll(1),
                        event::MouseEventKind::ScrollUp => app.on_scroll(-1),
                        _ => {}
                    }
                    Effect::None
                }
                // Resizing needs no handling: the next draw reads the new size,
                // and the app keeps no cached geometry (FR-7.8).
                _ => Effect::None,
            }
        } else {
            app.on_timeout()
        };

        apply(effect, &mut app, state_store.as_ref(), &doctor_sender);
    }

    logging::log(Level::Info, "shutting down normally");
    Ok(())
}

/// Performs whatever the reducer asked for.
///
/// This is the only function in `tui` that touches the outside world, which is
/// what lets [`App::render`] stay pure and keeps the loop responsive (NFR-1.2).
fn apply(
    effect: Effect,
    app: &mut App,
    state_store: &dyn StateStore,
    doctor_sender: &Sender<(u64, Vec<Check>)>,
) {
    match effect {
        Effect::None | Effect::KeepPending => {}

        Effect::SaveState => {
            if let Err(error) = state_store.save(&app.state) {
                app.notice(
                    app::NoticeLevel::Warn,
                    format!("could not save state: {error}"),
                );
            }
        }

        Effect::RunDoctor => {
            let job = app.doctor_job();
            let request = app.doctor_request();
            let sender = doctor_sender.clone();
            // The probes run `git` and `gh`, so they must not run on the event
            // loop (FR-9.3, NFR-1.2). The report comes back over the channel.
            let _ = std::thread::spawn(move || {
                let checks = crate::doctor::collect(&request);
                let _ = sender.send((job, checks));
            });
        }
    }
}
