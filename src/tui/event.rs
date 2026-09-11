//! A thin façade over the terminal event types.
//!
//! Everything else in `tui` goes through this module so that swapping the
//! backend later touches one file. Events are delivered to [`crate::tui::app::App`]
//! and never handled here (ARCH-5).

pub use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind, poll, read,
};
