//! Application state and the event reducer (ARCH-1, ARCH-5).
//!
//! State is owned by a single thread: keys arrive, become actions, and mutate
//! this struct. Nothing here performs IO, so the event loop can never block
//! (NFR-1.2).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};

use crate::Startup;
use crate::adapters::clock::SystemClock;
use crate::adapters::fs::TomlStateStore;
use crate::config::{Config, ConfigDocument};
use crate::doctor::{self, Check, Context};
use crate::error::Result;
use crate::logging::{self, Level};
use crate::paths::Home;
use crate::ports::{Clock, StateStore};
use crate::state::AppState;
use crate::tui::components;
use crate::tui::event::{KeyCode, KeyEvent, KeyModifiers};
use crate::tui::keymap::{self, KeyCombo, Keymap, Mode, Resolution};
use crate::tui::layout;
use crate::tui::theme::{self, Theme};
use crate::tui::update;

/// How long a notification stays in the status line (FR-7.6).
const NOTICE_LIFETIME: Duration = Duration::from_secs(6);
/// Poll interval when nothing is pending.
const IDLE_POLL: Duration = Duration::from_millis(250);
/// Keep at most this many notifications queued.
const MAX_NOTICES: usize = 3;

/// What this build can show, so the shell is honest about being a shell.
pub(crate) const ROADMAP: &[&str] = &[
    "M1  browse, filter and search pull requests",
    "M1  read the diff with vim motions and the mouse",
    "M2  a managed worktree per pull request",
    "M2  pick provider, model and thinking in the TUI",
    "M2  streamed analysis and a review-ordered diff",
    "M3  a persistent chat about the pull request",
    "M4  inline comments and one batched review",
    "M5  polish, docs and release builds",
];

/// The focused pane (FR-7.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    /// The pull request list (M1).
    #[default]
    PullRequests,
    /// The diff and review pane (M1).
    Diff,
}

impl Pane {
    /// The other pane.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::PullRequests => Self::Diff,
            Self::Diff => Self::PullRequests,
        }
    }

    /// Name used in the status line.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PullRequests => "list",
            Self::Diff => "diff",
        }
    }
}

/// The popup that currently owns the screen, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overlay {
    /// Nothing is open.
    #[default]
    None,
    /// The help popup (FR-7.3).
    Help,
    /// The leader menu (FR-7.3).
    Leader,
    /// Environment checks (FR-9.3).
    Doctor,
    /// Theme selection (FR-7.7).
    ThemePicker,
}

/// Severity of a status line message (FR-7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    /// Neutral information.
    Info,
    /// Something is off but the app still works.
    Warn,
    /// Something failed.
    Error,
}

/// A transient status line message.
#[derive(Debug, Clone)]
pub struct Notice {
    /// Severity, which selects the style.
    pub level: NoticeLevel,
    /// The message.
    pub text: String,
    expires: Instant,
}

/// The `:` command line buffer (FR-7.4).
#[derive(Debug, Clone, Default)]
pub struct CommandLine {
    /// What the user has typed.
    pub input: String,
    /// Why the last command was rejected.
    pub error: Option<String>,
}

impl CommandLine {
    /// Empties the buffer and clears any error.
    pub fn clear(&mut self) {
        self.input.clear();
        self.error = None;
    }

    /// Appends a character.
    pub fn push(&mut self, value: char) {
        self.input.push(value);
        self.error = None;
    }

    /// Removes the last character.
    pub fn backspace(&mut self) {
        self.input.pop();
        self.error = None;
    }
}

/// Everything the interface needs to know.
#[derive(Debug)]
pub struct App {
    /// The application's private directory.
    pub(crate) home: Home,
    /// Effective settings.
    pub(crate) config: Config,
    /// The parsed config file, kept for a future write-back (FR-8.6).
    pub(crate) document: ConfigDocument,
    /// Where the config file lives.
    pub(crate) config_path: PathBuf,
    /// Whether a config file was read.
    pub(crate) config_exists: bool,
    /// `file` or `built-in defaults`.
    pub(crate) config_source: &'static str,
    /// The keybinding engine.
    pub(crate) keymap: Keymap,
    /// The active theme.
    pub(crate) theme: Theme,
    /// Where the theme came from.
    pub(crate) theme_source: String,
    /// Persisted state.
    pub(crate) state: AppState,
    /// Warnings collected during startup and at runtime.
    pub(crate) warnings: Vec<String>,
    /// `--repo`, if given.
    pub(crate) repo: Option<String>,
    /// `--pr`, if given.
    pub(crate) requested_pr: Option<u64>,
    /// `--path`, if given.
    pub(crate) requested_path: Option<PathBuf>,
    /// `--remote`, if given.
    pub(crate) remote: Option<String>,
    /// Injected time source.
    pub(crate) clock: SystemClock,
    /// The state store in use.
    pub(crate) state_store: TomlStateStore,

    /// Current input mode (FR-7.1).
    pub(crate) mode: Mode,
    /// The open popup, if any.
    pub(crate) overlay: Overlay,
    /// Keys pressed so far in a multi-key sequence (FR-7.2).
    pub(crate) pending: Vec<KeyCombo>,
    /// When the pending sequence becomes unambiguous.
    pub(crate) deadline: Option<Instant>,
    /// Whether the leader menu is waiting for a continuation.
    pub(crate) leader_open: bool,
    /// The command line buffer.
    pub(crate) command: CommandLine,
    /// Recent notifications.
    pub(crate) notices: Vec<Notice>,
    /// Cursor over [`ROADMAP`].
    pub(crate) cursor: usize,
    /// Cursor inside the theme picker.
    pub(crate) picker_cursor: usize,
    /// The focused pane.
    pub(crate) focus: Pane,
    /// The last doctor report, computed when the popup opens.
    pub(crate) checks: Vec<Check>,
    /// Set when the user asks to quit.
    pub(crate) quit: bool,
    /// Unix time the interface started, for the uptime display.
    started_at: u64,
}

impl App {
    /// Builds the app from a resolved startup.
    ///
    /// # Errors
    ///
    /// Returns an error when the startup state cannot be turned into an app.
    pub fn new(startup: Startup) -> Result<Self> {
        let Startup {
            home,
            config,
            document,
            config_path,
            config_exists,
            config_source,
            keymap,
            theme,
            theme_source,
            state,
            warnings,
            repo,
            pr,
            path,
            remote,
            clock,
            state_store,
        } = startup;

        let focus = match state.focus.as_deref() {
            Some("diff") => Pane::Diff,
            _ => Pane::PullRequests,
        };

        let started_at = clock.now_unix_secs();

        let mut app = Self {
            home,
            config,
            document,
            config_path,
            config_exists,
            config_source,
            keymap,
            theme,
            theme_source,
            state,
            warnings,
            repo,
            requested_pr: pr,
            requested_path: path,
            remote,
            clock,
            state_store,
            started_at,
            mode: Mode::Normal,
            overlay: Overlay::None,
            pending: Vec::new(),
            deadline: None,
            leader_open: false,
            command: CommandLine::default(),
            notices: Vec::new(),
            cursor: 0,
            picker_cursor: 0,
            focus,
            checks: Vec::new(),
            quit: false,
        };
        app.report_startup_warnings();
        Ok(app)
    }

    /// Seconds since the interface started, used by the shell status pane.
    pub(crate) fn uptime_secs(&self) -> u64 {
        self.clock.now_unix_secs().saturating_sub(self.started_at)
    }

    /// How many keys the configuration document holds, including ones this build
    /// does not understand (FR-8.6).
    pub(crate) fn document_key_count(&self) -> usize {
        self.document.value().values().map(count_keys).sum()
    }

    /// Whether the user asked to quit.
    #[must_use]
    pub const fn should_quit(&self) -> bool {
        self.quit
    }

    /// Whether mouse capture should be enabled (FR-7.5).
    #[must_use]
    pub const fn mouse_enabled(&self) -> bool {
        self.config.ui.mouse
    }

    /// The active input mode (FR-7.1).
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// The open popup, if any.
    #[must_use]
    pub const fn overlay(&self) -> Overlay {
        self.overlay
    }

    /// The focused pane.
    #[must_use]
    pub const fn focus(&self) -> Pane {
        self.focus
    }

    /// The most recent notification, which is what the status line shows.
    #[must_use]
    pub fn latest_notice(&self) -> Option<&Notice> {
        self.notices.last()
    }

    /// The doctor report.
    pub(crate) fn checks(&self) -> &[Check] {
        &self.checks
    }

    /// The theme picker cursor.
    pub(crate) const fn picker_cursor(&self) -> usize {
        self.picker_cursor
    }

    /// Theme names the user can choose (FR-7.7).
    pub(crate) fn theme_names(&self) -> Vec<String> {
        theme::available(&self.home)
    }

    /// Everything the doctor needs, borrowed from this app.
    pub(crate) fn doctor_context(&self) -> Context<'_> {
        Context {
            home: &self.home,
            config: &self.config,
            config_path: &self.config_path,
            config_exists: self.config_exists,
            warnings: &self.warnings,
            keymap: &self.keymap,
            theme: &self.theme,
            theme_source: &self.theme_source,
        }
    }

    /// Adds a notification to the status line (FR-7.6).
    pub fn notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        let text = text.into();
        logging::log(
            match level {
                NoticeLevel::Info => Level::Info,
                NoticeLevel::Warn => Level::Warn,
                NoticeLevel::Error => Level::Error,
            },
            &text,
        );
        if self.notices.len() >= MAX_NOTICES {
            self.notices.remove(0);
        }
        self.notices.push(Notice {
            level,
            text,
            expires: Instant::now() + NOTICE_LIFETIME,
        });
    }

    /// Records a command line error, which stays until the next keystroke.
    pub(crate) fn command_error(&mut self, message: impl Into<String>) {
        self.command.error = Some(message.into());
    }

    /// Requests a clean shutdown.
    pub(crate) fn quit(&mut self) {
        self.quit = true;
    }

    /// Switches theme, remembering the choice (FR-7.7, FR-8.5).
    pub(crate) fn set_theme(&mut self, name: &str) {
        let mut warnings = Vec::new();
        match theme::load(&self.home, name, &mut warnings) {
            Ok((theme, source)) => {
                self.theme = theme;
                self.theme_source = source;
                for warning in warnings {
                    self.notice(NoticeLevel::Warn, warning);
                }
                self.state.theme = Some(name.to_owned());
                if let Err(error) = self.state_store.save(&self.state) {
                    self.notice(
                        NoticeLevel::Warn,
                        format!("could not remember the theme: {error}"),
                    );
                }
                self.notice(NoticeLevel::Info, format!("theme: {name}"));
            }
            Err(error) => self.notice(NoticeLevel::Error, error.to_string()),
        }
    }

    /// Reloads keybindings from disk, after `:set ui.leader=…`.
    pub(crate) fn reload_keymap(&mut self) {
        let mut warnings = Vec::new();
        match keymap::load(&self.home, &self.config.ui, &mut warnings) {
            Ok(keymap) => self.keymap = keymap,
            Err(error) => self.notice(NoticeLevel::Error, error.to_string()),
        }
        for warning in warnings {
            self.notice(NoticeLevel::Warn, warning);
        }
    }

    /// Opens a popup, taking the keyboard when the popup is modal.
    pub(crate) fn open_overlay(&mut self, overlay: Overlay) {
        match overlay {
            Overlay::Leader => {
                self.overlay = Overlay::Leader;
                self.leader_open = true;
                self.mode = Mode::Normal;
            }
            Overlay::None => {
                self.overlay = Overlay::None;
                self.leader_open = false;
                self.mode = Mode::Normal;
            }
            other => {
                self.overlay = other;
                self.leader_open = false;
                self.mode = Mode::Popup;
            }
        }

        if self.overlay == Overlay::Doctor {
            self.checks = doctor::collect(&self.doctor_context());
        }
        if self.overlay == Overlay::ThemePicker {
            let names = self.theme_names();
            self.picker_cursor = names
                .iter()
                .position(|candidate| candidate == self.theme.name())
                .unwrap_or(0);
        }
    }

    /// Closes whatever popup is open.
    pub(crate) fn close_overlay(&mut self) {
        self.open_overlay(Overlay::None);
    }

    /// How long to wait for more input.
    #[must_use]
    pub fn poll_timeout(&self) -> Duration {
        match self.deadline {
            Some(deadline) => deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1)),
            None => IDLE_POLL,
        }
    }

    /// Expires old notifications.
    pub fn tick(&mut self) {
        let now = Instant::now();
        self.notices.retain(|notice| notice.expires > now);
    }

    /// Handles one key press (FR-7.1, FR-7.2).
    pub fn on_key(&mut self, event: KeyEvent) {
        let combo = keymap::normalize(KeyCombo::from(event));
        match self.mode {
            Mode::Command => self.on_command_key(combo),
            Mode::Popup => self.on_popup_key(combo),
            _ => self.on_normal_key(combo),
        }
    }

    /// Scrolls the roadmap cursor with the mouse wheel (FR-7.5).
    pub fn on_scroll(&mut self, delta: i32) {
        if self.mode == Mode::Popup {
            if self.overlay == Overlay::ThemePicker {
                self.move_picker(delta);
            }
            return;
        }
        self.move_cursor(delta);
    }

    /// Fires a pending sequence once its timeout expires (FR-7.2).
    pub fn on_timeout(&mut self) {
        self.deadline = None;
        if self.pending.is_empty() || self.leader_open {
            return;
        }
        let Some(action) = self.pending_action() else {
            self.pending.clear();
            return;
        };
        let keep = update::dispatch(self, &action);
        if !keep {
            self.pending.clear();
        }
    }

    fn report_startup_warnings(&mut self) {
        let warnings = std::mem::take(&mut self.warnings);
        for warning in &warnings {
            logging::log(Level::Warn, warning);
        }
        for warning in warnings.into_iter().take(MAX_NOTICES) {
            self.notice(NoticeLevel::Warn, warning);
        }
    }

    fn on_normal_key(&mut self, combo: KeyCombo) {
        // Esc is a universal cancel: it drops a half-typed sequence and closes a
        // popup before it means anything else (FR-7.1).
        if combo.code == KeyCode::Esc
            && (!self.pending.is_empty() || self.leader_open || self.overlay != Overlay::None)
        {
            self.pending.clear();
            self.deadline = None;
            self.leader_open = false;
            self.overlay = Overlay::None;
            return;
        }

        self.pending.push(combo);
        self.advance();
    }

    fn advance(&mut self) {
        let timeout = self.keymap.timeout();
        let next = match self.keymap.resolve(Mode::Normal, &self.pending) {
            Resolution::Match(binding) => Some(binding.action.clone()),
            Resolution::Ambiguous(_) | Resolution::Prefix => {
                self.deadline = Some(Instant::now() + timeout);
                return;
            }
            Resolution::None => None,
        };

        if let Some(action) = next {
            let keep = update::dispatch(self, &action);
            self.deadline = None;
            if !keep {
                self.pending.clear();
                self.leader_open = false;
            }
        } else {
            let keys = keymap::describe_sequence(&self.pending);
            let was_leader = self.leader_open;
            self.pending.clear();
            self.deadline = None;
            self.leader_open = false;
            if was_leader {
                self.notice(NoticeLevel::Info, format!("`{keys}` is not bound"));
            } else {
                self.notice(NoticeLevel::Warn, format!("`{keys}` is not bound"));
            }
        }
    }

    /// The action the pending sequence would fire on timeout.
    fn pending_action(&self) -> Option<String> {
        match self.keymap.resolve(Mode::Normal, &self.pending) {
            Resolution::Match(binding) | Resolution::Ambiguous(binding) => {
                Some(binding.action.clone())
            }
            Resolution::Prefix | Resolution::None => None,
        }
    }

    fn on_popup_key(&mut self, combo: KeyCombo) {
        match combo.code {
            KeyCode::Esc => self.close_overlay(),
            KeyCode::Char('q') if combo.modifiers.is_empty() => self.close_overlay(),
            KeyCode::Char('j') | KeyCode::Down if self.overlay == Overlay::ThemePicker => {
                self.move_picker(1);
            }
            KeyCode::Char('k') | KeyCode::Up if self.overlay == Overlay::ThemePicker => {
                self.move_picker(-1);
            }
            KeyCode::Enter if self.overlay == Overlay::ThemePicker => {
                if let Some(name) = self.theme_names().get(self.picker_cursor).cloned() {
                    self.close_overlay();
                    self.set_theme(&name);
                }
            }
            _ => {}
        }
    }

    fn move_picker(&mut self, delta: i32) {
        let count = self.theme_names().len();
        if count == 0 {
            return;
        }
        self.picker_cursor = clamp_cursor(self.picker_cursor, delta, count);
    }

    /// Moves the roadmap cursor.
    pub(crate) fn move_cursor(&mut self, delta: i32) {
        self.cursor = clamp_cursor(self.cursor, delta, ROADMAP.len());
    }

    /// Moves the roadmap cursor to the ends.
    pub(crate) fn move_cursor_to(&mut self, last: bool) {
        self.cursor = if last {
            ROADMAP.len().saturating_sub(1)
        } else {
            0
        };
    }

    fn on_command_key(&mut self, combo: KeyCombo) {
        match combo.code {
            KeyCode::Esc => {
                self.command.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                let input = std::mem::take(&mut self.command.input);
                self.command.error = None;
                self.mode = Mode::Normal;
                update::command(self, &input);
            }
            KeyCode::Backspace => self.command.backspace(),
            KeyCode::Tab => {
                let completed = update::complete_command(&self.command.input);
                self.command.input = completed;
            }
            KeyCode::Char(value)
                if combo.modifiers.is_empty() || combo.modifiers == KeyModifiers::SHIFT =>
            {
                self.command.push(value);
            }
            _ => {}
        }
    }

    /// Renders a frame (FR-7.8).
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        if layout::is_too_small(area) {
            components::panes::render_too_small(frame, area, &self.theme);
            return;
        }

        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        components::header::render(frame, rows[0], self);
        components::panes::render(frame, rows[1], self);
        components::status_line::render(frame, rows[2], self);
        components::command_line::render(frame, rows[3], self);
        components::render_overlay(frame, area, self);
    }
}

fn clamp_cursor(current: usize, delta: i32, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let last = count - 1;
    let step = delta.unsigned_abs() as usize;
    let next = if delta.is_negative() {
        current.saturating_sub(step)
    } else {
        current.saturating_add(step)
    };
    next.min(last)
}

/// Counts the keys in a value, descending one level into tables.
fn count_keys(value: &toml::Value) -> usize {
    match value.as_table() {
        Some(table) => table.values().map(count_keys).sum(),
        None => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use crate::test_support::{TempHome, temp_home};

    fn app() -> (TempHome, App) {
        let dir = temp_home();
        let cli = Cli {
            repo: None,
            pr: None,
            path: None,
            remote: None,
            config: None,
            theme: None,
            home: Some(dir.path().to_path_buf()),
            log_level: None,
            check: false,
        };
        let startup = Startup::load(&cli).unwrap();
        (dir, App::new(startup).unwrap())
    }

    fn press(app: &mut App, keys: &str) {
        let leader = app.keymap.leader();
        for combo in keymap::parse_keys(keys, &leader).unwrap() {
            app.on_key(KeyEvent::new(combo.code, combo.modifiers));
        }
    }

    #[test]
    fn starts_in_normal_mode_with_nothing_open() {
        let (_dir, app) = app();
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.overlay(), Overlay::None);
        assert!(!app.should_quit());
    }

    #[test]
    fn ctrl_c_quits() {
        let (_dir, mut app) = app();
        press(&mut app, "<C-c>");
        assert!(app.should_quit());
    }

    #[test]
    fn question_mark_opens_help() {
        let (_dir, mut app) = app();
        press(&mut app, "?");
        assert_eq!(app.overlay(), Overlay::Help);
        assert_eq!(app.mode(), Mode::Popup);
    }

    #[test]
    fn escape_closes_a_popup() {
        let (_dir, mut app) = app();
        press(&mut app, "?");
        press(&mut app, "<Esc>");
        assert_eq!(app.overlay(), Overlay::None);
        assert_eq!(app.mode(), Mode::Normal);
    }

    #[test]
    fn colon_opens_the_command_line_and_escape_cancels_it() {
        let (_dir, mut app) = app();
        press(&mut app, ":");
        assert_eq!(app.mode(), Mode::Command);
        press(&mut app, "q");
        assert_eq!(app.command.input, "q");
        press(&mut app, "<Esc>");
        assert_eq!(app.mode(), Mode::Normal);
        assert!(app.command.input.is_empty());
        assert!(!app.should_quit());
    }

    #[test]
    fn colon_q_quits() {
        let (_dir, mut app) = app();
        press(&mut app, ":");
        press(&mut app, "q");
        press(&mut app, "<CR>");
        assert!(app.should_quit());
    }

    #[test]
    fn navigation_moves_the_cursor_and_stops_at_the_ends() {
        let (_dir, mut app) = app();
        assert_eq!(app.cursor, 0);
        press(&mut app, "j");
        assert_eq!(app.cursor, 1);
        press(&mut app, "k");
        press(&mut app, "k");
        assert_eq!(app.cursor, 0, "the cursor should not go negative");
        press(&mut app, "G");
        assert_eq!(app.cursor, ROADMAP.len() - 1);
        press(&mut app, "g");
        press(&mut app, "g");
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn a_two_key_sequence_fires_on_its_own() {
        let (_dir, mut app) = app();
        press(&mut app, "G");
        assert_eq!(app.cursor, ROADMAP.len() - 1);
        press(&mut app, "gg");
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn an_unbound_key_reports_itself_and_clears_the_sequence() {
        let (_dir, mut app) = app();
        press(&mut app, "z");
        assert!(app.pending.is_empty());
        let notice = app.latest_notice().expect("a notice should be shown");
        assert!(notice.text.contains('z'), "{:?}", notice.text);
        assert_eq!(notice.level, NoticeLevel::Warn);
    }

    #[test]
    fn the_leader_menu_waits_and_then_opens() {
        let (_dir, mut app) = app();
        press(&mut app, "<Space>");
        // Ambiguous: the leader is a prefix of longer bindings, so it waits.
        assert!(app.pending.len() == 1);
        assert!(!app.leader_open);
        app.on_timeout();
        assert!(app.leader_open);
        assert_eq!(app.overlay(), Overlay::Leader);
        // The pending sequence survives so a continuation still works.
        press(&mut app, "?");
        assert_eq!(app.overlay(), Overlay::Help);
        assert!(app.pending.is_empty());
    }

    #[test]
    fn escape_closes_the_leader_menu() {
        let (_dir, mut app) = app();
        press(&mut app, "<Space>");
        app.on_timeout();
        assert!(app.leader_open);
        press(&mut app, "<Esc>");
        assert!(!app.leader_open);
        assert!(app.pending.is_empty());
        assert_eq!(app.overlay(), Overlay::None);
    }

    #[test]
    fn an_unknown_leader_continuation_is_reported() {
        let (_dir, mut app) = app();
        press(&mut app, "<Space>");
        app.on_timeout();
        press(&mut app, "z");
        assert!(!app.leader_open);
        assert!(app.latest_notice().is_some());
    }

    #[test]
    fn theme_picker_applies_the_selected_theme_and_remembers_it() {
        let (_dir, mut app) = app();
        press(&mut app, "<Space>");
        press(&mut app, "t");
        assert_eq!(app.overlay(), Overlay::ThemePicker);

        // Move to "light" and confirm.
        press(&mut app, "j");
        press(&mut app, "<CR>");
        assert_eq!(app.theme.name(), "light");
        assert_eq!(app.state.theme.as_deref(), Some("light"));
        assert_eq!(app.overlay(), Overlay::None);

        // The choice is on disk, so a restart picks it up.
        let saved = crate::state::load(&app.home.state()).unwrap();
        assert_eq!(saved.theme.as_deref(), Some("light"));
    }

    #[test]
    fn doctor_collects_a_report() {
        let (_dir, mut app) = app();
        press(&mut app, ":");
        for character in "doctor".chars() {
            press(&mut app, &character.to_string());
        }
        press(&mut app, "<CR>");
        assert_eq!(app.overlay(), Overlay::Doctor);
        assert!(!app.checks().is_empty());
    }

    #[test]
    fn set_command_changes_the_timeout() {
        let (_dir, mut app) = app();
        assert_eq!(app.keymap.timeout(), Duration::from_millis(500));
        press(&mut app, ":");
        for character in "set ui.timeoutlen=120".chars() {
            press(&mut app, &character.to_string());
        }
        press(&mut app, "<CR>");
        assert_eq!(app.keymap.timeout(), Duration::from_millis(120));
        assert!(app.command.error.is_none());
    }

    #[test]
    fn a_bad_set_command_reports_an_error() {
        let (_dir, mut app) = app();
        press(&mut app, ":");
        for character in "set nonsense=1".chars() {
            press(&mut app, &character.to_string());
        }
        press(&mut app, "<CR>");
        let error = app.command.error.clone().expect("an error should be shown");
        assert!(error.contains("nonsense"), "{error}");
    }

    #[test]
    fn an_unknown_command_reports_an_error() {
        let (_dir, mut app) = app();
        press(&mut app, ":");
        for character in "bogus".chars() {
            press(&mut app, &character.to_string());
        }
        press(&mut app, "<CR>");
        assert!(app.command.error.is_some());
    }

    #[test]
    fn tab_completes_a_command() {
        let (_dir, mut app) = app();
        press(&mut app, ":");
        for character in "doc".chars() {
            press(&mut app, &character.to_string());
        }
        press(&mut app, "<Tab>");
        assert_eq!(app.command.input, "doctor");
    }

    #[test]
    fn notices_expire() {
        let (_dir, mut app) = app();
        app.notice(NoticeLevel::Info, "hello");
        assert!(app.latest_notice().is_some());
        app.notices.clear();
        app.tick();
        assert!(app.latest_notice().is_none());
    }

    #[test]
    fn the_mouse_wheel_moves_the_cursor() {
        let (_dir, mut app) = app();
        app.on_scroll(1);
        assert_eq!(app.cursor, 1);
        app.on_scroll(-1);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn pane_focus_cycles() {
        let (_dir, mut app) = app();
        assert_eq!(app.focus(), Pane::PullRequests);
        press(&mut app, "<Tab>");
        assert_eq!(app.focus(), Pane::Diff);
        press(&mut app, "<S-Tab>");
        assert_eq!(app.focus(), Pane::PullRequests);
    }

    #[test]
    fn every_registered_action_is_dispatched() {
        // Guards the registry against drifting from the dispatcher: an action
        // with no match arm falls through to the catch-all notice (FR-7.3).
        for definition in crate::tui::action::all() {
            let (_dir, mut app) = app();
            crate::tui::update::dispatch(&mut app, definition.id);
            if let Some(notice) = app.latest_notice() {
                assert!(
                    !notice.text.contains("not implemented"),
                    "`{}` reaches the catch-all: {}",
                    definition.id,
                    notice.text
                );
            }
        }
    }

    #[test]
    fn every_command_reachable_from_the_registry_resolves() {
        for definition in crate::tui::action::all() {
            assert!(
                crate::tui::update::COMMANDS
                    .iter()
                    .any(|(_, action)| *action == definition.id)
                    || definition.id.starts_with("nav.")
                    || definition.id.starts_with("pane.")
                    || definition.id == "app.cancel"
                    || definition.id == "app.leader_menu"
                    || definition.id == "theme.use_dark"
                    || definition.id == "theme.use_light",
                "`{}` has neither a command nor a documented binding path",
                definition.id
            );
        }
    }
}
