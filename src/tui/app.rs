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
use crate::application::models::CatalogState;
use crate::application::prs::FetchOutcome;
use crate::config::{Config, ConfigDocument, ModelSelection};
use crate::doctor::{Check, Context};
use crate::domain::diff::DiffSource;
use crate::domain::environment::{Environment, EnvironmentError};
use crate::domain::pr::PullRequestDetail;
use crate::error::Result;
use crate::logging::{self, Level};
use crate::paths::Home;
use crate::ports::catalog::CatalogPolicy;
use crate::ports::secret::SecretStore;
use crate::ports::workspace::{DiffOptions, Workspace};
use crate::state::AppState;
use crate::tui::action;
use crate::tui::components;
use crate::tui::components::model_picker::{
    self, PickerState, model_rows, provider_rows, thinking_rows,
};
use crate::tui::diff_view::DiffView;
use crate::tui::event::{self, KeyCode, KeyEvent, KeyModifiers};
use crate::tui::jobs::{self, Completion, Outcome};
use crate::tui::keymap::{self, KeyCombo, Keymap, Mode, Resolution};
use crate::tui::layout;
use crate::tui::list_view::PrListState;
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

/// What resolving the pending key sequence produced.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// A binding matched; the effect is what it asked for.
    Fired(Effect),
    /// A longer sequence could still match, so wait for the next key.
    Waiting,
    /// Nothing matches.
    Unmatched,
}

/// What the reducer wants the event loop to do next.
///
/// Actions are mutually exclusive in M0: the only action that keeps a pending
/// sequence is the leader menu, and the only ones that need the loop are a theme
/// change (persist it) and `:doctor` (probe off the event loop).
#[derive(Clone, PartialEq, Default)]
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
    /// Work out where we are running and whether the forge is usable (FR-1.1).
    DetectEnvironment,
    /// Fetch the first page of the current query (FR-2.1).
    LoadPullRequests,
    /// Fetch the next page, up to the configured cap (FR-2.1).
    LoadMore,
    /// Fetch the exact number of matches, which a full page makes necessary
    /// (FR-2.1).
    CountPullRequests,
    /// Open a pull request: its detail, then its diff (FR-2.4, FR-3.2).
    OpenPullRequest(u64),
    /// Re-fetch the diff of the open pull request, for a context change (FR-3.2).
    ReloadDiff,
    /// Put a path on the clipboard through the terminal (FR-3.4).
    CopyPath(String),
    /// Turn mouse capture on or off, which only the loop can do (FR-7.5).
    SetMouse(bool),
    /// Give up on the work in flight for the current screen (NFR-1.4).
    CancelInFlight,
    /// Load the model catalog (FR-4.7).
    LoadCatalog(CatalogPolicy),
    /// Materialise the open pull request in a managed worktree (FR-3.1).
    EnsureWorkspace(u64),
    /// Ask the provider whether the chosen model works (FR-4.5).
    CheckModel,
    /// Store a key the user typed (FR-4.5).
    SaveKey {
        /// Which provider it belongs to.
        provider: String,
        /// The key itself. Kept out of `Debug` by the custom impl below.
        key: String,
    },
    /// Write the chosen model into `config.toml` (FR-8.6).
    SaveSelection(Box<ModelSelection>),
    /// Remove a stored key (NFR-3.1).
    ClearKey(String),
    /// Remove the worktrees that are no longer needed (FR-3.1).
    CleanWorkspaces(bool),
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A key must never be printed, not even by `{:?}` in a log line (NFR-3.1).
        match self {
            Self::SaveKey { provider, .. } => f
                .debug_struct("SaveKey")
                .field("provider", provider)
                .field("key", &"<redacted>")
                .finish(),
            other => f.write_str(&effect_name(other)),
        }
    }
}

/// What the interface starts with: the themes it can cycle through and the list.
///
/// Both are read once, before the terminal is taken over, so that cycling themes and
/// paging the list later are pure computation (NFR-1.2).
fn initial_view(
    home: &crate::paths::Home,
    config: &Config,
    state: &AppState,
) -> (Vec<String>, PrListState, Pane) {
    let focus = state.focus.as_deref().map_or(Pane::default(), Pane::parse);
    let theme_names = theme::available(home);
    let list = PrListState::new(
        u32::try_from(config.review.page_size).unwrap_or(50),
        u32::try_from(
            config
                .review
                .page_size
                .saturating_mul(config.review.max_pages.max(1)),
        )
        .unwrap_or(500),
    );
    (theme_names, list, focus)
}

/// The calendar year for a Unix timestamp, for "is this model old?" (FR-4.7).
fn now_year(unix_secs: u64) -> u32 {
    chrono::DateTime::from_timestamp(i64::try_from(unix_secs).unwrap_or(i64::MAX), 0)
        .and_then(|time| time.format("%Y").to_string().parse::<u32>().ok())
        // Before 1970 there is no calendar year to speak of; models are newer.
        .unwrap_or(1970)
}

/// A short name for an effect, for logs and tests.
fn effect_name(effect: &Effect) -> String {
    match effect {
        Effect::None => "none".to_owned(),
        Effect::KeepPending => "keep-pending".to_owned(),
        Effect::SaveState => "save-state".to_owned(),
        Effect::RunDoctor => "run-doctor".to_owned(),
        Effect::DetectEnvironment => "detect-environment".to_owned(),
        Effect::LoadPullRequests => "load-pull-requests".to_owned(),
        Effect::LoadMore => "load-more".to_owned(),
        Effect::CountPullRequests => "count-pull-requests".to_owned(),
        Effect::OpenPullRequest(number) => format!("open-pull-request({number})"),
        Effect::ReloadDiff => "reload-diff".to_owned(),
        Effect::CopyPath(_) => "copy-path".to_owned(),
        Effect::SetMouse(enabled) => format!("set-mouse({enabled})"),
        Effect::CancelInFlight => "cancel-in-flight".to_owned(),
        Effect::LoadCatalog(policy) => format!("load-catalog({policy:?})"),
        Effect::EnsureWorkspace(number) => format!("ensure-workspace({number})"),
        Effect::CheckModel => "check-model".to_owned(),
        Effect::SaveKey { .. } => "save-key".to_owned(),
        Effect::SaveSelection(_) => "save-selection".to_owned(),
        Effect::ClearKey(provider) => format!("clear-key({provider})"),
        Effect::CleanWorkspaces(all) => format!("clean-workspaces({all})"),
    }
}

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

/// A pull request being opened, and how far along that is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opening {
    /// Which pull request.
    pub number: u64,
    /// Which step the fetch is on.
    pub stage: OpeningStage,
    /// When the fetch started, for the elapsed time.
    started_at: u64,
}

/// The two steps of opening a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpeningStage {
    /// `gh pr view`: the metadata, commits, checks and reviews.
    Detail,
    /// `gh pr diff`: the patch itself, which is the slow one.
    Diff,
}

impl Opening {
    /// The first line of the indicator.
    #[must_use]
    pub fn headline(&self) -> String {
        format!("Opening #{}", self.number)
    }

    /// The second line: what is being fetched, and for how long.
    #[must_use]
    pub fn detail(&self, now: u64) -> String {
        let what = match self.stage {
            OpeningStage::Detail => "fetching the pull request",
            OpeningStage::Diff => "fetching the diff",
        };
        let elapsed = now.saturating_sub(self.started_at);
        if elapsed < 2 {
            what.to_owned()
        } else {
            format!("{what} · {elapsed}s so far")
        }
    }
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
    /// Every theme the user can choose, read once at startup (and refreshed when
    /// the picker opens) so toggling never touches the disk (NFR-1.2).
    pub(crate) theme_names: Vec<String>,
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
    /// Themes offered by the picker, resolved when it opens so rendering never
    /// reads the filesystem (FR-7.7).
    pub(crate) picker_items: Vec<PickerEntry>,
    /// Cursor inside the theme picker.
    pub(crate) picker_cursor: usize,
    /// Theme in force when the picker opened, restored if it is cancelled.
    theme_before_picker: Option<(Theme, String)>,
    /// Restricts the help popup to one action, set by `:keymap <action>`.
    pub(crate) help_filter: Option<String>,
    /// The first line the help popup shows: the list is longer than most terminals.
    pub(crate) help_scroll: usize,
    /// What detection resolved, once it has run (FR-1.1).
    pub(crate) environment: Option<Environment>,
    /// Why detection failed, when it did.
    pub(crate) environment_error: Option<EnvironmentError>,
    /// Whether detection is still running.
    pub(crate) environment_running: bool,
    /// The pull request list (FR-2.1).
    pub(crate) list: PrListState,
    /// The detail of the open pull request (FR-2.4).
    pub(crate) detail: Option<PullRequestDetail>,
    /// The review view, present when a pull request is open (FR-3.3).
    pub(crate) review: Option<DiffView>,
    /// The pull request being opened, if one is (FR-7.6).
    opening: Option<Opening>,
    /// Advances the opening spinner; driven by the loop's tick.
    spinner: usize,
    /// Whether the diff is still being fetched.
    pub(crate) diff_loading: bool,
    /// The model catalog, once it has been fetched (FR-4.7).
    pub(crate) catalog: Option<CatalogState>,
    /// The model picker, when it is open (FR-4.5).
    pub(crate) picker: Option<PickerState>,
    /// The selection in use, resolved against the catalog (FR-4.5).
    pub(crate) active_model: Option<crate::application::models::ResolvedSelection>,
    /// Why the configured model cannot be used, when it cannot.
    pub(crate) model_problem: Option<String>,
    /// The job id of the picker's connection check (FR-4.5).
    pub(crate) check_job: u64,
    /// The job id of the catalog fetch (FR-4.7).
    pub(crate) catalog_job: u64,
    /// The diff flags the review screen is using (FR-3.2).
    pub(crate) diff_options: DiffOptions,
    /// Where the diff on screen was read from (FR-3.2).
    pub(crate) diff_source: DiffSource,
    /// The credential store, for reading and writing provider keys (FR-4.5).
    pub(crate) secret_store: std::sync::Arc<dyn SecretStore>,
    /// The workspace port, for listing managed worktrees (FR-3.1).
    pub(crate) workspace_port: std::sync::Arc<dyn crate::ports::WorkspacePort>,
    /// The open pull request's worktree, once it exists (FR-3.1).
    pub(crate) workspace: Option<Workspace>,
    /// The job id of the worktree being built (FR-3.1).
    pub(crate) workspace_job: u64,
    /// Why the shown diff came from the cache, when it did (DEC-14).
    pub(crate) diff_offline: Option<String>,
    /// The job id of the newest detection request.
    pub(crate) environment_job: u64,
    /// The job id of the newest list request, so a superseded answer is dropped.
    pub(crate) list_job: u64,
    /// The job id of the newest count request.
    pub(crate) count_job: u64,
    /// The job id of the newest detail request.
    pub(crate) detail_job: u64,
    /// The job id of the newest diff request.
    pub(crate) patch_job: u64,
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
    /// The width of the last frame.
    last_width: u16,
    /// The filter bar's rectangle, learned from the last frame (FR-7.5).
    filter_bar_rect: ratatui::layout::Rect,
    /// The list pane's rectangle, learned from the last frame.
    list_pane: ratatui::layout::Rect,
    /// The whole body rectangle, for the bounds a click has to fall inside.
    body_rect: ratatui::layout::Rect,
    /// The first row of the review panes.
    review_top: u16,
    /// The width of the file tree, which separates the two review panes.
    tree_width: u16,
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
            secret_store,
            workspace_port,
            ..
        } = startup;

        let (theme_names, list, focus) = initial_view(&home, &config, &state);

        let mut app = Self {
            secret_store,
            workspace_port,
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
            theme_names,
            environment: None,
            environment_error: None,
            environment_running: true,
            list,
            detail: None,
            review: None,
            opening: None,
            spinner: 0,
            diff_loading: false,
            catalog: None,
            picker: None,
            active_model: None,
            model_problem: None,
            check_job: 0,
            catalog_job: 0,
            diff_options: DiffOptions::default(),
            diff_source: DiffSource::Forge,
            workspace: None,
            workspace_job: 0,
            diff_offline: None,
            environment_job: 0,
            list_job: 0,
            count_job: 0,
            detail_job: 0,
            patch_job: 0,
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
            picker_items: Vec::new(),
            picker_cursor: 0,
            theme_before_picker: None,
            help_filter: None,
            help_scroll: 0,
            focus,
            checks: Vec::new(),
            doctor_running: false,
            doctor_job: 0,
            now_unix_secs: 0,
            started_at: 0,
            // Zero until a frame has been drawn: a width that was never measured
            // must not be used to decide anything.
            last_width: 0,
            filter_bar_rect: ratatui::layout::Rect::default(),
            list_pane: ratatui::layout::Rect::default(),
            body_rect: ratatui::layout::Rect::default(),
            review_top: 0,
            tree_width: 30,
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
            config_keys: self.document_key_count(),
            environment: self.environment.clone(),
            environment_error: self.environment_error.clone(),
        }
    }

    /// Records the current time, called once per loop iteration by the loop.
    /// The clock as of this loop iteration, for components that show ages.
    #[must_use]
    pub fn now(&self) -> crate::domain::time::Timestamp {
        chrono::DateTime::from_timestamp(i64::try_from(self.now_unix_secs).unwrap_or(0), 0)
            .unwrap_or_default()
    }

    /// Replaces the pull request list (FR-2.1).
    ///
    /// The loop reaches this through [`Self::apply_completion`]; it is public so a
    /// rendered frame can be tested against a known list, the same way
    /// [`Self::open_review`] makes the review screen testable.
    pub fn set_pull_requests(&mut self, page: crate::ports::forge::PullRequestPage) {
        self.list.replace(page);
    }

    /// Opens the review screen for a detail and its diff (FR-3.3).
    pub fn open_review(&mut self, detail: PullRequestDetail, view: DiffView) {
        self.detail = Some(detail);
        self.review = Some(view);
        self.focus = Pane::Diff;
        self.diff_loading = false;
        self.stop_opening();
    }

    /// Closes the review screen and returns to the list (FR-3.4).
    pub fn close_review(&mut self) -> bool {
        if self.review.is_none() {
            return false;
        }
        self.review = None;
        self.detail = None;
        self.diff_offline = None;
        self.diff_loading = false;
        self.focus = Pane::PullRequests;
        true
    }

    /// Whether a pull request is open, which decides which screen is drawn.
    #[must_use]
    pub fn review_screen(&self) -> Option<&DiffView> {
        self.review.as_ref()
    }

    /// Replaces the review view, keeping the detail it belongs to.
    pub fn set_review(&mut self, view: DiffView) {
        self.review = Some(view);
    }

    /// The open review view for editing, if any.
    pub fn review_mut(&mut self) -> Option<&mut DiffView> {
        self.review.as_mut()
    }

    /// Remembers which job a request became, so a superseded answer can be dropped.
    pub fn record_job(&mut self, effect: &Effect, id: u64) {
        match effect {
            Effect::DetectEnvironment => self.environment_job = id,
            Effect::LoadPullRequests | Effect::LoadMore => {
                self.list_job = id;
                self.list.loading = true;
                self.list.error = None;
            }
            Effect::CountPullRequests => {
                self.count_job = id;
                self.list.counting = true;
            }
            Effect::OpenPullRequest(number) => {
                self.detail_job = id;
                self.diff_loading = true;
                // The indicator appears on the key press rather than when the first
                // response arrives: the wait is what it exists for.
                self.begin_opening(*number);
            }
            Effect::RunDoctor => self.doctor_job = id,
            // The rest ask for no job, or are handled by `apply` rather than here.
            _ => {}
        }
    }

    /// Applies the result of a background job (ARCH-5).
    ///
    /// Returns what the loop should do next, if the result makes something else
    /// necessary: opening a pull request needs its diff, a truncated list needs its
    /// count, and so on. The reducer stays the only thing that decides, and it still
    /// performs no IO.
    ///
    /// Every arm is gated on the job id it answers, so a superseded result — an error
    /// as much as a success — is dropped rather than painted over a fresher answer.
    pub fn apply_completion(&mut self, completion: jobs::Completion) -> Option<Effect> {
        let Completion { job, outcome } = completion;

        match outcome {
            Outcome::Environment(environment) if job == self.environment_job => {
                self.set_environment(*environment);
                self.notice(NoticeLevel::Info, self.environment_summary());
                Some(Effect::LoadPullRequests)
            }
            Outcome::EnvironmentFailed(error) if job == self.environment_job => {
                self.set_environment_error(*error);
                None
            }
            Outcome::CachedPage(cached) => {
                // Painted before the network answers; the fetch that follows replaces
                // it in place, keeping the cursor on the same pull request (FR-2.3).
                let age = cached.age_secs;
                let stale = cached.stale;
                self.list.replace(cached.value);
                // The age is shown as a duration of *this* kind of data rather than
                // as a wall-clock time: "cached 12s ago" is what the user needs.
                self.list.stale = Some(if stale {
                    format!("cached {age}s ago")
                } else {
                    "cached".to_owned()
                });
                self.list.loading = true;
                None
            }
            Outcome::Page(outcome) if job == self.list_job => self.apply_page(*outcome),
            Outcome::Count(count) if job == self.count_job => {
                self.list.set_total(count);
                None
            }
            Outcome::Detail(outcome) if job == self.detail_job => {
                self.apply_detail(*outcome);
                Some(Effect::ReloadDiff)
            }
            Outcome::Patch { outcome, source } if job == self.patch_job => {
                self.apply_patch(*outcome, source)
            }
            Outcome::Catalog(load) if job == self.catalog_job => {
                self.apply_catalog(*load);
                None
            }
            Outcome::Workspace(workspace) if job == self.workspace_job => {
                self.workspace = Some(*workspace);
                // The diff was read from the forge a moment ago; now that the code is
                // on disk, the same diff is read locally so the context and whitespace
                // toggles mean something (FR-3.2).
                Some(Effect::ReloadDiff)
            }
            Outcome::ModelChecked(outcome) if job == self.check_job => {
                self.check_job = 0;
                self.finish_check(&outcome);
                None
            }
            Outcome::WorkspacesCleaned {
                removed,
                kept,
                failed,
            } if job == self.workspace_job => {
                self.workspace_job = 0;
                self.report_worktrees_cleaned(removed, kept, &failed);
                None
            }
            Outcome::Checks(checks) => {
                self.apply_checks(job, checks);
                None
            }
            // A failure is gated like any other result: a superseded request's error
            // must not be announced as if it were the newest one.
            Outcome::Failed(message) if self.is_current_job(job) => {
                self.report_job_failure(job, &message);
                None
            }
            Outcome::Environment(_)
            | Outcome::EnvironmentFailed(_)
            | Outcome::Page(_)
            | Outcome::Count(_)
            | Outcome::Detail(_)
            | Outcome::Patch { .. }
            | Outcome::Catalog(_)
            | Outcome::Workspace(_)
            | Outcome::ModelChecked(_)
            | Outcome::WorkspacesCleaned { .. }
            | Outcome::Failed(_)
            | Outcome::Abandoned => None,
        }
    }

    /// Stores a detail and says what was opened (FR-2.4).
    fn apply_detail(&mut self, outcome: FetchOutcome<PullRequestDetail>) {
        self.diff_offline = outcome.offline_reason().map(|_| "offline".to_owned());
        let detail = outcome.into_value();
        self.notice(
            NoticeLevel::Info,
            format!(
                "opened #{} · {} commit(s) · {} file(s)",
                detail.summary.number,
                detail.commits.len(),
                detail.summary.changed_files
            ),
        );
        self.detail = Some(detail);
    }

    /// Adopts a freshly fetched catalog and re-resolves the configured model
    /// (FR-4.5, FR-4.7).
    fn apply_catalog(&mut self, load: crate::ports::CatalogLoad) {
        let state = crate::application::models::CatalogState {
            providers: crate::application::models::provider_choices(&load.catalog),
            load,
        };
        let summary = state.summary();
        self.catalog = Some(state);
        // A selection made in an earlier run is only usable once the catalog that
        // describes it is here, so this is where it is resolved.
        self.resolve_active_model();
        self.notice(NoticeLevel::Info, format!("model catalog: {summary}"));
        if let Some(picker) = self.picker.as_mut() {
            picker.set_notice(Some(summary));
        }
        self.refresh_picker();
    }

    /// Says what `:workspace clean` did (FR-3.1).
    fn report_worktrees_cleaned(&mut self, removed: usize, kept: usize, failed: &[String]) {
        let mut message = format!("removed {removed} worktree(s), kept {kept}");
        if !failed.is_empty() {
            let _ = std::fmt::Write::write_fmt(
                &mut message,
                format_args!("; {} could not be removed", failed.len()),
            );
            for failure in failed.iter().take(MAX_NOTICES) {
                self.notice(NoticeLevel::Warn, failure.clone());
            }
        }
        self.notice(NoticeLevel::Info, message);
    }

    /// A sentence naming the repository and account detection resolved.
    fn environment_summary(&self) -> String {
        let repo = self
            .environment
            .as_ref()
            .map_or_else(String::new, |environment| environment.repo.slug());
        let account = self
            .environment
            .as_ref()
            .and_then(|environment| environment.gh.account.clone())
            .unwrap_or_else(|| "an unknown account".to_owned());
        format!("reading {repo} as {account}")
    }

    /// Applies a fetched or cached page (FR-2.1).
    fn apply_page(
        &mut self,
        outcome: FetchOutcome<crate::ports::forge::PullRequestPage>,
    ) -> Option<Effect> {
        // A full page means the total is unknown, so it is asked for separately
        // rather than guessed at (FR-2.1).
        let wanted_count = outcome.value().total.is_none() && outcome.value().may_have_more();
        let offline = outcome.offline_reason().is_some();
        self.list.stale = offline.then(|| "offline".to_owned());

        match outcome {
            FetchOutcome::Fresh(page) | FetchOutcome::Offline { value: page, .. } => {
                self.list.replace(page);
            }
        }

        wanted_count.then_some(Effect::CountPullRequests)
    }

    /// Applies a fetched or cached patch (FR-3.3).
    fn apply_patch(
        &mut self,
        outcome: FetchOutcome<crate::domain::diff::Patch>,
        source: DiffSource,
    ) -> Option<Effect> {
        self.diff_source = source;
        self.diff_offline = outcome.offline_reason().map(|_| "offline".to_owned());
        let patch = outcome.into_value();
        let view = DiffView::with_options(
            patch,
            self.config.review.context_lines,
            self.config.review.ignore_whitespace,
        );
        let files = view.patch.stats();
        match self.detail.take() {
            Some(detail) => self.open_review(detail, view),
            None => self.set_review(view),
        }
        self.notice(
            NoticeLevel::Info,
            format!(
                "{} · from the {} · {}",
                files.label(),
                source.label(),
                self.list.status_label()
            ),
        );
        None
    }

    /// Notes that a pull request is being opened, which the indicator shows.
    pub fn begin_opening(&mut self, number: u64) {
        self.opening = Some(Opening {
            number,
            stage: OpeningStage::Detail,
            started_at: self.now_unix_secs,
        });
    }

    /// Moves the indicator on to the second step.
    pub fn advance_opening(&mut self) {
        if let Some(opening) = self.opening.as_mut() {
            opening.stage = OpeningStage::Diff;
        }
    }

    /// Stops showing the indicator.
    pub fn stop_opening(&mut self) {
        self.opening = None;
    }

    /// The pull request being opened, if one is.
    #[must_use]
    pub fn opening(&self) -> Option<Opening> {
        self.opening
    }

    /// The spinner frame, advanced by the loop's tick.
    #[must_use]
    pub fn spinner(&self) -> usize {
        self.spinner
    }

    /// The clock as of the last loop iteration.
    #[must_use]
    pub fn now_unix_secs(&self) -> u64 {
        self.now_unix_secs
    }

    /// Whether anything is in flight for the screen the user is looking at.
    #[must_use]
    pub fn loading_something(&self) -> bool {
        if self.environment_running || self.list.loading || self.list.counting {
            return true;
        }
        self.review.is_none() && self.diff_loading
    }

    /// Forgets the work that was cancelled, so the spinners stop.
    pub fn cancelled_in_flight(&mut self) {
        self.stop_opening();
        self.environment_running = false;
        self.list.loading = false;
        self.list.counting = false;
        self.diff_loading = false;
        self.notice(NoticeLevel::Info, "cancelled");
    }

    /// Whether `job` is the newest request for the slot it belongs to.
    #[must_use]
    pub fn is_current_job(&self, job: u64) -> bool {
        job == self.environment_job
            || job == self.list_job
            || job == self.count_job
            || job == self.detail_job
            || job == self.patch_job
            || job == self.doctor_job
            || job == self.catalog_job
            || job == self.workspace_job
            || job == self.check_job
    }

    /// Records a job failure where the user will see it.
    fn report_job_failure(&mut self, job: u64, message: &str) {
        if job == self.list_job {
            self.list.loading = false;
            self.list.counting = false;
            self.list.error = Some(message.to_owned());
        } else if job == self.detail_job || job == self.patch_job {
            self.diff_loading = false;
            self.stop_opening();
            self.notice(NoticeLevel::Error, format!("could not open it: {message}"));
        } else if job == self.count_job {
            // A missing count is not worth a notification: the list already says
            // "showing 50 of ≥50", which is true.
            self.list.counting = false;
        } else if job == self.catalog_job {
            self.catalog_job = 0;
            // Without a catalog there is nothing to resolve the configured model
            // against, so the status line says so rather than pretending (FR-4.5).
            self.model_problem.get_or_insert_with(|| message.to_owned());
            if let Some(picker) = self.picker.as_mut() {
                picker.set_notice(Some(message.to_owned()));
                self.notice(NoticeLevel::Warn, format!("model catalog: {message}"));
            }
        } else if job == self.workspace_job {
            // Not being able to materialise the code is not fatal: the diff from the
            // forge is already on screen, and only the toggles need the worktree.
            self.workspace_job = 0;
            self.notice(
                NoticeLevel::Warn,
                format!("working from the remote diff: {message}"),
            );
        } else if job == self.check_job {
            self.check_job = 0;
            if let Some(picker) = self.picker.as_mut() {
                picker.set_checking(false);
                picker.set_notice(Some(message.to_owned()));
            }
            self.notice(NoticeLevel::Error, format!("model check failed: {message}"));
        } else {
            self.notice(NoticeLevel::Error, message.to_owned());
        }
        logging::log(Level::Warn, format!("job failed: {message}"));
    }

    /// Handles a mouse event (FR-7.5).
    ///
    /// The wheel scrolls whatever the pointer is over, and a click focuses the pane
    /// and moves the selection to the row under the pointer.
    pub fn on_mouse(&mut self, event: event::MouseEvent) -> Effect {
        match event.kind {
            event::MouseEventKind::ScrollDown => {
                self.scroll_at(event.column, event.row, 1);
                Effect::None
            }
            event::MouseEventKind::ScrollUp => {
                self.scroll_at(event.column, event.row, -1);
                Effect::None
            }
            event::MouseEventKind::Down(event::MouseButton::Left) => {
                self.click_at(event.column, event.row);
                Effect::None
            }
            _ => Effect::None,
        }
    }

    /// Scrolls the pane under the pointer.
    fn scroll_at(&mut self, column: u16, row: u16, delta: i32) {
        // Three rows a notch, and it scrolls the *text*: a wheel is for moving what you
        // are reading. Moving the selection instead reads as the wheel being broken —
        // or backwards — because the text only budges once the selection reaches the
        // edge of the window.
        let pane = self.pane_at(column, row).unwrap_or(self.focus);
        match pane {
            Pane::PullRequests => {
                self.list.scroll_by(delta * 3);
                self.focus = Pane::PullRequests;
            }
            Pane::Diff => {
                if let Some(view) = self.review.as_mut() {
                    // The tree and the diff share the region; the tree is the narrow
                    // one on the left.
                    if column < self.tree_width {
                        view.scroll_tree_by(delta * 3);
                    } else {
                        view.tree_focused = false;
                        view.scroll_by(delta * 3);
                    }
                }
            }
        }
    }

    /// Focuses the pane under the pointer and moves its cursor to the row clicked.
    fn click_at(&mut self, column: u16, row: u16) {
        let Some(pane) = self.pane_at(column, row) else {
            return;
        };
        self.focus = pane;
        match pane {
            Pane::PullRequests => {
                // The visible row under the pointer, turned into an absolute index by
                // the same offset the frame drew with.
                if let Some(index) =
                    crate::tui::components::pr_list::row_at(self.list_pane, self.list.scroll, row)
                    && index < self.list.visible_len()
                {
                    self.list.select_visible(index);
                }
            }
            Pane::Diff => {
                // The visible row under the pointer, turned into an absolute index by
                // the offset the frame drew with. Passing the offset itself to
                // `move_by`/`move_tree` (both of which are *relative*) is what made
                // clicks land somewhere else entirely once anything had scrolled.
                let Some(offset) = self.review_row(row) else {
                    return;
                };
                if let Some(view) = self.review.as_mut() {
                    if column < self.tree_width {
                        let index = view.tree_scroll + offset;
                        // Only a row that exists: clicking the empty space below the
                        // last file must not open it.
                        if index < view.tree.len() {
                            view.select_tree_row(index);
                            // A click on a tree row does what pressing Enter on it
                            // does: opening a file, or folding a folder (FR-7.5).
                            view.activate_tree();
                        }
                    } else {
                        let index = view.scroll + offset;
                        if index < view.rows.len() {
                            view.tree_focused = false;
                            view.select_row(index);
                        }
                    }
                }
            }
        }
    }

    /// The row of a review pane a terminal row is over, as an offset into the visible
    /// rows, or `None` when it is over a border, the tab bar, or nothing.
    fn review_row(&self, row: u16) -> Option<usize> {
        let first = self.review_top.saturating_add(1);
        let last = self.body_rect.bottom().saturating_sub(2);
        if row < first || row > last {
            return None;
        }
        Some(usize::from(row - first))
    }

    /// Which pane a terminal coordinate is in, if any.
    ///
    /// Anything outside a pane is `None`: a click on the filter bar or on a border
    /// must not move a cursor somewhere the user did not point at.
    fn pane_at(&self, column: u16, row: u16) -> Option<Pane> {
        if self.review.is_some() {
            // The tree and the diff share the area below the tab bar.
            return (row >= self.review_top).then_some(Pane::Diff);
        }
        self.list_pane
            .contains(ratatui::layout::Position::from((column, row)))
            .then_some(Pane::PullRequests)
    }

    /// Records the pane geometry of the last frame, so a mouse event can be placed.
    ///
    /// Called from `render`, which is where the sizes are known. It is arithmetic
    /// only: the render path still performs no IO.
    fn record_geometry(&mut self, body: ratatui::layout::Rect) {
        self.last_width = body.width;
        self.body_rect = body;
        self.tree_width = crate::tui::components::review::TREE_WIDTH;
        self.review_top = body.y + 1;
        // The filter bar sits above the list; the pane below it is the one the mouse
        // is tested against, and the same rectangle the renderer draws into.
        let (filter_bar, list) = components::panes::body_split(body);
        self.filter_bar_rect = filter_bar;
        self.list_pane = list;
    }

    /// Recomputes the scroll offsets for the panes that are about to be drawn.
    /// Works out the scroll offsets for the panes about to be drawn.
    ///
    /// The renderer and the mouse both read the result, so the row a click maps to is
    /// the row that was drawn there.
    fn sync_scroll(&mut self) {
        let inner = self.review_body_height();
        if let Some(view) = self.review.as_mut() {
            view.prepare(inner, inner);
        }
        let height = crate::tui::components::pr_list::layout(self.list_pane).height;
        let position = self.list.cursor_position().unwrap_or(0);
        self.list.scroll = crate::tui::components::ensure_visible(
            position,
            self.list.scroll,
            height,
            self.list.visible_len(),
        );
        // The wheel offsets are in visible rows, and a search or a refresh can change
        // how many there are, so the stored offset is clamped after either.
        self.list.scroll = self
            .list
            .scroll
            .min(self.list.visible_len().saturating_sub(height));
    }

    /// How tall the review panes' row area is.
    fn review_body_height(&self) -> u16 {
        // The body, less the tab row, less the two borders.
        self.list_pane
            .height
            .saturating_add(crate::tui::components::filter_bar::HEIGHT)
            .saturating_sub(3)
    }

    /// Opens the command line with a prefix already typed (FR-7.4).
    ///
    /// Used by `<leader>f` and `<leader>s`, which are shortcuts for a command rather
    /// than a second implementation of one.
    pub fn open_command(&mut self, prefix: &str) {
        self.cancel_overlay();
        self.command.clear();
        for character in prefix.chars() {
            self.command.push(character);
        }
        self.mode = Mode::Command;
    }

    /// Turns mouse capture off or on at runtime, for `:set mouse=` (FR-7.5).
    pub fn show_mouse(&mut self, enabled: bool) {
        self.config.ui.mouse = enabled;
    }

    /// The terminal width of the last frame.
    ///
    /// Wrapping a toggle needs the width the *user* has, and only the renderer knows
    /// it; it is recorded rather than guessed at.
    #[must_use]
    pub fn terminal_width(&self) -> u16 {
        self.last_width
    }

    /// Records that the environment was resolved (FR-1.1).
    pub fn set_environment(&mut self, environment: Environment) {
        self.environment = Some(environment);
        self.environment_error = None;
        self.environment_running = false;
    }

    /// Records that the environment could not be resolved (FR-1.1).
    pub fn set_environment_error(&mut self, error: EnvironmentError) {
        self.environment_error = Some(error);
        self.environment_running = false;
    }

    /// Sets the clock for this iteration of the event loop.
    ///
    /// The reducer never reads the clock itself, so this is how "now" reaches it —
    /// and how a test can render the same frame twice.
    pub fn set_now(&mut self, now_unix_secs: u64) {
        if self.started_at == 0 {
            self.started_at = now_unix_secs;
        }
        self.now_unix_secs = now_unix_secs;
    }

    /// How many keys the configuration document holds, including the ones this
    /// build does not understand (FR-8.6).
    #[must_use]
    pub fn document_key_count(&self) -> usize {
        self.document.key_count()
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
    /// The error currently shown under the command line, if any.
    #[must_use]
    pub fn command_error_text(&self) -> Option<&str> {
        self.command.error.as_deref()
    }

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

    /// Switches to the next available theme, wrapping around (FR-7.7).
    ///
    /// Cycles through everything the user has: the built-ins plus any theme files
    /// they added, in the order the picker lists them.
    pub(crate) fn toggle_theme(&mut self) -> Effect {
        if self.theme_names.is_empty() {
            self.notice(NoticeLevel::Warn, "no themes are available");
            return Effect::None;
        }
        let next = self
            .theme_names
            .iter()
            .position(|name| name == &self.theme_request)
            .map_or(0, |index| (index + 1) % self.theme_names.len());
        let name = self.theme_names[next].clone();
        self.set_theme(&name)
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
        if overlay == Overlay::Help {
            self.help_scroll = 0;
        }
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
                // One entry per theme *name*, which was read once at startup. The
                // file behind the entry is read when the cursor lands on it, so
                // opening the picker costs nothing and moving down one row costs one
                // small local read (FR-7.7).
                self.picker_items = self
                    .theme_names
                    .iter()
                    .cloned()
                    .map(|name| PickerEntry {
                        name,
                        theme: None,
                        source: None,
                    })
                    .collect();
                self.picker_cursor = self
                    .picker_items
                    .iter()
                    .position(|entry| entry.name == self.theme_request)
                    .unwrap_or(0);
                self.preview_picker();
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
        // The indicator's spinner is driven from here: the loop already calls this once
        // per iteration, which is exactly the cadence an animation wants.
        self.spinner = self.spinner.wrapping_add(1);
        self.tick_at(Instant::now());
    }

    /// Expiry with the clock passed in, so it is testable.
    pub(crate) fn tick_at(&mut self, now: Instant) {
        self.notices.retain(|notice| notice.is_live(now));
    }

    /// Handles one key press (FR-7.1, FR-7.2).
    pub fn on_key(&mut self, event: KeyEvent) -> Effect {
        let combo = keymap::normalize(KeyCombo::from(event));
        // The picker takes every key while it is open: it is a text-entry surface, so
        // a bare `j` belongs in its filter rather than in whatever is behind it
        // (FR-7.2's rule, applied to the modal).
        if self.picker.is_some() {
            return self.on_picker_key(combo);
        }
        match self.mode {
            Mode::Command => self.on_command_key(combo),
            Mode::Search => self.on_search_key(combo),
            Mode::Popup => self.on_popup_key(combo),
            Mode::Normal | Mode::Insert | Mode::Visual => self.on_normal_key(combo),
        }
    }

    /// Opens the model picker and asks for the catalog (FR-4.5, FR-4.7).
    pub(crate) fn open_model_picker(&mut self) -> Effect {
        let mut picker = PickerState::new();
        picker.set_notice(Some("loading the model catalog…".to_owned()));
        self.picker = Some(picker);
        self.refresh_picker();
        // A cache that is fresh answers instantly; a stale one is refreshed in the
        // background, and the picker shows it either way (FR-4.7).
        Effect::LoadCatalog(CatalogPolicy::CacheFirst)
    }

    /// Records the job id of the catalog fetch (FR-4.7).
    pub(crate) fn record_catalog_job(&mut self, job: u64) {
        self.catalog_job = job;
    }

    /// Records the job id of the worktree being built (FR-3.1).
    pub(crate) fn record_workspace_job(&mut self, job: u64) {
        self.workspace_job = job;
    }

    /// Records the job id of the connection check (FR-4.5).
    pub(crate) fn record_check_job(&mut self, job: u64) {
        self.check_job = job;
        if let Some(picker) = self.picker.as_mut() {
            picker.set_checking(true);
            picker.set_notice(Some("asking the provider…".to_owned()));
        }
    }

    /// The selection currently configured, if it resolves (FR-4.5).
    #[must_use]
    pub fn active_model(&self) -> Option<&crate::application::models::ResolvedSelection> {
        self.active_model.as_ref()
    }

    /// Why the configured model cannot be used, if it cannot.
    #[must_use]
    pub fn model_problem(&self) -> Option<&str> {
        self.model_problem.as_deref()
    }

    /// Where the diff on screen came from (FR-3.2).
    #[must_use]
    pub fn diff_source(&self) -> DiffSource {
        self.diff_source
    }

    /// The picker, when it is open.
    #[must_use]
    pub fn picker(&self) -> Option<&PickerState> {
        self.picker.as_ref()
    }

    /// The managed worktrees on disk (FR-3.1).
    ///
    /// A directory listing, read on demand: the same bounded local read the theme
    /// list does, and cheap enough that `:workspace` does not need a job.
    ///
    /// # Errors
    ///
    /// Returns the message from the filesystem when the directory cannot be read.
    pub fn worktrees(
        &self,
    ) -> std::result::Result<Vec<crate::ports::workspace::WorkspaceEntry>, String> {
        self.workspace_port
            .list()
            .map_err(|error| error.to_string())
    }

    /// The credential store (FR-4.5).
    #[must_use]
    pub fn secret_store(&self) -> &dyn SecretStore {
        self.secret_store.as_ref()
    }

    /// The selection the picker has assembled so far, if it is complete (FR-4.5).
    #[must_use]
    pub fn picker_selection(&self) -> Option<ModelSelection> {
        let picker = self.picker.as_ref()?;
        Some(ModelSelection {
            provider: picker.provider()?.to_owned(),
            model: picker.model()?.to_owned(),
            temperature: None,
            max_tokens: None,
            reasoning: picker.thinking().cloned(),
        })
    }

    /// The picker, mutably, for the cases that have to change what it says.
    pub(crate) fn picker_mut(&mut self) -> Option<&mut PickerState> {
        self.picker.as_mut()
    }

    /// Closes the picker (FR-4.5).
    pub(crate) fn close_picker(&mut self) {
        self.picker = None;
    }

    /// Makes a selection the active one, and resolves it (FR-4.5).
    pub(crate) fn set_active_selection(&mut self, selection: ModelSelection) {
        self.config.llm.active = Some(selection);
        self.resolve_active_model();
    }

    /// Whether the app should materialise worktrees at all (FR-3.1).
    #[must_use]
    pub fn wants_workspace(&self) -> bool {
        // `[workspace].mode = "remote"` is how a user says "never write a worktree",
        // and it is honoured here rather than in the adapter so the reason is visible.
        !self.config.workspace.mode.eq_ignore_ascii_case("remote")
    }

    /// Whether the open pull request's worktree matches the commit on screen
    /// (FR-3.1, FR-4.3).
    #[must_use]
    pub fn workspace_ready(&self) -> bool {
        match (&self.workspace, &self.detail) {
            (Some(workspace), Some(detail)) => workspace.head_sha == detail.summary.head_sha,
            _ => false,
        }
    }

    /// The request that asks the provider whether the active model works (FR-4.5).
    #[must_use]
    pub fn check_request(&self) -> Option<crate::ports::llm::ChatRequest> {
        let resolved = self.active_model.as_ref()?;
        let key = self
            .secret_store
            .get(&resolved.provider, resolved.env_var.as_deref())
            .ok()
            .flatten()?;
        Some(crate::application::models::connection_check(resolved, key))
    }

    /// The provider the picker is on, if it has chosen one (FR-4.5).
    #[must_use]
    pub fn picker_provider(&self) -> Option<String> {
        self.picker
            .as_ref()
            .and_then(|picker| picker.provider().map(str::to_owned))
    }

    /// Fills the picker's rows for the step it is on (FR-4.5).
    pub(crate) fn refresh_picker(&mut self) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let step = picker.step();
        // The picker says where its list came from, which is how a user can tell a
        // stale catalog from a fresh one without opening `:catalog` (FR-4.7).
        let source = self.catalog.as_ref().map(CatalogState::summary);
        let query = picker.query().to_owned();
        let rows = match step {
            model_picker::Step::Provider => self
                .catalog
                .as_ref()
                .map(|state| provider_rows(state, &query))
                .unwrap_or_default(),
            model_picker::Step::Model => {
                let provider = picker.provider().map(str::to_owned);
                match (self.catalog.as_ref(), provider) {
                    (Some(state), Some(provider)) => {
                        model_rows(state, &provider, &query, now_year(self.now_unix_secs()))
                    }
                    _ => Vec::new(),
                }
            }
            model_picker::Step::Thinking => {
                let chosen = (
                    picker.provider().map(str::to_owned),
                    picker.model().map(str::to_owned),
                );
                match (self.catalog.as_ref(), chosen) {
                    (Some(state), (Some(provider), Some(model))) => {
                        thinking_rows(state, &provider, &model)
                    }
                    _ => Vec::new(),
                }
            }
            model_picker::Step::Key | model_picker::Step::Confirm => Vec::new(),
        };
        if let Some(picker) = self.picker.as_mut() {
            picker.set_rows(rows);
            if let Some(source) = source
                && picker.notice().is_none()
            {
                picker.set_notice(Some(source));
            }
        }
    }

    /// Handles a key while the picker is open (FR-4.5).
    fn on_picker_key(&mut self, combo: KeyCombo) -> Effect {
        let Some(mut picker) = self.picker.take() else {
            return Effect::None;
        };
        let mut effect = Effect::None;
        let mut close = false;
        match combo.code {
            // `Esc` walks back a step, and closes the picker from the first step:
            // that is "back out without changing anything" (FR-4.5).
            KeyCode::Esc => close = !picker.step_back(),
            KeyCode::Enter => match picker.advance() {
                // Nothing to do for either: the picker has already moved, or has put
                // the reason on screen itself.
                model_picker::PickerStep::Moved | model_picker::PickerStep::Refused(_) => {}
                model_picker::PickerStep::KeyTyped { provider, key } => {
                    effect = Effect::SaveKey { provider, key };
                }
                model_picker::PickerStep::Commit {
                    provider,
                    model,
                    thinking,
                } => {
                    let selection = ModelSelection {
                        provider,
                        model,
                        temperature: None,
                        max_tokens: None,
                        reasoning: thinking,
                    };
                    effect = Effect::SaveSelection(Box::new(selection));
                }
            },
            KeyCode::Up => picker.move_cursor(-1),
            KeyCode::Down => picker.move_cursor(1),
            KeyCode::Char('p') if combo.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.move_cursor(-1);
            }
            KeyCode::Char('n') if combo.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.move_cursor(1);
            }
            KeyCode::Char('u') if combo.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.clear_query();
            }
            KeyCode::Backspace => picker.backspace(),
            // A bare character is text here, including `j`, `k`, `q` and the leader
            // key: the alternative is a search box that cannot contain a `j`.
            KeyCode::Char(character)
                if !combo.modifiers.contains(KeyModifiers::CONTROL)
                    && !combo.modifiers.contains(KeyModifiers::ALT) =>
            {
                picker.push_char(character);
            }
            _ => {}
        }
        if close {
            self.picker = None;
        } else {
            self.picker = Some(picker);
            self.refresh_picker();
        }
        effect
    }

    /// Marks the picker's step as having stored a key (FR-4.5).
    pub(crate) fn picker_key_stored(&mut self) {
        if let Some(picker) = self.picker.as_mut() {
            picker.key_stored();
        }
        self.refresh_picker();
    }

    /// Resolves the configured selection against the catalog (FR-4.5).
    pub(crate) fn resolve_active_model(&mut self) {
        let Some(state) = self.catalog.as_ref() else {
            return;
        };
        let Some(selection) = self.config.llm.active.clone() else {
            self.active_model = None;
            self.model_problem = None;
            return;
        };
        match crate::application::models::resolve_selection(
            &state.load.catalog,
            &selection,
            self.secret_store.as_ref(),
        ) {
            Ok(resolved) => {
                for warning in &resolved.warnings {
                    self.notice(NoticeLevel::Warn, format!("model: {warning}"));
                }
                self.model_problem = None;
                self.active_model = Some(resolved);
            }
            Err(error) => {
                self.active_model = None;
                self.model_problem = Some(error.to_string());
            }
        }
    }

    /// Reports the result of the connection check (FR-4.5).
    fn finish_check(&mut self, outcome: &crate::ports::llm::ChatOutcome) {
        let answer = outcome.text.trim().to_owned();
        let usage = outcome.usage.map_or_else(
            || "no usage reported".to_owned(),
            |usage| {
                format!(
                    "{} prompt + {} completion tokens",
                    usage.prompt, usage.completion
                )
            },
        );
        let message = if answer.is_empty() {
            format!("the provider answered with no text ({usage})")
        } else {
            format!("the provider answered ({usage})")
        };
        if let Some(picker) = self.picker.as_mut() {
            picker.set_checking(false);
            picker.set_notice(Some(message.clone()));
        }
        self.notice(NoticeLevel::Info, message);
    }

    /// Scrolls the roadmap or the picker with the mouse wheel (FR-7.5).
    /// Fires a pending sequence once its timeout expires (FR-7.2).
    pub fn on_timeout(&mut self) -> Effect {
        self.deadline = None;
        if self.pending.is_empty() || self.leader_open {
            return Effect::None;
        }

        match self.step(true) {
            Step::Fired(effect) => {
                if effect != Effect::KeepPending {
                    self.finish_sequence();
                }
                effect
            }
            // `g` on its own is a prefix of `gg` but is not bound to anything;
            // dropping it silently is the vim behaviour.
            Step::Waiting | Step::Unmatched => {
                self.pending.clear();
                Effect::None
            }
        }
    }

    /// Opens the doctor popup and marks the report as being collected (FR-9.3).
    pub(crate) fn start_doctor(&mut self) {
        self.checks = Vec::new();
        self.doctor_running = true;
        self.open_overlay(Overlay::Doctor);
        self.notice(NoticeLevel::Info, "collecting the environment report…");
    }

    /// Delivers a doctor report collected off the event loop (FR-9.3).
    ///
    /// The job id is checked because a report for a superseded request must not
    /// replace a newer one.
    pub(crate) fn apply_checks(&mut self, job: u64, checks: Vec<Check>) {
        if job != self.doctor_job {
            return;
        }
        self.checks = checks;
        self.doctor_running = false;
    }

    /// Surfaces the startup warnings without consuming them: the shell pane and
    /// `:doctor` report the same list, and `App::warnings` is what they read.
    fn report_startup_warnings(&mut self) {
        for warning in &self.warnings {
            logging::log(Level::Warn, warning);
        }
        let announced: Vec<String> = self.warnings.iter().take(MAX_NOTICES).cloned().collect();
        for warning in announced {
            self.notice(NoticeLevel::Warn, warning);
        }
    }

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

    /// Whether a key press matches a global binding that should apply while a
    /// command is being typed (FR-8.3).
    ///
    /// Only combinations with a real modifier qualify. On the command line a bare
    /// `?`, `:` or the leader key is text the user is typing, not a command —
    /// treating them as bindings makes `:set ui.timeoutlen=250` impossible,
    /// because the space would open the leader menu.
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

        match self.step(false) {
            Step::Fired(effect) => {
                if effect != Effect::KeepPending {
                    self.finish_sequence();
                }
                effect
            }
            Step::Waiting => Effect::None,
            Step::Unmatched => {
                let keys = keymap::describe_sequence(&self.pending);
                let was_leader = self.leader_open;
                self.finish_sequence();
                // A modified global must still work while a longer sequence is
                // pending: `<Space>` then `<C-c>` has to quit, not complain.
                if let Some(effect) = self.rescue_global(combo) {
                    return effect;
                }
                let level = if was_leader {
                    NoticeLevel::Info
                } else {
                    NoticeLevel::Warn
                };
                self.notice(level, format!("`{keys}` is not bound"));
                Effect::None
            }
        }
    }

    /// Advances the pending sequence one step.
    ///
    /// `fire_ambiguous` makes a sequence that is both a binding and a prefix of a
    /// longer one (the leader, typically) fire its own binding, which is what the
    /// timeout does; on a key press it means "wait for the next key".
    fn step(&mut self, fire_ambiguous: bool) -> Step {
        let timeout = self.keymap.timeout();
        let mode = self.mode;

        let action = match self.keymap.resolve(mode, &self.pending) {
            Resolution::Match(binding) => binding.action.clone(),
            Resolution::Ambiguous(binding) if fire_ambiguous => binding.action.clone(),
            Resolution::Ambiguous(binding) => {
                let action = binding.action.clone();
                if action::is_hint(&action) {
                    // Show the hints now, not after `timeoutlen` (which-key
                    // behaviour): the sequence stays pending, so the next key can
                    // still complete a longer binding (FR-7.3).
                    self.deadline = Some(Instant::now() + timeout);
                    return Step::Fired(update::dispatch(self, &action));
                }
                self.deadline = Some(Instant::now() + timeout);
                return Step::Waiting;
            }
            Resolution::Prefix => {
                self.deadline = Some(Instant::now() + timeout);
                return Step::Waiting;
            }
            Resolution::None => return Step::Unmatched,
        };

        self.deadline = None;
        Step::Fired(update::dispatch(self, &action))
    }

    /// Fires a modified global binding that the pending sequence swallowed.
    fn rescue_global(&mut self, combo: KeyCombo) -> Option<Effect> {
        if !combo
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return None;
        }
        match self.keymap.resolve_global(&[combo]) {
            Resolution::Match(binding) => {
                let action = binding.action.clone();
                self.finish_sequence();
                Some(update::dispatch(self, &action))
            }
            _ => None,
        }
    }

    fn on_popup_key(&mut self, combo: KeyCombo) -> Effect {
        // Popups resolve the popup scope first (globals included), so
        // `[keys.popup]` bindings work, multi-key sequences included. The arms
        // below are the built-in popup behaviour that needs no binding (FR-8.3).
        self.pending.push(combo);

        match self.step(false) {
            Step::Fired(effect) => {
                if effect != Effect::KeepPending {
                    self.finish_sequence();
                }
                return effect;
            }
            Step::Waiting => return Effect::None,
            Step::Unmatched => {
                self.pending.clear();
                self.deadline = None;
            }
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
            // The help list is longer than most terminals, so it scrolls.
            KeyCode::Char('j') | KeyCode::Down if self.overlay == Overlay::Help => {
                self.help_scroll = self.help_scroll.saturating_add(1);
                Effect::None
            }
            KeyCode::Char('k') | KeyCode::Up if self.overlay == Overlay::Help => {
                self.help_scroll = self.help_scroll.saturating_sub(1);
                Effect::None
            }
            KeyCode::Char('g') if self.overlay == Overlay::Help => {
                self.help_scroll = 0;
                Effect::None
            }
            KeyCode::Char('G') if self.overlay == Overlay::Help => {
                self.help_scroll = usize::MAX;
                Effect::None
            }
            KeyCode::Char('d') if self.overlay == Overlay::Help => {
                self.help_scroll = self.help_scroll.saturating_add(10);
                Effect::None
            }
            KeyCode::Char('u') if self.overlay == Overlay::Help => {
                self.help_scroll = self.help_scroll.saturating_sub(10);
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

    /// Loads the theme the picker cursor is on, and previews it.
    ///
    /// A file that cannot be read is reported here rather than when the choice is
    /// committed, so the user never ends up with a theme they cannot see.
    fn preview_picker(&mut self) {
        let Some(entry) = self.picker_items.get(self.picker_cursor) else {
            return;
        };
        if entry.theme.is_some() {
            return;
        }
        let name = entry.name.clone();
        let mut warnings = Vec::new();
        match theme::load(&self.home, &name, &mut warnings) {
            Ok((theme, source)) => {
                self.theme = theme;
                name.clone_into(&mut self.theme_request);
                if let Some(entry) = self.picker_items.get_mut(self.picker_cursor) {
                    entry.theme = Some(self.theme.clone());
                    entry.source = Some(source.clone());
                }
                self.theme_source = source;
            }
            Err(error) => {
                self.notice(NoticeLevel::Warn, format!("{name}: {error}"));
            }
        }
    }

    fn move_picker(&mut self, delta: i32) {
        let count = self.picker_items.len();
        if count == 0 {
            return;
        }
        self.picker_cursor = clamp_cursor(self.picker_cursor, delta, count);
        // Loading the entry the cursor landed on also previews it; `cancel_overlay`
        // puts the original theme back.
        self.preview_picker();
    }

    /// Typing in the `/` box filters the loaded list as the user types (FR-2.2).
    ///
    /// Ordinary characters are text, exactly as on the command line: a search for
    /// "n" must not jump to the next match, and a search for "x" must not clear the
    /// filters. Only a modified key can be a binding here.
    fn on_search_key(&mut self, combo: KeyCombo) -> Effect {
        if let Some(action) = self.global_action(combo) {
            return update::dispatch(self, &action);
        }
        if combo.code == KeyCode::Esc || combo.code == KeyCode::Enter {
            self.mode = Mode::Normal;
            return Effect::None;
        }

        match combo.code {
            KeyCode::Backspace => {
                let mut text = self.list.search.clone();
                text.pop();
                self.list.set_search(&text);
                self.list.stale = None;
                Effect::None
            }
            // Ctrl-U clears the box, which is the one shortcut worth having while
            // typing in it.
            KeyCode::Char('u') if combo.modifiers == KeyModifiers::CONTROL => {
                self.list.set_search("");
                self.list.stale = None;
                Effect::None
            }
            KeyCode::Char(value)
                if combo.modifiers.is_empty() || combo.modifiers == KeyModifiers::SHIFT =>
            {
                let mut text = self.list.search.clone();
                text.push(value);
                self.list.set_search(&text);
                self.list.stale = None;
                Effect::None
            }
            _ => Effect::None,
        }
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

        self.record_geometry(rows[1]);
        self.sync_scroll();

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
    fn navigation_moves_the_list_cursor_and_stops_at_the_ends() {
        let (_dir, mut app) = list_app();
        assert_eq!(app.list.selected().unwrap().number, 1);

        press(&mut app, "j");
        assert_eq!(app.list.selected().unwrap().number, 2);
        press(&mut app, "j");
        assert_eq!(app.list.selected().unwrap().number, 3);
        press(&mut app, "k");
        press(&mut app, "k");
        assert_eq!(app.list.selected().unwrap().number, 1);
        press(&mut app, "k");
        assert_eq!(
            app.list.selected().unwrap().number,
            1,
            "and not past the top"
        );

        press(&mut app, "G");
        assert_eq!(app.list.selected().unwrap().number, 5);
        press(&mut app, "gg");
        assert_eq!(app.list.selected().unwrap().number, 1);
    }

    #[test]
    fn the_page_keys_move_the_list_too() {
        let (_dir, mut app) = list_app();
        app.list.viewport = 2;
        // A two-row viewport: half a screen is one row, a whole screen is two.
        press(&mut app, "<C-d>");
        assert_eq!(app.list.selected().unwrap().number, 2, "half a screen");
        press(&mut app, "<C-f>");
        assert_eq!(app.list.selected().unwrap().number, 4, "a whole screen");
        press(&mut app, "<C-u>");
        assert_eq!(app.list.selected().unwrap().number, 3, "half a screen back");
    }

    #[test]
    fn an_unbound_key_reports_itself_and_clears_the_sequence() {
        let (_dir, mut app) = app();
        // `z` is the prefix of `za` now, so this uses a key bound to nothing.
        press(&mut app, "Q");
        assert!(app.pending.is_empty());
        let notice = app.latest_notice().expect("a notice should be shown");
        assert!(notice.text.contains('Q'), "{:?}", notice.text);
        assert_eq!(notice.level, NoticeLevel::Warn);
    }

    #[test]
    fn the_leader_menu_opens_on_the_key_press() {
        // A hints menu must not wait for the ambiguity timeout: the whole point of
        // pressing the leader is to see what is available (FR-7.3).
        let (_dir, mut app) = app();
        press(&mut app, "<Space>");
        assert_eq!(app.pending.len(), 1, "the sequence stays open");
        assert!(app.leader_open);
        assert_eq!(app.overlay(), Overlay::Leader);

        // A continuation still completes the longer binding.
        press(&mut app, "?");
        assert_eq!(app.overlay(), Overlay::Help);
        assert!(app.pending.is_empty());
    }

    #[test]
    fn an_ambiguous_binding_that_is_not_a_hint_still_waits() {
        let dir = temp_home();
        dir.write(
            "keybinds.toml",
            "[keys.normal]\n\"g\" = \"nav.top\"\n\"gg\" = \"nav.bottom\"\n",
        );
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
        app.set_pull_requests(crate::ports::forge::PullRequestPage::complete(
            (1..=5).map(summary).collect(),
            50,
        ));
        app.list.move_cursor(3);
        assert_eq!(app.list.selected().unwrap().number, 4);

        // `g` is bound and is also a prefix of `gg`, so it waits rather than
        // firing immediately: only hints short-circuit the timeout.
        press(&mut app, "g");
        assert_eq!(
            app.list.selected().unwrap().number,
            4,
            "the shorter binding must not fire yet"
        );
        app.on_timeout();
        assert_eq!(
            app.list.selected().unwrap().number,
            1,
            "the timeout fires `g`, which is nav.top"
        );

        app.list.move_cursor(3);
        press(&mut app, "gg");
        assert_eq!(
            app.list.selected().unwrap().number,
            5,
            "the longer binding is the one that fires"
        );
    }

    #[test]
    fn toggling_cycles_through_every_theme() {
        let (_dir, mut app) = app();
        assert_eq!(app.theme.name(), "dark");
        assert_eq!(
            app.theme_names,
            vec!["dark".to_owned(), "light".to_owned()],
            "the list is read once at startup"
        );

        let effect = press(&mut app, "<Space>T");
        assert_eq!(effect, Effect::SaveState);
        assert_eq!(app.theme.name(), "light");

        // Wrap around rather than stopping at the last theme.
        press(&mut app, "<Space>T");
        assert_eq!(app.theme.name(), "dark");
    }

    #[test]
    fn toggling_and_the_picker_agree_on_the_current_theme() {
        let (_dir, mut app) = app();
        press(&mut app, "<Space>T");
        assert_eq!(app.theme.name(), "light");
        press(&mut app, "<Space>t");
        assert_eq!(app.picker_cursor(), 1, "the picker marks the toggled theme");
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

        // The job id is what the loop stamps on the request and what the report
        // carries back.
        let job = 7;
        app.record_job(&Effect::RunDoctor, job);
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
        app.record_job(&Effect::RunDoctor, 1);

        // A second request supersedes the first.
        press(&mut app, ":");
        for character in "doctor".chars() {
            press(&mut app, &character.to_string());
        }
        press(&mut app, "<CR>");
        app.record_job(&Effect::RunDoctor, 2);

        app.apply_checks(1, Vec::new());
        assert!(
            app.doctor_running,
            "the stale report must not resolve the job"
        );

        app.apply_checks(
            2,
            vec![Check {
                name: "home",
                status: crate::doctor::Status::Ok,
                detail: "ok".to_owned(),
            }],
        );
        assert!(!app.doctor_running);
    }

    #[test]
    fn a_leader_binding_closes_the_menu_when_it_fires() {
        // Needs rows, because the assertion is about what the cursor did afterwards.
        // The menu opens on the ambiguity timeout, so a test that fires
        // <leader>t without waiting would never have opened it and would miss
        // this (FR-7.3).
        let (_dir, mut app) = list_app();
        press(&mut app, "<Space>");
        app.on_timeout();
        assert_eq!(app.overlay(), Overlay::Leader);

        press(&mut app, "T");
        assert_eq!(app.theme.name(), "light");
        assert!(!app.leader_open);
        assert_eq!(
            app.overlay(),
            Overlay::None,
            "the leader menu must not outlive the binding it fired"
        );

        // And the cursor is reachable again.
        press(&mut app, "j");
        assert_eq!(app.list.selected().map(|pr| pr.number), Some(2));
    }

    #[test]
    fn the_leader_menu_opened_from_a_popup_still_completes() {
        // The popup path did not seed the pending sequence, so every entry of a
        // menu opened with <Space> from a popup was dead (FR-7.3).
        let (_dir, mut app) = app();
        press(&mut app, "?");
        assert_eq!(app.overlay(), Overlay::Help);

        press(&mut app, "<Space>");
        app.on_timeout();
        assert_eq!(app.overlay(), Overlay::Leader);
        assert_eq!(app.pending.len(), 1, "the leader must stay pending");

        press(&mut app, "t");
        assert_eq!(app.overlay(), Overlay::ThemePicker);
    }

    #[test]
    fn the_command_line_closes_an_open_popup() {
        // `:` used to leave the popup on screen, so it stopped being modal and
        // the mode/overlay pairing was one open_overlay never produces (FR-7.1).
        let (_dir, mut app) = app();
        press(&mut app, "?");
        press(&mut app, ":");
        assert_eq!(app.mode(), Mode::Command);
        assert_eq!(app.overlay(), Overlay::None);

        press(&mut app, "<Esc>");
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.overlay(), Overlay::None);
    }

    #[test]
    fn a_modified_global_still_works_mid_sequence() {
        // `<Space>` then `<C-c>` used to be reported as "not bound" instead of
        // quitting (FR-8.3).
        let (_dir, mut app) = app();
        press(&mut app, "<Space>");
        app.on_timeout();
        assert!(app.leader_open);
        press(&mut app, "<C-c>");
        assert!(app.should_quit());
    }

    #[test]
    fn popup_bindings_from_the_keymap_fire() {
        let dir = temp_home();
        dir.write(
            "keybinds.toml",
            "[keys.popup]\n\"x\" = \"app.theme_picker\"\n\"z z\" = \"app.help\"\n",
        );
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

        // A single-key popup binding.
        press(&mut app, "?");
        assert_eq!(app.overlay(), Overlay::Help);
        press(&mut app, "x");
        assert_eq!(app.overlay(), Overlay::ThemePicker);

        // A multi-key popup binding.
        press(&mut app, "<Esc>");
        press(&mut app, "?");
        press(&mut app, "z");
        press(&mut app, "z");
        assert_eq!(
            app.overlay(),
            Overlay::Help,
            "multi-key popup sequences work"
        );
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

    /// Draws a frame and hands back the terminal, so a test can ask where something
    /// actually appeared.
    fn drawn(
        app: &mut App,
        width: u16,
        height: u16,
    ) -> ratatui::Terminal<ratatui::backend::TestBackend> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        terminal
    }

    /// The terminal row whose text contains `needle`.
    fn row_of(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>, needle: &str) -> u16 {
        let buffer = terminal.backend().buffer();
        let area = buffer.area();
        for y in area.top()..area.bottom() {
            let mut line = String::new();
            for x in area.left()..area.right() {
                line.push_str(buffer[(x, y)].symbol());
            }
            if line.contains(needle) {
                return y;
            }
        }
        panic!("`{needle}` is not on screen");
    }

    /// Draws a frame so the pane geometry the mouse arithmetic needs is real.
    fn frame(app: &mut App, width: u16, height: u16) {
        let _ = drawn(app, width, height);
    }

    /// One pull request summary, for tests that only need rows to exist.
    fn summary(number: u64) -> crate::domain::pr::PullRequestSummary {
        crate::domain::pr::PullRequestSummary {
            number,
            title: format!("PR {number}"),
            author: "alice".to_owned(),
            state: crate::domain::pr::PrState::Open,
            is_draft: false,
            base_ref: "main".to_owned(),
            head_ref: "topic".to_owned(),
            head_sha: "abc".to_owned(),
            created_at: crate::domain::time::Timestamp::default(),
            updated_at: crate::domain::time::Timestamp::default(),
            additions: 1,
            deletions: 1,
            changed_files: 1,
            labels: Vec::new(),
            review_decision: None,
            checks: crate::domain::pr::CheckSummary::default(),
            url: String::new(),
            is_cross_repository: false,
        }
    }

    fn click(column: u16, row: u16) -> event::MouseEvent {
        event::MouseEvent {
            kind: event::MouseEventKind::Down(event::MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn wheel(column: u16, row: u16, down: bool) -> event::MouseEvent {
        event::MouseEvent {
            kind: if down {
                event::MouseEventKind::ScrollDown
            } else {
                event::MouseEventKind::ScrollUp
            },
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn list_app() -> (TempHome, App) {
        let (dir, mut app) = app();
        app.set_pull_requests(crate::ports::forge::PullRequestPage::complete(
            (1..=5).map(summary).collect(),
            50,
        ));
        (dir, app)
    }

    #[test]
    fn a_click_on_a_list_row_selects_that_row() {
        let (_dir, mut app) = list_app();
        let terminal = drawn(&mut app, 100, 30);

        // The row is found in the *rendered frame*, not from the geometry constants
        // the click handler uses: a test that shares the implementation's arithmetic
        // passes with the implementation's mistakes (this one did, when the handler
        // was two rows out).
        let row = row_of(&terminal, "PR 3");
        app.on_mouse(click(10, row));
        assert_eq!(app.list.selected().unwrap().number, 3);

        let row = row_of(&terminal, "PR 5");
        app.on_mouse(click(10, row));
        assert_eq!(app.list.selected().unwrap().number, 5);

        // The filter bar is not a row: a click there must not move the cursor.
        app.on_mouse(click(10, app.filter_bar_rect.y));
        assert_eq!(app.list.selected().unwrap().number, 5);
    }

    #[test]
    fn a_click_lands_on_the_right_row_when_the_list_is_scrolled() {
        // More pull requests than fit even in a full-height pane, so the list has to
        // scroll for the test to mean anything.
        let (dir, mut app) = app();
        app.set_pull_requests(crate::ports::forge::PullRequestPage::complete(
            (1..=40).map(summary).collect(),
            50,
        ));
        let _ = &dir;
        press(&mut app, "G");
        let terminal = drawn(&mut app, 100, 24);

        let row = row_of(&terminal, "PR 40");
        assert!(
            app.list.scroll > 0,
            "the list scrolled to reach the last row"
        );

        app.on_mouse(click(10, row));
        assert_eq!(
            app.list.selected().unwrap().number,
            40,
            "a click maps through the scroll offset"
        );
    }

    #[test]
    fn the_wheel_scrolls_the_list_rather_than_only_moving_the_cursor() {
        // More rows than fit, so the wheel has something to scroll.
        let (dir, mut app) = app();
        app.set_pull_requests(crate::ports::forge::PullRequestPage::complete(
            (1..=40).map(summary).collect(),
            50,
        ));
        let _ = &dir;
        drawn(&mut app, 100, 24);
        assert_eq!(app.list.scroll, 0);

        app.on_mouse(wheel(10, 10, true));
        assert!(app.list.scroll > 0, "a wheel-down event moves the text");

        let scrolled = app.list.scroll;
        app.on_mouse(wheel(10, 10, false));
        assert!(
            app.list.scroll < scrolled,
            "and a wheel-up event moves it back"
        );
        assert_eq!(app.list.scroll, 0, "back to the top");
    }

    #[test]
    fn the_wheel_scrolls_the_diff_and_the_tree() {
        use crate::tui::diff_view::DiffView;

        let (_dir, mut app) = review_app();
        // A diff that is taller than the pane, so the wheel has somewhere to go.
        let patch = crate::domain::diff::parse_patch(include_str!(
            "../../tests/fixtures/gh/pr-diff-large.patch"
        ));
        app.set_review(DiffView::new(patch));
        frame(&mut app, 120, 30);
        assert_eq!(app.review.as_ref().unwrap().scroll, 0);

        app.on_mouse(wheel(60, app.review_top + 4, true));
        let after = app.review.as_ref().unwrap().scroll;
        assert!(after > 0, "the wheel moves the diff text");

        app.on_mouse(wheel(60, app.review_top + 4, false));
        assert!(
            app.review.as_ref().unwrap().scroll < after,
            "and moves it back"
        );
    }

    #[test]
    fn a_click_below_the_last_tree_row_does_nothing() {
        let (_dir, mut app) = review_app();
        frame(&mut app, 120, 30);
        let before = app.review.as_ref().unwrap().current_path().cloned();

        // Far below the two-line tree, inside the pane's rectangle.
        app.on_mouse(click(5, app.review_top + 12));
        assert_eq!(
            app.review.as_ref().unwrap().current_path().cloned(),
            before,
            "the empty space under the tree is not a row"
        );
    }

    /// A review with a directory, so the tree has a folder row to click.
    fn review_app() -> (TempHome, App) {
        use crate::tui::diff_view::DiffView;

        let (dir, mut app) = list_app();
        let patch = crate::domain::diff::parse_patch(
            "diff --git a/src/one.rs b/src/one.rs\n--- a/src/one.rs\n+++ b/src/one.rs\n@@ -1 +1 @@\n-a\n+b\n\
             diff --git a/src/two.rs b/src/two.rs\n--- a/src/two.rs\n+++ b/src/two.rs\n@@ -1 +1 @@\n-c\n+d\n",
        );
        app.set_review(DiffView::new(patch));
        (dir, app)
    }

    #[test]
    fn a_click_on_a_file_row_opens_that_file() {
        let (_dir, mut app) = review_app();
        let terminal = drawn(&mut app, 120, 30);

        // Row two of the tree is `src/two.rs`: the row is found on screen, so the test
        // does not depend on how the pane's rows are counted.
        let row = row_of(&terminal, "two.rs");
        app.on_mouse(click(5, row));
        assert_eq!(
            app.review
                .as_ref()
                .unwrap()
                .current_path()
                .unwrap()
                .as_str(),
            "src/two.rs"
        );
        assert!(app.review.as_ref().unwrap().current_file_label().is_some());
    }

    #[test]
    fn a_click_on_a_directory_row_folds_it_and_keeps_the_tree_focused() {
        let (_dir, mut app) = review_app();
        frame(&mut app, 120, 30);

        app.on_mouse(click(5, app.review_top + 1));
        let view = app.review.as_ref().unwrap();
        assert!(view.tree_focused, "the tree has the cursor");
        assert!(view.folded_dirs.contains("src"), "the folder folded");
        assert!(
            !view.tree.iter().any(|row| row.label == "one.rs"),
            "and its files are hidden"
        );
    }

    #[test]
    fn a_click_in_the_diff_moves_the_diff_cursor() {
        let (_dir, mut app) = review_app();
        frame(&mut app, 120, 30);

        app.on_mouse(click(80, app.review_top + 3));
        let view = app.review.as_ref().unwrap();
        assert!(!view.tree_focused);
        assert_eq!(view.cursor, 2, "the row under the pointer");
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
        let effect = press(&mut app, "<Space>T");
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
