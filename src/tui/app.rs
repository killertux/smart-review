//! Application state and the event reducer (ARCH-1, ARCH-5).
//!
//! State is owned by one thread: keys arrive, become actions, and mutate this
//! struct. The reducer never performs IO — not even the adapter calls it used to
//! make — and instead returns an [`Effect`] that the event loop in
//! [`crate::tui::run`] applies. That keeps rendering a pure function of state
//! (ARCHITECTURE §4) and keeps the event loop responsive (NFR-1.2).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};

use crate::Startup;
use crate::config::{Config, ConfigDocument};
use crate::doctor::{Check, Context};
use crate::error::Result;
use crate::logging::{self, Level};
use crate::paths::Home;
use crate::state::AppState;
use crate::tui::components;
use crate::tui::event::{KeyCode, KeyEvent, KeyModifiers};
use crate::tui::keymap::{self, KeyCombo, Keymap, Mode, Resolution};
use crate::tui::layout;
use crate::tui::theme::{self, Theme};
use crate::tui::update;

/// How long an informational notification stays in the status line (FR-7.6).
const NOTICE_LIFETIME: Duration = Duration::from_secs(6);
/// Poll interval when nothing is pending.
const IDLE_POLL: Duration = Duration::from_millis(250);
/// Keep at most this many notifications queued.
const MAX_NOTICES: usize = 3;
/// How many command candidates to offer while typing (FR-7.3).
pub(crate) const PALETTE_ROWS: usize = 3;

/// What the reducer wants the event loop to do next.
///
/// Actions are mutually exclusive in M0: the only action that keeps a pending
/// sequence is the leader menu, and the only ones that need the loop are a theme
/// change (persist it) and `:doctor` (probe off the event loop).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Effect {
    /// Nothing to do; the pending key sequence is finished with.
    #[default]
    None,
    /// Keep the pending sequence so the next key can complete it.
    KeepPending,
    /// Persist the state file.
    SaveState,
    /// Collect the environment report off the event loop.
    RunDoctor,
}

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

    /// The other pane, going backwards.
    ///
    /// Identical to [`Pane::next`] with two panes, but explicit so adding a third
    /// pane does not silently make `pane.prev` move forwards.
    #[must_use]
    pub const fn prev(self) -> Self {
        self.next()
    }

    /// Name used in the status line and in `state.toml`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PullRequests => "list",
            Self::Diff => "diff",
        }
    }

    /// Parses the name written to `state.toml`.
    fn parse(name: &str) -> Self {
        match name {
            "diff" => Self::Diff,
            _ => Self::PullRequests,
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

/// A status line message.
#[derive(Debug, Clone)]
pub struct Notice {
    /// Severity, which selects the style.
    pub level: NoticeLevel,
    /// The message.
    pub text: String,
    /// When it disappears; `None` means it stays until dismissed (FR-7.6).
    expires: Option<Instant>,
}

impl Notice {
    /// Whether the notice is still relevant at `now`.
    fn is_live(&self, now: Instant) -> bool {
        self.expires.is_none_or(|expires| expires > now)
    }
}

/// One row of the theme picker.
///
/// The theme is resolved when the picker opens, so moving the cursor never
/// touches the disk and previewing cannot fail halfway through (FR-7.7).
#[derive(Debug, Clone)]
pub(crate) struct PickerEntry {
    /// The name shown, and the one used by `:theme`.
    pub(crate) name: String,
    /// The resolved theme, when it loaded.
    pub(crate) theme: Option<Theme>,
    /// Where it came from, for the status line.
    pub(crate) source: Option<String>,
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
///
/// A flat struct on purpose: it is the reducer's working memory, and grouping the
/// flags into sub-structs would hide the transitions that the tests assert on.
#[allow(clippy::struct_excessive_bools)]
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
    /// The name the active theme was requested by, which is what the picker
    /// marks. It can differ from `theme.name()`: a theme file may declare its own
    /// display name.
    pub(crate) theme_request: String,
    /// Persisted state. Written by the event loop, never from the reducer.
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
    /// Themes offered by the picker, resolved when it opens so rendering never
    /// reads the filesystem (FR-7.7).
    pub(crate) picker_items: Vec<PickerEntry>,
    /// Cursor inside the theme picker.
    pub(crate) picker_cursor: usize,
    /// Theme in force when the picker opened, restored if it is cancelled.
    theme_before_picker: Option<(Theme, String)>,
    /// Restricts the help popup to one action, set by `:keymap <action>`.
    pub(crate) help_filter: Option<String>,
    /// The focused pane.
    pub(crate) focus: Pane,
    /// The last doctor report, delivered by a job (FR-9.3).
    pub(crate) checks: Vec<Check>,
    /// Whether a doctor job is in flight.
    pub(crate) doctor_running: bool,
    /// Counter used to discard a report from a superseded job.
    doctor_job: u64,
    /// Unix time as of the current loop iteration, injected by the loop so the
    /// reducer and the components never read the clock themselves.
    now_unix_secs: u64,
    /// Unix time the interface started.
    started_at: u64,
    /// Set when the user asks to quit.
    pub(crate) quit: bool,
}

impl App {
    /// Builds the app from a resolved startup.
    ///
    /// The clock and the state store are deliberately left to the event loop:
    /// the reducer must not be able to perform IO.
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
            theme_request,
            state,
            warnings,
            repo,
            pr,
            path,
            remote,
            ..
        } = startup;

        let focus = state.focus.as_deref().map_or(Pane::default(), Pane::parse);

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
            theme_request,
            state,
            warnings,
            repo,
            requested_pr: pr,
            requested_path: path,
            remote,
            mode: Mode::Normal,
            overlay: Overlay::None,
            pending: Vec::new(),
            deadline: None,
            leader_open: false,
            command: CommandLine::default(),
            notices: Vec::new(),
            cursor: 0,
            picker_items: Vec::new(),
            picker_cursor: 0,
            theme_before_picker: None,
            help_filter: None,
            focus,
            checks: Vec::new(),
            doctor_running: false,
            doctor_job: 0,
            now_unix_secs: 0,
            started_at: 0,
            quit: false,
        };
        app.report_startup_warnings();
        Ok(app)
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

    /// The theme picker cursor.
    #[must_use]
    pub(crate) const fn picker_cursor(&self) -> usize {
        self.picker_cursor
    }

    /// The most recent live notification, which is what the status line shows.
    #[must_use]
    pub fn latest_notice(&self) -> Option<&Notice> {
        let now = Instant::now();
        self.notices.iter().rev().find(|notice| notice.is_live(now))
    }

    /// The doctor report delivered by the last job.
    #[must_use]
    pub(crate) fn checks(&self) -> &[Check] {
        &self.checks
    }

    /// Everything the doctor needs, owned so it can be handed to a background
    /// thread (FR-9.3).
    pub(crate) fn doctor_request(&self) -> Context {
        Context {
            home: self.home.clone(),
            config: self.config.clone(),
            config_path: self.config_path.clone(),
            config_exists: self.config_exists,
            warnings: self.warnings.clone(),
            keymap: self.keymap.clone(),
            theme: self.theme.clone(),
            theme_source: self.theme_source.clone(),
        }
    }

    /// Records the current time, called once per loop iteration by the loop.
    pub(crate) fn set_now(&mut self, now_unix_secs: u64) {
        if self.started_at == 0 {
            self.started_at = now_unix_secs;
        }
        self.now_unix_secs = now_unix_secs;
    }

    /// Seconds since the interface started.
    pub(crate) fn uptime_secs(&self) -> u64 {
        self.now_unix_secs.saturating_sub(self.started_at)
    }

    /// How many keys the configuration document holds, including the ones this
    /// build does not understand (FR-8.6).
    pub(crate) fn document_key_count(&self) -> usize {
        self.document.value().values().map(count_keys).sum()
    }

    /// Adds a notification to the status line (FR-7.6).
    ///
    /// Errors do not expire: they stay until the user dismisses them.
    pub fn notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        let text = text.into();

        // Deduplicate: an identical message refreshes the existing one instead of
        // stacking up.
        if let Some(existing) = self
            .notices
            .iter_mut()
            .find(|notice| notice.level == level && notice.text == text)
        {
            existing.expires = expiry_for(level);
            return;
        }

        logging::log(
            match level {
                NoticeLevel::Info => Level::Info,
                NoticeLevel::Warn => Level::Warn,
                NoticeLevel::Error => Level::Error,
            },
            &text,
        );

        if self.notices.len() >= MAX_NOTICES {
            // Evict an expiring notice before a persistent error: errors stay
            // until the user dismisses them (FR-7.6).
            let victim = self
                .notices
                .iter()
                .position(|notice| notice.expires.is_some())
                .unwrap_or(0);
            self.notices.remove(victim);
        }
        self.notices.push(Notice {
            level,
            text,
            expires: expiry_for(level),
        });
    }

    /// Removes every notification, including the persistent errors (FR-7.6).
    pub(crate) fn dismiss_notices(&mut self) {
        self.notices.clear();
    }

    /// Records a command line error.
    ///
    /// The command line is closed by the time a command runs, so the error also
    /// goes to the status line, which is where the user is looking (FR-7.4,
    /// FR-9.1).
    pub(crate) fn command_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.notice(NoticeLevel::Error, message.clone());
        self.command.error = Some(message);
    }

    /// Requests a clean shutdown.
    pub(crate) fn quit(&mut self) {
        self.quit = true;
    }

    /// Switches theme and records the choice (FR-7.7, FR-8.5).
    ///
    /// Persisting is left to the loop, hence the returned effect.
    pub(crate) fn set_theme(&mut self, name: &str) -> Effect {
        let mut warnings = Vec::new();
        match theme::load(&self.home, name, &mut warnings) {
            Ok((theme, source)) => {
                self.theme = theme;
                self.theme_source = source;
                name.clone_into(&mut self.theme_request);
                for warning in warnings {
                    self.notice(NoticeLevel::Warn, warning);
                }
                self.state.theme = Some(name.to_owned());
                self.notice(NoticeLevel::Info, format!("theme: {name}"));
                Effect::SaveState
            }
            Err(error) => {
                self.notice(NoticeLevel::Error, error.to_string());
                Effect::None
            }
        }
    }

    /// Re-reads the active theme from disk (FR-8.4).
    pub(crate) fn reload_theme(&mut self) -> Effect {
        let name = self.theme_request.clone();
        let mut warnings = Vec::new();
        match theme::load(&self.home, &name, &mut warnings) {
            Ok((theme, source)) => {
                self.theme = theme;
                self.theme_source.clone_from(&source);
                for warning in warnings {
                    self.notice(NoticeLevel::Warn, warning);
                }
                self.notice(
                    NoticeLevel::Info,
                    format!("reloaded theme {name} from {source}"),
                );
            }
            Err(error) => self.notice(NoticeLevel::Error, error.to_string()),
        }
        Effect::None
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
    ///
    /// Anything the popup needs from the filesystem is captured here, so
    /// rendering stays pure.
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

        match self.overlay {
            Overlay::ThemePicker => {
                self.theme_before_picker = Some((self.theme.clone(), self.theme_source.clone()));
                // Resolve every candidate once, here, so moving the cursor never
                // reads the disk and a broken theme file is reported when the
                // picker opens rather than when it is committed (FR-7.7).
                let mut warnings = Vec::new();
                let entries: Vec<PickerEntry> = theme::available(&self.home)
                    .into_iter()
                    .map(|name| match theme::load(&self.home, &name, &mut warnings) {
                        Ok((theme, source)) => PickerEntry {
                            name,
                            theme: Some(theme),
                            source: Some(source),
                        },
                        Err(error) => {
                            warnings.push(error.to_string());
                            PickerEntry {
                                name,
                                theme: None,
                                source: None,
                            }
                        }
                    })
                    .collect();
                self.picker_items = entries;
                self.picker_cursor = self
                    .picker_items
                    .iter()
                    .position(|entry| entry.name == self.theme_request)
                    .unwrap_or(0);
                for warning in warnings {
                    self.notice(NoticeLevel::Warn, warning);
                }
            }
            Overlay::Help => self.help_filter = None,
            _ => {}
        }
    }

    /// Closes a popup, keeping whatever it selected.
    pub(crate) fn close_overlay(&mut self) {
        self.overlay = Overlay::None;
        self.leader_open = false;
        self.mode = Mode::Normal;
        self.theme_before_picker = None;
        self.help_filter = None;
    }

    /// Closes a popup and undoes any preview it applied (FR-7.7).
    pub(crate) fn cancel_overlay(&mut self) {
        if let Some((theme, source)) = self.theme_before_picker.take() {
            self.theme = theme;
            self.theme_source = source;
        }
        self.close_overlay();
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
        self.tick_at(Instant::now());
    }

    /// Expiry with the clock passed in, so it is testable.
    pub(crate) fn tick_at(&mut self, now: Instant) {
        self.notices.retain(|notice| notice.is_live(now));
    }

    /// Handles one key press (FR-7.1, FR-7.2).
    pub fn on_key(&mut self, event: KeyEvent) -> Effect {
        let combo = keymap::normalize(KeyCombo::from(event));
        match self.mode {
            Mode::Command => self.on_command_key(combo),
            Mode::Popup => self.on_popup_key(combo),
            Mode::Normal | Mode::Insert | Mode::Search | Mode::Visual => self.on_normal_key(combo),
        }
    }

    /// Scrolls the roadmap or the picker with the mouse wheel (FR-7.5).
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
    pub fn on_timeout(&mut self) -> Effect {
        self.deadline = None;
        if self.pending.is_empty() || self.leader_open {
            return Effect::None;
        }

        let Some(action) = self.pending_action() else {
            // `g` on its own is a prefix of `gg` but is not bound to anything;
            // dropping it silently is the vim behaviour.
            self.pending.clear();
            return Effect::None;
        };

        let effect = update::dispatch(self, &action);
        if effect != Effect::KeepPending {
            self.pending.clear();
        }
        effect
    }

    /// Marks the doctor job as started and opens the popup (FR-9.3).
    ///
    /// Returns the job id so the event loop can tag the report: a report from a
    /// superseded job must not be applied.
    pub(crate) fn start_doctor(&mut self) -> u64 {
        self.doctor_job = self.doctor_job.wrapping_add(1);
        self.checks = Vec::new();
        self.doctor_running = true;
        self.open_overlay(Overlay::Doctor);
        self.notice(NoticeLevel::Info, "collecting the environment report…");
        self.doctor_job
    }

    /// The id of the most recent doctor request.
    pub(crate) const fn doctor_job(&self) -> u64 {
        self.doctor_job
    }

    /// Delivers a doctor report collected off the event loop (FR-9.3).
    pub(crate) fn apply_checks(&mut self, job: u64, checks: Vec<Check>) {
        if job != self.doctor_job {
            return; // a superseded report
        }
        self.checks = checks;
        self.doctor_running = false;
    }

    /// The doctor job died without reporting, so stop waiting for it.
    pub(crate) fn abort_doctor_job(&mut self) {
        if self.doctor_running {
            self.doctor_running = false;
            self.notice(
                NoticeLevel::Error,
                "the environment report could not be collected",
            );
        }
        self.doctor_job = self.doctor_job.wrapping_add(1);
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

    /// Whether a key press matches a global binding that should apply even in a
    /// text-entry mode (FR-8.3).
    ///
    /// Only combinations with a real modifier qualify. Inside the command line a
    /// bare `?`, `:` or the leader key is text the user is typing, not a
    /// command — treating them as bindings makes `:set ui.timeoutlen=250`
    /// impossible, because the space would open the leader menu.
    /// Ends a key sequence, closing the leader menu with it (FR-7.3).
    ///
    /// `Effect::KeepPending` skips this so the next key can complete the
    /// sequence; every other outcome means the sequence is over, and the menu
    /// must not outlive it.
    fn finish_sequence(&mut self) {
        self.pending.clear();
        self.deadline = None;
        self.leader_open = false;
        if self.overlay == Overlay::Leader {
            self.overlay = Overlay::None;
        }
    }

    fn global_action(&self, combo: KeyCombo) -> Option<String> {
        if !combo
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return None;
        }
        match self.keymap.resolve_global(&[combo]) {
            Resolution::Match(binding) => Some(binding.action.clone()),
            _ => None,
        }
    }

    fn on_normal_key(&mut self, combo: KeyCombo) -> Effect {
        // Esc is a universal cancel: it drops a half-typed sequence and closes a
        // popup before it means anything else (FR-7.1).
        if combo.code == KeyCode::Esc
            && (!self.pending.is_empty() || self.leader_open || self.overlay != Overlay::None)
        {
            self.pending.clear();
            self.deadline = None;
            self.cancel_overlay();
            return Effect::None;
        }

        self.pending.push(combo);
        self.advance()
    }

    fn advance(&mut self) -> Effect {
        let timeout = self.keymap.timeout();
        let mode = self.mode;

        let next = match self.keymap.resolve(mode, &self.pending) {
            Resolution::Match(binding) => Some(binding.action.clone()),
            Resolution::Ambiguous(_) | Resolution::Prefix => {
                self.deadline = Some(Instant::now() + timeout);
                return Effect::None;
            }
            Resolution::None => None,
        };

        if let Some(action) = next {
            let effect = update::dispatch(self, &action);
            if effect != Effect::KeepPending {
                self.finish_sequence();
            }
            return effect;
        }

        let keys = keymap::describe_sequence(&self.pending);
        let was_leader = self.leader_open;
        self.finish_sequence();
        let level = if was_leader {
            NoticeLevel::Info
        } else {
            NoticeLevel::Warn
        };
        self.notice(level, format!("`{keys}` is not bound"));
        Effect::None
    }

    /// The action the pending sequence would fire on timeout.
    fn pending_action(&self) -> Option<String> {
        match self.keymap.resolve(self.mode, &self.pending) {
            Resolution::Match(binding) | Resolution::Ambiguous(binding) => {
                Some(binding.action.clone())
            }
            Resolution::Prefix | Resolution::None => None,
        }
    }

    fn on_popup_key(&mut self, combo: KeyCombo) -> Effect {
        // The popup table governs popups, global bindings included, so a user can
        // bind keys for a popup. The arms below are the built-in popup behaviour
        // that deliberately needs no binding (FR-8.3).
        if let Resolution::Match(binding) = self.keymap.resolve(Mode::Popup, &[combo]) {
            let action = binding.action.clone();
            return update::dispatch(self, &action);
        }

        match combo.code {
            KeyCode::Esc => {
                self.cancel_overlay();
                Effect::None
            }
            KeyCode::Char('q') if combo.modifiers.is_empty() => {
                self.cancel_overlay();
                Effect::None
            }
            KeyCode::Char('j') | KeyCode::Down if self.overlay == Overlay::ThemePicker => {
                self.move_picker(1);
                Effect::None
            }
            KeyCode::Char('k') | KeyCode::Up if self.overlay == Overlay::ThemePicker => {
                self.move_picker(-1);
                Effect::None
            }
            KeyCode::Enter if self.overlay == Overlay::ThemePicker => {
                let name = self
                    .picker_items
                    .get(self.picker_cursor)
                    .map(|entry| entry.name.clone());
                self.close_overlay();
                match name {
                    Some(name) => self.set_theme(&name),
                    None => Effect::None,
                }
            }
            _ => Effect::None,
        }
    }

    fn move_picker(&mut self, delta: i32) {
        let count = self.picker_items.len();
        if count == 0 {
            return;
        }
        self.picker_cursor = clamp_cursor(self.picker_cursor, delta, count);
        // Preview from the list resolved when the picker opened; `cancel_overlay`
        // puts the original back.
        if let Some(entry) = self.picker_items.get(self.picker_cursor)
            && let Some(theme) = &entry.theme
        {
            self.theme = theme.clone();
            if let Some(source) = &entry.source {
                self.theme_source.clone_from(source);
            }
        }
    }

    /// Moves the roadmap cursor.
    pub(crate) fn move_cursor(&mut self, delta: i32) {
        self.cursor = clamp_cursor(self.cursor, delta, ROADMAP.len());
    }

    /// Moves the roadmap cursor to one end.
    pub(crate) fn move_cursor_to(&mut self, last: bool) {
        self.cursor = if last {
            ROADMAP.len().saturating_sub(1)
        } else {
            0
        };
    }

    fn on_command_key(&mut self, combo: KeyCombo) -> Effect {
        if let Some(action) = self.global_action(combo) {
            return update::dispatch(self, &action);
        }

        match combo.code {
            KeyCode::Esc => {
                self.command.clear();
                self.mode = Mode::Normal;
                Effect::None
            }
            KeyCode::Enter => {
                let input = std::mem::take(&mut self.command.input);
                self.command.error = None;
                self.mode = Mode::Normal;
                update::command(self, &input)
            }
            KeyCode::Backspace => {
                self.command.backspace();
                Effect::None
            }
            KeyCode::Tab => {
                self.command.input = update::complete_command(&self.command.input);
                Effect::None
            }
            KeyCode::Char(value)
                if combo.modifiers.is_empty() || combo.modifiers == KeyModifiers::SHIFT =>
            {
                self.command.push(value);
                Effect::None
            }
            _ => Effect::None,
        }
    }

    /// Renders a frame (FR-7.8).
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        if layout::is_too_small(area) {
            components::panes::render_too_small(frame, area, &self.theme);
            return;
        }

        let palette_rows = if self.mode == Mode::Command {
            u16::try_from(PALETTE_ROWS).unwrap_or(0)
        } else {
            0
        };

        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(palette_rows),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        components::header::render(frame, rows[0], self);
        components::panes::render(frame, rows[1], self);
        components::palette::render(frame, rows[2], self);
        components::status_line::render(frame, rows[3], self);
        components::command_line::render(frame, rows[4], self);
        components::render_overlay(frame, area, self);
    }
}

/// How long a notice of this level should live (FR-7.6).
fn expiry_for(level: NoticeLevel) -> Option<Instant> {
    match level {
        NoticeLevel::Error => None,
        NoticeLevel::Info | NoticeLevel::Warn => Some(Instant::now() + NOTICE_LIFETIME),
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

/// Counts the keys in a value, descending into tables.
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
        let mut app = App::new(startup).unwrap();
        app.set_now(1_000);
        (dir, app)
    }

    fn press(app: &mut App, keys: &str) -> Effect {
        let leader = app.keymap.leader();
        let mut effect = Effect::None;
        for combo in keymap::parse_keys(keys, &leader).unwrap() {
            effect = app.on_key(KeyEvent::new(combo.code, combo.modifiers));
        }
        effect
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
        assert_eq!(app.pending.len(), 1);
        assert!(!app.leader_open);

        let effect = app.on_timeout();
        assert_eq!(effect, Effect::KeepPending, "the menu stays open for a key");
        assert!(app.leader_open);
        assert_eq!(app.overlay(), Overlay::Leader);

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
    fn an_unknown_leader_continuation_closes_the_menu() {
        let (_dir, mut app) = app();
        press(&mut app, "<Space>");
        app.on_timeout();
        press(&mut app, "z");
        assert!(!app.leader_open);
        assert_eq!(
            app.overlay(),
            Overlay::None,
            "the menu must not outlive its sequence"
        );
        assert!(app.latest_notice().is_some());
    }

    #[test]
    fn ctrl_c_works_while_a_popup_is_open() {
        let (_dir, mut app) = app();
        press(&mut app, "?");
        assert_eq!(app.overlay(), Overlay::Help);
        press(&mut app, "<C-c>");
        assert!(app.should_quit(), "global bindings apply in every mode");
    }

    #[test]
    fn ctrl_c_works_on_the_command_line() {
        let (_dir, mut app) = app();
        press(&mut app, ":");
        press(&mut app, "hel");
        press(&mut app, "<C-c>");
        assert!(app.should_quit());
    }

    #[test]
    fn bare_keys_are_text_on_the_command_line() {
        // `?`, `:` and the leader are bindings in normal mode but ordinary
        // characters while a command is being typed (FR-7.4).
        let (_dir, mut app) = app();
        press(&mut app, ":");
        for character in "set ui.timeoutlen=120".chars() {
            press(&mut app, &character.to_string());
        }
        assert_eq!(app.mode(), Mode::Command);
        assert_eq!(app.command.input, "set ui.timeoutlen=120");
        assert_eq!(app.overlay(), Overlay::None, "the leader must not open");
    }

    #[test]
    fn theme_picker_applies_the_selected_theme_and_remembers_it() {
        let (_dir, mut app) = app();
        press(&mut app, "<Space>t");
        assert_eq!(app.overlay(), Overlay::ThemePicker);

        let effect = press(&mut app, "j");
        assert_eq!(effect, Effect::None, "moving only previews");
        assert_eq!(app.theme.name(), "light", "moving previews the theme");

        let effect = press(&mut app, "<CR>");
        assert_eq!(effect, Effect::SaveState, "committing persists the choice");
        assert_eq!(app.theme.name(), "light");
        assert_eq!(app.state.theme.as_deref(), Some("light"));
        assert_eq!(app.overlay(), Overlay::None);
    }

    #[test]
    fn cancelling_the_theme_picker_restores_the_previous_theme() {
        let (_dir, mut app) = app();
        assert_eq!(app.theme.name(), "dark");
        press(&mut app, "<Space>t");
        press(&mut app, "j");
        assert_eq!(app.theme.name(), "light");
        press(&mut app, "<Esc>");
        assert_eq!(app.theme.name(), "dark", "cancelling undoes the preview");
        assert!(app.state.theme.is_none(), "nothing was persisted");
    }

    #[test]
    fn doctor_opens_immediately_and_fills_in_from_the_job() {
        let (_dir, mut app) = app();
        press(&mut app, ":");
        for character in "doctor".chars() {
            press(&mut app, &character.to_string());
        }
        let effect = press(&mut app, "<CR>");
        assert_eq!(effect, Effect::RunDoctor);
        assert_eq!(app.overlay(), Overlay::Doctor);
        assert!(app.doctor_running);
        assert!(app.checks().is_empty());

        let job = app.doctor_job();
        app.apply_checks(
            job,
            vec![Check {
                name: "home",
                status: crate::doctor::Status::Ok,
                detail: "ok".to_owned(),
            }],
        );
        assert!(!app.doctor_running);
        assert_eq!(app.checks().len(), 1);
    }

    #[test]
    fn a_stale_doctor_report_is_discarded() {
        let (_dir, mut app) = app();
        press(&mut app, ":");
        for character in "doctor".chars() {
            press(&mut app, &character.to_string());
        }
        press(&mut app, "<CR>");
        let first = app.doctor_job();

        // A second request supersedes the first.
        press(&mut app, ":");
        for character in "doctor".chars() {
            press(&mut app, &character.to_string());
        }
        press(&mut app, "<CR>");

        app.apply_checks(first, Vec::new());
        assert!(
            app.doctor_running,
            "the stale report must not resolve the job"
        );

        app.apply_checks(
            app.doctor_job(),
            vec![Check {
                name: "home",
                status: crate::doctor::Status::Ok,
                detail: "ok".to_owned(),
            }],
        );
        assert!(!app.doctor_running);
    }

    #[test]
    fn a_dead_doctor_job_stops_waiting() {
        let (_dir, mut app) = app();
        press(&mut app, ":");
        for character in "doctor".chars() {
            press(&mut app, &character.to_string());
        }
        press(&mut app, "<CR>");
        assert!(app.doctor_running);
        app.abort_doctor_job();
        assert!(!app.doctor_running);
    }

    #[test]
    fn a_leader_binding_closes_the_menu_when_it_fires() {
        // The menu opens on the ambiguity timeout, so a test that fires
        // <leader>t without waiting would never have opened it and would miss
        // this (FR-7.3).
        let (_dir, mut app) = app();
        press(&mut app, "<Space>");
        app.on_timeout();
        assert_eq!(app.overlay(), Overlay::Leader);

        press(&mut app, "l");
        assert_eq!(app.theme.name(), "light");
        assert!(!app.leader_open);
        assert_eq!(
            app.overlay(),
            Overlay::None,
            "the leader menu must not outlive the binding it fired"
        );

        // And the cursor is reachable again.
        press(&mut app, "j");
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn a_persistent_error_survives_the_notice_queue() {
        let (_dir, mut app) = app();
        app.notice(NoticeLevel::Error, "boom");
        for text in ["one", "two", "three", "four"] {
            app.notice(NoticeLevel::Info, text);
        }
        assert!(
            app.notices
                .iter()
                .any(|notice| notice.level == NoticeLevel::Error),
            "an error must not be evicted by informational notices"
        );
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
    fn a_bad_set_command_reaches_the_status_line() {
        let (_dir, mut app) = app();
        press(&mut app, ":");
        for character in "set nonsense=1".chars() {
            press(&mut app, &character.to_string());
        }
        press(&mut app, "<CR>");

        let error = app
            .command
            .error
            .clone()
            .expect("an error should be recorded");
        assert!(error.contains("nonsense"), "{error}");

        // The command line is closed by now, so the notice is what the user sees.
        let notice = app
            .latest_notice()
            .expect("an error notice should be shown");
        assert_eq!(notice.level, NoticeLevel::Error);
        assert!(notice.text.contains("nonsense"), "{}", notice.text);
    }

    #[test]
    fn an_unknown_command_reaches_the_status_line() {
        let (_dir, mut app) = app();
        press(&mut app, ":");
        for character in "bogus".chars() {
            press(&mut app, &character.to_string());
        }
        press(&mut app, "<CR>");
        let notice = app.latest_notice().expect("a notice should be shown");
        assert_eq!(notice.level, NoticeLevel::Error);
        assert!(notice.text.contains("bogus"), "{}", notice.text);
        assert!(app.latest_notice().is_some());
        // Errors stay until they are dismissed (FR-7.6).
        app.tick_at(Instant::now() + Duration::from_secs(600));
        assert!(app.latest_notice().is_some(), "errors do not expire");
    }

    #[test]
    fn informational_notices_expire() {
        let (_dir, mut app) = app();
        app.notice(NoticeLevel::Info, "hello");
        assert!(app.latest_notice().is_some());
        app.tick_at(Instant::now() + NOTICE_LIFETIME + Duration::from_secs(1));
        assert!(app.latest_notice().is_none());
    }

    #[test]
    fn identical_notices_are_deduplicated() {
        let (_dir, mut app) = app();
        app.notice(NoticeLevel::Warn, "same");
        app.notice(NoticeLevel::Warn, "same");
        app.notice(NoticeLevel::Warn, "same");
        assert_eq!(app.notices.len(), 1);

        app.notice(NoticeLevel::Error, "same");
        assert_eq!(app.notices.len(), 2, "a different level is a new notice");
    }

    #[test]
    fn notices_can_be_dismissed() {
        let (_dir, mut app) = app();
        app.notice(NoticeLevel::Error, "boom");
        press(&mut app, ":");
        for character in "messages".chars() {
            press(&mut app, &character.to_string());
        }
        press(&mut app, "<CR>");
        assert!(app.latest_notice().is_none());
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
    fn the_mouse_wheel_moves_the_cursor() {
        let (_dir, mut app) = app();
        app.on_scroll(1);
        assert_eq!(app.cursor, 1);
        app.on_scroll(-1);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn pane_focus_cycles_and_is_remembered() {
        let (_dir, mut app) = app();
        assert_eq!(app.focus(), Pane::PullRequests);
        press(&mut app, "<Tab>");
        assert_eq!(app.focus(), Pane::Diff);
        press(&mut app, "<S-Tab>");
        assert_eq!(app.focus(), Pane::PullRequests);
    }

    #[test]
    fn a_theme_change_asks_the_loop_to_persist_state() {
        let (_dir, mut app) = app();
        let effect = press(&mut app, "<Space>l");
        assert_eq!(effect, Effect::SaveState);
        assert_eq!(app.state.theme.as_deref(), Some("light"));
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
}
