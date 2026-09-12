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
use crate::application::analysis::AnalysisIntent;
use crate::application::models::CatalogState;
use crate::application::prs::FetchOutcome;
use crate::config::{Config, ConfigDocument, ModelSelection};
use crate::doctor::{Check, Context};
use crate::domain::context::BundlePolicy;
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
    /// Read whatever analysis is stored for the open pull request (FR-4.3).
    LoadAnalysis,
    /// Gather the context bundle, which is what an analysis would send (FR-4.6).
    GatherContext(AnalysisIntent),
    /// Ask the provider for an analysis (FR-4.1).
    RunAnalysis {
        /// Recompute even when the cache has a matching entry (`:analyze --force`).
        force: bool,
    },
    /// Give up on the run in flight, keeping the text that arrived (FR-4.4).
    CancelAnalysis,
    /// Store the review-plan overrides for this pull request (FR-4.2).
    SavePlan(Box<crate::domain::plan::Plan>),
    /// Read the chat sessions for the open pull request (FR-5.1).
    LoadChat,
    /// Start a new conversation, keeping the old ones (FR-5.1).
    NewChat,
    /// Open one of the stored conversations (FR-5.1).
    OpenChat(String),
    /// Ask a question (FR-5.2, FR-5.3).
    AskChat,
    /// Give up on the answer in flight, keeping the text that arrived (FR-5.2).
    CancelChat,
    /// Ask the last question again (FR-5.2).
    RetryChat,
    /// Write a transcript to a file (FR-5.1).
    ExportChat(String),
    /// Remove the conversations DEC-9's cap says are too old (FR-8.5).
    PruneChat,
    /// Store the files the user added to the context (FR-5.3).
    SaveContextFiles,
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
            Self::OpenChat(id) => f.debug_tuple("OpenChat").field(id).finish(),
            Self::ExportChat(format) => f.debug_tuple("ExportChat").field(format).finish(),
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
        Effect::LoadAnalysis => "load-analysis".to_owned(),
        Effect::GatherContext(intent) => format!("gather-context({intent:?})"),
        Effect::RunAnalysis { force } => format!("run-analysis(force={force})"),
        Effect::CancelAnalysis => "cancel-analysis".to_owned(),
        Effect::SavePlan(_) => "save-plan".to_owned(),
        Effect::LoadChat => "load-chat".to_owned(),
        Effect::NewChat => "new-chat".to_owned(),
        Effect::OpenChat(id) => format!("open-chat({id})"),
        Effect::AskChat => "ask-chat".to_owned(),
        Effect::CancelChat => "cancel-chat".to_owned(),
        Effect::RetryChat => "retry-chat".to_owned(),
        Effect::ExportChat(format) => format!("export-chat({format})"),
        Effect::PruneChat => "prune-chat".to_owned(),
        Effect::SaveContextFiles => "save-context-files".to_owned(),
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
    /// The chat pane (M3, FR-5.1).
    Chat,
}

impl Pane {
    /// The next pane, in the order `Tab` walks them.
    ///
    /// The order is the one the review screen shows left-to-right and top-to-bottom:
    /// the list, the diff, then the conversation about it. `Tab` from the chat pane
    /// goes back to the list rather than to the diff, because the list is where a
    /// session starts and a cycle that could not reach it would be a trap.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::PullRequests => Self::Diff,
            Self::Diff => Self::Chat,
            Self::Chat => Self::PullRequests,
        }
    }

    /// The previous pane, going backwards.
    #[must_use]
    pub const fn prev(self) -> Self {
        match self {
            Self::PullRequests => Self::Chat,
            Self::Diff => Self::PullRequests,
            Self::Chat => Self::Diff,
        }
    }

    /// Name used in the status line and in `state.toml`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PullRequests => "list",
            Self::Diff => "diff",
            Self::Chat => "chat",
        }
    }

    /// Parses the name written to `state.toml`.
    fn parse(name: &str) -> Self {
        match name {
            "diff" => Self::Diff,
            "chat" => Self::Chat,
            _ => Self::PullRequests,
        }
    }
}

/// Where the panes were on the last frame (FR-7.5).
///
/// One struct rather than seven fields, because they are written together from one
/// layout and read together by one mouse handler: two of them disagreeing is exactly how
/// a click comes to land on the row above the one it was aimed at.
#[derive(Debug, Clone, Copy, Default)]
struct Geometry {
    /// The width of the last frame, which the fit checks read.
    width: u16,
    /// The filter bar (FR-7.5).
    filter_bar: ratatui::layout::Rect,
    /// The list pane.
    list: ratatui::layout::Rect,
    /// The whole body, for the bounds a click has to fall inside.
    body: ratatui::layout::Rect,
    /// The first row of the review panes, below the tab bar.
    review_top: u16,
    /// The width of the file tree, which separates the two review panes.
    tree_width: u16,
    /// The chat pane, when it is open (FR-5.1).
    chat: Option<ratatui::layout::Rect>,
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
    /// The analysis panel: the summary, the intent, the risks and the plan (FR-4.1).
    Analysis,
    /// The context inspector: what would be sent and what was left out (FR-4.6).
    Context,
    /// The model's own text, when it could not be used as an analysis (FR-4.1).
    RawAnswer,
}

/// Everything the analysis panel owns (FR-4.1, FR-4.3, FR-4.4, FR-4.6).
///
/// Grouped because it changes together: a run replaces the analysis, its plan, its
/// warnings and its stream in one step, and a struct is what makes that one step
/// visible instead of thirteen assignments that must be kept in order.
#[derive(Debug, Default)]
pub struct PanelState {
    /// The analysis for the open pull request.
    pub analysis: Option<Box<crate::ports::StoredAnalysis>>,
    /// An analysis for the same pull request at an older commit (DEC-15).
    pub stale: Option<Box<crate::ports::StoredAnalysis>>,
    /// What the analysis is doing right now.
    pub state: AnalysisState,
    /// The gathered context bundle (FR-4.6).
    pub bundle: Option<Box<crate::domain::context::Bundle>>,
    /// Which pull request and commit the bundle was gathered for.
    pub bundle_for: Option<(u64, String)>,
    /// The review plan in force (FR-4.2).
    pub plan: Option<crate::domain::plan::Plan>,
    /// The job id of the run (FR-4.4).
    pub job: u64,
    /// The job id of the context gather (FR-4.6).
    pub context_job: u64,
    /// The job id of the stored-analysis read (FR-4.3).
    pub stored_job: u64,
    /// The text that has streamed in (FR-4.4).
    pub stream: StreamBuffer,
    /// What normalization corrected (FR-4.1).
    pub warnings: Vec<String>,
    /// The model's unusable text, with the reason (FR-4.1).
    pub raw: Option<(String, String)>,
    /// Whether the user has confirmed the first send for this repository (FR-4.6).
    pub confirmed: bool,
}

/// Everything the analysis panel owns (FR-4.1, FR-4.3, FR-4.4, FR-4.6).
///
/// What the analysis is doing (FR-4.4).
///
/// The states are the interface's, not the use case's: what a user needs to know is
/// whether an answer is expected, whether one is arriving, and whether the last one
/// was usable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AnalysisState {
    /// Nothing requested.
    #[default]
    Idle,
    /// Gathering the context, which is the local half.
    Gathering,
    /// Gathered, and waiting for the user to agree to send it (FR-4.6).
    Confirming,
    /// Waiting for the provider, with the stage it is on.
    Running {
        /// What the job is doing: asking, or repairing.
        stage: String,
    },
    /// Answering, with text arriving.
    Streaming {
        /// What the job is doing: asking, or repairing.
        stage: String,
    },
    /// An analysis is available.
    Ready,
    /// The answer could not be used (FR-4.1).
    Unusable {
        /// Why, in the model's words where possible.
        reason: String,
    },
    /// The user stopped it (FR-4.4).
    Cancelled,
}

impl AnalysisState {
    /// Whether work is in flight, which is what `Esc` cancels.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(
            self,
            Self::Gathering | Self::Running { .. } | Self::Streaming { .. }
        )
    }

    /// Whether the user is being asked to confirm a send (FR-4.6).
    #[must_use]
    pub fn is_confirming(&self) -> bool {
        matches!(self, Self::Confirming)
    }

    /// The one-line description for the status area.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Idle => "no analysis".to_owned(),
            Self::Gathering => "gathering the context".to_owned(),
            Self::Confirming => "ready to send; confirm with <leader>a".to_owned(),
            Self::Running { stage } | Self::Streaming { stage } => stage.clone(),
            Self::Ready => "analysed".to_owned(),
            Self::Unusable { reason } => format!("unusable answer: {reason}"),
            Self::Cancelled => "cancelled".to_owned(),
        }
    }
}

/// Somewhere to keep text that arrives in pieces.
///
/// Bounded: what the interface shows while an answer streams is a preview, and a
/// provider that streams a megabyte should not be able to make the interface hold it
/// twice (the completion carries the whole thing anyway).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamBuffer {
    text: String,
    truncated: bool,
}

/// The most streamed text the preview keeps.
pub const MAX_STREAM_BYTES: usize = 64 * 1024;

impl StreamBuffer {
    /// Adds a piece, keeping the total bounded.
    pub fn push(&mut self, delta: &str) {
        if self.text.len() >= MAX_STREAM_BYTES {
            self.truncated = true;
            return;
        }
        let room = MAX_STREAM_BYTES.saturating_sub(self.text.len());
        if delta.len() <= room {
            self.text.push_str(delta);
            return;
        }
        let mut end = room;
        while end > 0 && !delta.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&delta[..end]);
        self.truncated = true;
    }

    /// The text so far.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the preview was cut short.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Forgets everything, for a new run.
    pub fn clear(&mut self) {
        self.text.clear();
        self.truncated = false;
    }
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
    /// Whether the automatic fetch for a configured model has already been tried.
    pub(crate) catalog_auto_fetched: bool,
    /// The diff flags the review screen is using (FR-3.2).
    pub(crate) diff_options: DiffOptions,
    /// Where the diff on screen was read from (FR-3.2).
    pub(crate) diff_source: DiffSource,
    /// The credential store, for reading and writing provider keys (FR-4.5).
    pub(crate) secret_store: std::sync::Arc<dyn SecretStore>,
    /// The workspace port, for listing managed worktrees (FR-3.1).
    pub(crate) workspace_port: std::sync::Arc<dyn crate::ports::WorkspacePort>,
    /// Where analyses and their review-plan overrides are kept (FR-4.3).
    pub(crate) analysis_cache: std::sync::Arc<dyn crate::ports::AnalysisCachePort>,
    /// The open pull request's worktree, once it exists (FR-3.1).
    pub(crate) workspace: Option<Workspace>,
    /// The job id of the worktree being built (FR-3.1).
    pub(crate) workspace_job: u64,
    /// Why the shown diff came from the cache, when it did (DEC-14).
    pub(crate) diff_offline: Option<String>,
    /// The analysis panel's state (FR-4.1, FR-4.3, FR-4.4, FR-4.6).
    pub(crate) panel: PanelState,
    /// The chat pane's state (FR-5.1).
    pub(crate) chat: crate::tui::chat::ChatState,
    /// The bundle a question would send, once it has been gathered (FR-4.6).
    chat_bundle: Option<Box<crate::domain::context::Bundle>>,
    /// Which pull request and commit the bundle was gathered for (FR-4.6).
    chat_bundle_for: Option<(u64, String)>,
    /// Where chat sessions are kept (FR-5.1).
    pub(crate) chat_store: std::sync::Arc<dyn crate::ports::ChatStorePort>,
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
    /// Where the panes were on the last frame (FR-7.5).
    geometry: Geometry,
    /// Set when the user asks to quit.
    pub(crate) quit: bool,
    /// Set when `state.toml` no longer describes the app.
    ///
    /// The reducer cannot write it (no IO in the reducer), so it says *that* the file is
    /// stale and the loop writes it. Without this the one-time opt-in of FR-4.6 lived
    /// only as long as the process, and every restart asked the same question again.
    state_dirty: bool,
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
            analysis,
            chat: chat_store,
            ..
        } = startup;

        let (theme_names, list, focus) = initial_view(&home, &config, &state);

        let mut app = Self {
            secret_store,
            workspace_port,
            analysis_cache: analysis,
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
            catalog_auto_fetched: false,
            diff_options: DiffOptions::default(),
            diff_source: DiffSource::Forge,
            workspace: None,
            workspace_job: 0,
            diff_offline: None,
            panel: PanelState::default(),
            chat: crate::tui::chat::ChatState::default(),
            chat_bundle: None,
            chat_bundle_for: None,
            chat_store,
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
            geometry: Geometry::default(),
            quit: false,
            state_dirty: false,
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
    ///
    /// The focus is only taken when this *opens* the screen. Every patch arrival goes
    /// through here — the forge's diff, then the worktree's, then again after a context
    /// change — and a late arrival that re-focused the diff pane would pull the keyboard
    /// out of whatever the user had moved on to. With two panes that was invisible;
    /// with a compose box it silently turned a typed question into key bindings.
    pub fn open_review(&mut self, detail: PullRequestDetail, view: DiffView) {
        let opening = self.review.is_none();
        self.detail = Some(detail);
        self.review = Some(view);
        if opening {
            self.focus = Pane::Diff;
            self.sync_mode_to_focus();
        }
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
            // The analysis group has its own handler: three outcomes that share the
            // panel's state, and a match with twenty arms is one where the interesting
            // ones hide.
            Outcome::Stored { .. } | Outcome::Context { .. } | Outcome::Analyzed(_)
                if job == self.panel.stored_job
                    || job == self.panel.context_job
                    || job == self.panel.job =>
            {
                self.apply_analysis_outcome(job, outcome)
            }
            // The chat group is its own handler for the same reason: a loaded list, a
            // gather and an answer all belong to the pane's state.
            Outcome::ChatLoaded { .. }
            | Outcome::ChatAnswered(_)
            | Outcome::ChatGathered { .. }
                if job == self.chat.load_job || job == self.chat.job =>
            {
                self.apply_chat_outcome(job, outcome)
            }
            Outcome::Checks(checks) => {
                self.apply_checks(job, checks);
                None
            }
            // A failure is gated like any other result: a superseded request's error
            // must not be announced as if it were the newest one.
            Outcome::Failed(message) if self.is_current_job(job) => {
                self.apply_failure(job, &message)
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
            | Outcome::Stored { .. }
            | Outcome::Context { .. }
            | Outcome::Analyzed(_)
            | Outcome::ChatLoaded { .. }
            | Outcome::ChatAnswered(_)
            | Outcome::ChatGathered { .. }
            | Outcome::Failed(_)
            | Outcome::Abandoned => None,
        }
    }

    /// The files the user has added to the context for this pull request (FR-5.3).
    ///
    /// Kept in `state.toml` rather than beside the session: "I want the design document
    /// in the bundle" is a preference about the pull request, not a property of one
    /// conversation, and it should hold for the next one too.
    pub(crate) fn context_files_key(&self) -> Option<String> {
        let environment = self.environment.as_ref()?;
        let pr = self.detail.as_ref().map(|detail| detail.summary.number)?;
        Some(format!("{}#{pr}", environment.repo.key()))
    }

    /// Loads the added files for the open pull request.
    pub(crate) fn load_context_files(&mut self) {
        let Some(key) = self.context_files_key() else {
            self.chat.added.clear();
            return;
        };
        self.chat.added = self
            .state
            .context_files
            .get(&key)
            .cloned()
            .unwrap_or_default();
    }

    /// Records the added files, so the next run starts with them (FR-8.5).
    pub(crate) fn remember_context_files(&mut self) {
        let Some(key) = self.context_files_key() else {
            return;
        };
        if self.chat.added.is_empty() {
            self.state.context_files.remove(&key);
        } else {
            self.state
                .context_files
                .insert(key, self.chat.added.clone());
        }
    }

    /// Adds a path to the context, or says why it cannot be (FR-5.3).
    pub(crate) fn add_context_file(&mut self, path: &str) -> std::result::Result<String, String> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err("which file? `:context add src/domain/money.rs`".to_owned());
        }
        if crate::domain::context::is_secret_path(trimmed) {
            // Refused rather than added and then elided: the user asked for this file
            // by name, and a bundle that silently leaves it out is the one thing
            // FR-4.6 exists to prevent.
            return Err(format!(
                "{trimmed} looks like a secret (a `.env` file or a credential) and is \
                 never sent"
            ));
        }
        if !self.path_in_diff(trimmed) && !self.path_exists_at_head(trimmed) {
            return Err(format!(
                "{trimmed} is not in this pull request's changed files and not in the \
                 checkout at this commit"
            ));
        }
        if !self.chat.added.iter().any(|added| added == trimmed) {
            self.chat.added.push(trimmed.to_owned());
        }
        Ok(format!(
            "added {trimmed} to the context of this pull request ({} file(s) in total; \
             every question will include it)",
            self.chat.added.len()
        ))
    }

    /// Removes a path from the context (FR-5.3).
    pub(crate) fn remove_context_file(
        &mut self,
        path: &str,
    ) -> std::result::Result<String, String> {
        let before = self.chat.added.len();
        self.chat.added.retain(|added| added != path.trim());
        if self.chat.added.len() == before {
            return Err(format!("{path} was not in the context"));
        }
        Ok(format!("removed {path} from the context"))
    }

    /// Whether the checkout at head has a path, which is what `:context add` checks
    /// against (FR-5.3).
    fn path_exists_at_head(&self, path: &str) -> bool {
        let (Some(workspace), true) = (&self.workspace, self.workspace_ready()) else {
            return false;
        };
        self.workspace_port
            .read_file(
                &workspace.path,
                &workspace.head_sha,
                path,
                &crate::ports::Cancel::new(),
            )
            .is_ok()
    }

    /// The request a question would send (FR-5.2, FR-5.3).
    ///
    /// Mirrors [`App::analysis_request`] deliberately: the two ask the same model the
    /// same way, and the only difference is what they ask about.
    pub(crate) fn chat_request(
        &self,
    ) -> Option<(
        crate::application::chat::ChatSpec,
        crate::domain::chat::Session,
    )> {
        let detail = self.detail.as_ref()?;
        let resolved = self.active_model.as_ref()?;
        let secret = self
            .secret_store
            .get(&resolved.provider, resolved.env_var.as_deref())
            .ok()
            .flatten()?;
        let model = self.catalog.as_ref().and_then(|state| {
            state
                .load
                .catalog
                .model(&resolved.provider, &resolved.model)
        });
        let output_limit = model.and_then(crate::domain::model::CatalogModel::output_limit);
        let mut chat = crate::application::models::analysis_chat(resolved, secret, output_limit);
        // A chat answer is read as it arrives and is usually shorter than an analysis of
        // the same pull request, so the long analysis timeout would only hold a dead
        // connection open.
        chat.timeout_secs = CHAT_TIMEOUT_SECS;
        let policy = crate::domain::context::BundlePolicy {
            max_context_tokens: crate::application::models::context_budget(
                model.and_then(crate::domain::model::CatalogModel::context_limit),
                self.config.llm.max_context_tokens,
                chat.max_tokens,
            ),
            max_file_bytes: self.config.llm.max_file_bytes,
            ..crate::domain::context::BundlePolicy::default()
        };
        let spec = crate::application::chat::ChatSpec {
            repo: self
                .environment
                .as_ref()
                .map(|environment| environment.repo.clone())?,
            pr: detail.summary.number,
            head_sha: detail.summary.head_sha.clone(),
            chat,
            detail: Box::new(detail.clone()),
            patch: self
                .review
                .as_ref()
                .map(|view| Box::new(view.patch.clone())),
            checkout: self.checkout(),
            policy,
            added: self.chat.added.clone(),
            cost: model.and_then(|model| model.cost.clone()),
        };
        let session = self.chat.session.clone().unwrap_or_else(|| {
            crate::application::chat::new_session(
                crate::domain::chat::session_id(
                    self.now_unix_secs,
                    self.chat.sessions.len() as u64,
                ),
                &spec.repo,
                spec.pr,
                &spec.head_sha,
                &format!("{}/{}", spec.chat.provider, spec.chat.model),
                self.config
                    .llm
                    .active
                    .as_ref()
                    .and_then(|selection| selection.reasoning.clone()),
                self.now_unix_secs,
            )
        });
        Some((spec, session))
    }

    /// Shows the chat pane, loading the conversation if it is not there yet.
    pub(crate) fn show_chat(&mut self) {
        self.chat.open();
        self.load_context_files();
    }

    /// Shows the conversation list, without asking the store again: the list is what
    /// the last load returned, and `:chat list` is not a refresh (FR-5.1).
    pub(crate) fn list_chats(&mut self) {
        self.chat.open = true;
        self.chat.listing = true;
    }

    /// Records the job id of a chat load (FR-5.1).
    pub fn record_chat_load(&mut self, job: u64) {
        self.chat.load_job = job;
    }

    /// Records the job id of the question in flight (FR-5.2).
    pub fn record_chat_job(&mut self, job: u64) {
        self.chat.job = job;
    }

    /// Opens a fresh conversation, keeping the old ones (FR-5.1).
    pub(crate) fn begin_chat(&mut self) {
        self.chat.open();
        self.chat.reset();
        self.load_context_files();
        // The bundle is gathered per question, and a new conversation is a new subject
        // to estimate: keeping the old estimate would show the price of a different
        // question.
        self.chat_bundle = None;
        self.chat_bundle_for = None;
    }

    /// Notes that the user is being asked before the first send (FR-4.6).
    pub(crate) fn await_chat_confirmation(&mut self) {
        self.chat.open = true;
        let question = self.chat.input.text().trim().to_owned();
        self.chat.awaiting_confirmation = Some(question);
    }

    /// Starts the answer to a question (FR-5.1, FR-5.2).
    ///
    /// The question joins the conversation *now*, before the provider answers, and the
    /// session is stored here rather than when the answer arrives. Both matter for the
    /// same reason: the answer has to land somewhere, and a conversation whose first
    /// turn only exists once its answer does loses the question if the answer never
    /// comes. It also means the user sees what they asked while the model is thinking,
    /// which is what every chat interface does and what makes a slow answer tolerable.
    pub(crate) fn begin_chat_answer(
        &mut self,
        question: &str,
        session: crate::domain::chat::Session,
    ) {
        self.chat.status = crate::tui::chat::ChatStatus::Sending {
            stage: format!("asking {}", self.model_label()),
        };
        self.chat.pending = Some(question.to_owned());
        self.chat.awaiting_confirmation = None;
        self.chat.stream.clear();
        self.chat.scroll = 0;
        self.chat_bundle = None;
        self.chat_bundle_for = None;
        self.chat.input.clear();

        let mut session = session;
        session.messages.push(crate::domain::chat::Message::user(
            question,
            self.now_unix_secs,
        ));
        session.updated_at = self.now_unix_secs;
        self.chat.session = Some(session);
        self.persist_chat();
    }

    /// Gives up on the answer in flight, keeping what arrived (FR-5.2).
    ///
    /// The job id is deliberately *not* cleared: the worker still finishes — with the
    /// text that arrived, marked as stopped — and that completion has to be matched to
    /// be stored. Clearing the id here is what made the partial answer visible while it
    /// was streaming and gone the moment the app restarted.
    pub(crate) fn stop_chat(&mut self) {
        self.chat.stop();
    }

    /// Takes the gathered bundle when it is for the commit on screen (FR-4.6).
    pub(crate) fn take_chat_bundle(&mut self) -> Option<crate::domain::context::Bundle> {
        let head = self.detail.as_ref()?.summary.head_sha.clone();
        let pr = self.detail.as_ref()?.summary.number;
        if self.chat_bundle_for.as_ref() != Some(&(pr, head)) {
            return None;
        }
        self.chat_bundle.take().map(|bundle| *bundle)
    }

    /// Writes a transcript (FR-5.1).
    ///
    /// # Errors
    ///
    /// Returns the filesystem's message when the file cannot be written.
    pub(crate) fn export_chat(&self, format: &str) -> std::result::Result<String, String> {
        let session = self
            .chat
            .session
            .as_ref()
            .ok_or_else(|| "there is no conversation to export yet".to_owned())?;
        let (extension, body) = match format.trim() {
            "" | "md" | "markdown" => ("md", crate::domain::chat::to_markdown(session)),
            "json" => ("json", crate::domain::chat::to_json(session)?),
            other => {
                return Err(format!("{other} is not a format; use md or json"));
            }
        };
        let name = format!(
            "chat-{}-{}.{extension}",
            session.pr,
            crate::domain::chat::short_sha(&session.id)
        );
        let path = self.home.exports().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        crate::adapters::fs::write_atomic(&path, &body).map_err(|error| error.to_string())?;
        Ok(path.display().to_string())
    }

    /// Re-reads the conversation list, after a write or a prune (FR-5.1).
    pub(crate) fn reload_chat_list(&mut self) {
        let (Some(repo), Some(pr)) = (
            self.environment
                .as_ref()
                .map(|environment| environment.repo.clone()),
            self.detail.as_ref().map(|detail| detail.summary.number),
        ) else {
            return;
        };
        self.chat.sessions = self.chat_store.list(&repo, pr).unwrap_or_default();
    }

    /// Keeps a gathered bundle, and asks for the answer when the user has already
    /// agreed to send it (FR-4.6, FR-5.3).
    ///
    /// Returns the effect that turns a gathered bundle into a question, which is how a
    /// bundle becomes a request without the reducer holding either one.
    fn apply_chat_gathered(&mut self, outcome: jobs::Outcome) -> Option<Effect> {
        let jobs::Outcome::ChatGathered {
            bundle,
            session,
            question,
        } = outcome
        else {
            return None;
        };
        let (pr, head) = self
            .detail
            .as_ref()
            .map(|detail| (detail.summary.number, detail.summary.head_sha.clone()))?;
        self.chat_bundle_for = Some((pr, head));
        self.chat_bundle = Some(bundle);

        // The confirmation is where the estimate is shown, because it is the only place
        // the size of the request is known before something is paid for (FR-4.6).
        if self.chat.is_confirming() {
            self.show_chat_estimate(&session);
            return None;
        }
        // Already agreed: the question goes now, with the bundle that was just gathered.
        self.chat.staged = Some(question);
        Some(Effect::AskChat)
    }

    /// Shows what a question would send, and asks for the key press that agrees to it
    /// (FR-4.6).
    fn show_chat_estimate(&mut self, session: &crate::domain::chat::Session) {
        let Some((spec, _)) = self.chat_request() else {
            return;
        };
        let Some(bundle) = self.chat_bundle.as_deref() else {
            return;
        };
        let estimate = crate::application::chat::estimate_of(&spec, session, bundle);
        let summary = bundle.summary();
        self.chat.set_estimate(estimate);
        self.chat.open = true;
        self.notice(
            NoticeLevel::Info,
            format!(
                "this sends {summary} of this pull request to {}; press Enter again to send",
                self.model_label()
            ),
        );
    }

    /// A job that failed, with the one follow-up a failure can have (FR-9.1).
    ///
    /// The decision to fetch a missing catalog is made *before* the failure is reported:
    /// reporting clears the job id it would be checked against, and fetching is the more
    /// useful thing to do than announcing that there was nothing to read.
    fn apply_failure(&mut self, job: u64, message: &str) -> Option<Effect> {
        if let Some(effect) = self.catalog_unavailable(job) {
            self.catalog_job = 0;
            return Some(effect);
        }
        self.report_job_failure(job, message);
        None
    }

    /// The outcomes that belong to the chat pane (FR-5.1–FR-5.3).
    fn apply_chat_outcome(&mut self, job: u64, outcome: jobs::Outcome) -> Option<Effect> {
        match outcome {
            Outcome::ChatLoaded { .. } if job == self.chat.load_job => {
                self.apply_chat_loaded(outcome);
                None
            }
            Outcome::ChatAnswered(_) if job == self.chat.job => {
                self.apply_chat_answer(outcome);
                None
            }
            Outcome::ChatGathered { .. } if job == self.chat.load_job => {
                self.apply_chat_gathered(outcome)
            }
            _ => None,
        }
    }

    /// Applies the sessions a load produced, opening the conversation it named
    /// (FR-5.1).
    fn apply_chat_loaded(&mut self, outcome: jobs::Outcome) {
        let jobs::Outcome::ChatLoaded {
            sessions,
            session,
            missing,
        } = outcome
        else {
            return;
        };
        self.chat.sessions = sessions;
        if let Some(session) = session {
            self.chat.session = Some(*session);
            // The estimate belongs to the conversation that was open when it was made.
            self.chat.estimate = None;
        }
        if let Some(id) = missing {
            self.notice(
                NoticeLevel::Warn,
                format!("there is no chat session {id} for this pull request"),
            );
        }
    }

    /// Appends an answer to the conversation it belongs to (FR-5.1, FR-5.2).
    fn apply_chat_answer(&mut self, outcome: jobs::Outcome) {
        let jobs::Outcome::ChatAnswered(answered) = outcome else {
            return;
        };
        let crate::tui::jobs::ChatAnswered { run, session, .. } = *answered;
        self.chat.job = 0;
        self.chat.pending = None;

        // A superseded answer is dropped, which is what makes `Esc` and a second
        // question safe to press in either order.
        let Some(current) = self.chat.session.as_mut() else {
            return;
        };
        if current.id != session {
            return;
        }
        match run {
            crate::application::chat::ChatRun::Answered(answer) => {
                current.messages.push(answer.message);
                current.updated_at = self.now_unix_secs;
                self.chat.status = crate::tui::chat::ChatStatus::Idle;
                self.record_analysis_opt_in();
                // The store may refuse (DEC-9's size cap) or fail. Either way the
                // conversation on screen is complete: the user's words are in memory and
                // the message says what could not be written.
                self.persist_chat();
            }
            crate::application::chat::ChatRun::Cancelled(message) => {
                // FR-5.2: what arrived stays visible, marked as stopped. It is stored,
                // so restarting the app does not hide the fact that the user stopped it.
                current.messages.push(*message);
                current.updated_at = self.now_unix_secs;
                self.chat.status = crate::tui::chat::ChatStatus::Stopped;
                self.persist_chat();
            }
        }
        // The input was cleared when the question was taken, not here: a user who types
        // the next question while the answer is arriving must not lose it to the answer
        // landing.
        self.chat.stream.clear();
        self.chat.scroll = 0;
    }

    /// Writes the open conversation to the store (FR-5.1).
    ///
    /// A failure here is reported rather than swallowed, and it does not lose the
    /// conversation: it is in memory, and the notice says what could not be written.
    pub(crate) fn persist_chat(&mut self) {
        let Some(session) = self.chat.session.clone() else {
            return;
        };
        if let Err(error) = self.chat_store.put(&session) {
            self.notice(
                NoticeLevel::Warn,
                format!("the conversation could not be saved: {error}"),
            );
        }
        // The repository comes from the session itself, so a conversation cannot be
        // listed under a pull request it does not belong to.
        if let Some(repo) = self
            .environment
            .as_ref()
            .map(|environment| environment.repo.clone())
        {
            self.chat.sessions = self.chat_store.list(&repo, session.pr).unwrap_or_default();
        }
    }

    /// The outcomes that belong to the analysis panel (FR-4.1, FR-4.3, FR-4.6).
    ///
    /// Returns what to do next, which is how a gathered bundle becomes the request it
    /// was gathered for.
    fn apply_analysis_outcome(&mut self, job: u64, outcome: jobs::Outcome) -> Option<Effect> {
        match outcome {
            Outcome::Stored {
                current,
                stale,
                plan,
            } if job == self.panel.stored_job => {
                self.panel.stored_job = 0;
                self.apply_stored(
                    current.map(|stored| *stored),
                    stale.map(|stored| *stored),
                    plan.map(|plan| *plan),
                );
                None
            }
            Outcome::Context { bundle, intent } if job == self.panel.context_job => {
                self.panel.context_job = 0;
                self.apply_context(*bundle, intent)
            }
            Outcome::Analyzed(run) if job == self.panel.job => {
                self.panel.job = 0;
                self.apply_analysis(*run);
                None
            }
            // Unreachable: `apply_completion` routes only these three here, gated on
            // their own job ids.
            _ => None,
        }
    }

    /// Fetches the catalog when a configured model needs it and the cache was empty.
    ///
    /// The startup load is cache-only so that a fresh run makes no network call
    /// (FR-4.7). That is the right default, but it left a user who had already chosen a
    /// model being told there was no model until they opened the picker — a fetch they
    /// had not asked for is worth less than a working model they did ask for.
    ///
    /// Once only: an unreachable catalog must not become a fetch loop, and the second
    /// failure is a real failure worth reporting.
    #[must_use]
    pub fn catalog_unavailable(&mut self, job: u64) -> Option<Effect> {
        if job != self.catalog_job
            || self.catalog.is_some()
            || self.catalog_auto_fetched
            || self.config.llm.active.is_none()
        {
            // No model configured means the picker is where a model is chosen, and it
            // fetches the catalog when it opens.
            return None;
        }
        self.catalog_auto_fetched = true;
        logging::log(
            Level::Debug,
            "no cached catalog and a model is configured: fetching it now",
        );
        Some(Effect::LoadCatalog(
            crate::ports::catalog::CatalogPolicy::Refresh,
        ))
    }

    /// Applies what the analysis cache held (FR-4.3, DEC-15).
    ///
    /// A stored analysis for the *current* head is used without asking; one for an
    /// older head is not presented as current but is offered, because the alternative
    /// — throwing it away — spends money to recompute something the user may still
    /// want to read (DEC-15).
    pub fn apply_stored(
        &mut self,
        current: Option<crate::ports::StoredAnalysis>,
        stale: Option<crate::ports::StoredAnalysis>,
        plan: Option<crate::domain::plan::Plan>,
    ) {
        self.panel.stale = stale.map(Box::new);
        if let Some(stored) = current {
            self.adopt_analysis(stored);
        } else if let Some(stale) = &self.panel.stale {
            let age = crate::domain::time::relative(
                crate::domain::time::from_unix_secs(
                    i64::try_from(self.now_unix_secs).unwrap_or(i64::MAX),
                ),
                crate::domain::time::from_unix_secs(
                    i64::try_from(stale.stored_at).unwrap_or(i64::MAX),
                ),
            );
            self.panel.analysis = None;
            self.panel.state = AnalysisState::Idle;
            self.notice(
                NoticeLevel::Info,
                format!(
                    "an analysis from {age} ago covers an older commit ({}); <leader>a analyses \
                     the current one",
                    stale.key.head_sha.get(..8).unwrap_or(&stale.key.head_sha)
                ),
            );
        }
        // The stored overrides win only when they describe the same commit the
        // analysis does; otherwise the analysis's own order is used (FR-4.2).
        if let Some(plan) = plan {
            let head = self.analysis_head();
            let usable = head.as_deref().is_some_and(|head| plan.matches_head(head));
            if usable {
                self.panel.plan = Some(plan);
                self.apply_plan_to_review();
            } else if plan.overridden {
                self.notice(
                    NoticeLevel::Info,
                    "the saved review order was made against an older commit and was not used"
                        .to_owned(),
                );
            }
        }
    }

    /// Applies a gathered context bundle (FR-4.6).
    pub fn apply_context(
        &mut self,
        bundle: crate::domain::context::Bundle,
        intent: AnalysisIntent,
    ) -> Option<Effect> {
        self.panel.bundle_for = self
            .detail
            .as_ref()
            .map(|detail| (detail.summary.number, detail.summary.head_sha.clone()));
        // The gather is over. Whatever happens next (a run, the inspector, or the user
        // thinking about it) starts from "nothing is in flight", which is what the
        // second key press and the status line both read.
        self.panel.state = AnalysisState::Idle;
        let bundle = Box::new(bundle);
        match intent {
            AnalysisIntent::Inspect => {
                self.panel.bundle = Some(bundle);
                self.open_overlay(Overlay::Context);
                None
            }
            AnalysisIntent::Run => {
                self.panel.bundle = Some(bundle);
                Some(Effect::RunAnalysis { force: false })
            }
            AnalysisIntent::Estimate => {
                let summary = bundle.summary();
                self.panel.bundle = Some(bundle);
                // The opt-in notice is per repository and shown once (FR-4.6): after
                // it, analysis is one key press.
                if self.analysis_opt_in_recorded() {
                    return Some(Effect::RunAnalysis { force: false });
                }
                self.panel.confirmed = true;
                // The panel as well as the notice: a notice expires after a few
                // seconds, and a question that disappears before it is answered is not
                // a question. This is also where the size estimate stays readable.
                self.panel.state = AnalysisState::Confirming;
                self.notice(
                    NoticeLevel::Info,
                    format!(
                        "this sends {summary} of this pull request to {}; press <leader>a again \
                         to confirm",
                        self.model_label()
                    ),
                );
                self.open_overlay(Overlay::Analysis);
                None
            }
        }
    }

    /// Applies a finished run, whichever way it went (FR-4.1, FR-4.4).
    pub fn apply_analysis(&mut self, run: crate::application::analysis::AnalysisRun) {
        use crate::application::analysis::AnalysisRun;
        match run {
            AnalysisRun::Ready(ready) => {
                let stored = crate::ports::StoredAnalysis {
                    key: self
                        .analysis_key()
                        .unwrap_or_else(|| crate::ports::AnalysisKey {
                            repo: String::new(),
                            pr: self.detail.as_ref().map_or(0, |d| d.summary.number),
                            head_sha: String::new(),
                            provider: String::new(),
                            model: String::new(),
                            thinking: None,
                            prompt_version: crate::domain::analysis::PROMPT_VERSION,
                        }),
                    analysis: (*ready.analysis).clone(),
                    raw: self.panel.stream.text().to_owned(),
                    // The document is already in the cache (the use case stored it);
                    // this value is what the panel shows, carrying the same
                    // corrections and the same repair flag the cache holds.
                    warnings: ready.warnings.clone(),
                    repaired: ready.repaired,
                    stored_at: self.now_unix_secs,
                };
                self.adopt_analysis(stored);
                let usage = ready
                    .usage
                    .map(|usage| {
                        let reasoning = usage
                            .reasoning
                            .map(|tokens| format!(", {tokens} reasoning"))
                            .unwrap_or_default();
                        format!(" · {} in/{}{reasoning} out", usage.prompt, usage.completion)
                    })
                    .unwrap_or_default();
                self.notice(
                    NoticeLevel::Info,
                    format!(
                        "analysed #{} with {}{usage}{}",
                        self.detail
                            .as_ref()
                            .map_or(0, |detail| detail.summary.number),
                        self.model_label(),
                        if ready.repaired {
                            " (after a retry)"
                        } else {
                            ""
                        }
                    ),
                );
            }
            AnalysisRun::Unparsed(unparsed) => {
                // Kept rather than discarded: the text is the only explanation of what
                // the model did instead of answering (FR-4.1).
                self.panel.raw = Some((unparsed.reason.clone(), unparsed.raw.clone()));
                self.panel.state = AnalysisState::Unusable {
                    reason: unparsed.reason.clone(),
                };
                self.notice(
                    NoticeLevel::Warn,
                    format!(
                        "the answer could not be used: {} · `:analysis raw` shows it",
                        unparsed.reason
                    ),
                );
                self.open_overlay(Overlay::RawAnswer);
            }
            AnalysisRun::Cancelled => {
                self.panel.state = AnalysisState::Cancelled;
                self.notice(
                    NoticeLevel::Info,
                    "the analysis was cancelled; any text it had produced is kept".to_owned(),
                );
            }
        }
    }

    /// Takes a document as the current analysis and re-orders the review (FR-4.2).
    fn adopt_analysis(&mut self, stored: crate::ports::StoredAnalysis) {
        // The plan is derived here rather than in the view, so the panel and the tree
        // can never disagree about what the analysis said (FR-4.2).
        let derived = crate::domain::plan::Plan::from_analysis(&stored.analysis);
        // A user who ordered the files by hand keeps that order when the same analysis
        // is read again; a plan for a different commit is replaced rather than applied
        // to files it no longer describes (FR-4.2).
        let keep = self.panel.plan.as_ref().is_some_and(|existing| {
            existing.overridden && existing.matches_head(&stored.analysis.head_sha)
        });
        let repaired = stored.repaired;
        self.panel.warnings.clone_from(&stored.warnings);
        self.panel.stale = None;
        self.panel.raw = None;
        self.panel.stream.clear();
        self.panel.state = AnalysisState::Ready;
        self.panel.analysis = Some(Box::new(stored));
        if !keep {
            self.panel.plan = Some(derived);
        }
        self.apply_plan_to_review();
        if repaired {
            self.notice(
                NoticeLevel::Info,
                "the first answer was not usable JSON; it was asked again".to_owned(),
            );
        }
    }

    /// Pushes the plan, or its absence, into the review view (FR-3.5).
    pub(crate) fn apply_plan_to_review(&mut self) {
        let plan = self.panel.plan.clone();
        if let Some(view) = self.review.as_mut() {
            view.set_plan(plan);
        }
    }

    /// The question the cache is asked for the open pull request (FR-4.3).
    ///
    /// Built from the resolved selection rather than from the config file, because the
    /// key must describe what would actually be sent — including the thinking setting,
    /// which is the one part that only exists after the catalog has been consulted
    /// (FR-4.8).
    #[must_use]
    pub fn analysis_key(&self) -> Option<crate::ports::AnalysisKey> {
        let detail = self.detail.as_ref()?;
        let environment = self.environment.as_ref()?;
        let resolved = self.active_model.as_ref()?;
        Some(crate::ports::AnalysisKey {
            repo: environment.repo.key(),
            pr: detail.summary.number,
            head_sha: detail.summary.head_sha.clone(),
            provider: resolved.provider.clone(),
            model: resolved.model.clone(),
            thinking: self
                .config
                .llm
                .active
                .as_ref()
                .and_then(|selection| selection.reasoning.clone()),
            prompt_version: crate::domain::analysis::PROMPT_VERSION,
        })
    }

    /// The commit the current analysis describes, if there is one.
    #[must_use]
    pub fn analysis_head(&self) -> Option<String> {
        self.panel
            .analysis
            .as_ref()
            .map(|stored| stored.analysis.head_sha.clone())
            .or_else(|| {
                self.detail
                    .as_ref()
                    .map(|detail| detail.summary.head_sha.clone())
            })
    }

    /// The model, as the interface names it.
    pub(crate) fn model_label(&self) -> String {
        self.active_model.as_ref().map_or_else(
            || "the configured model".to_owned(),
            |resolved| format!("{}/{}", resolved.provider, resolved.model),
        )
    }

    /// Whether this repository has already been told what an analysis sends (FR-4.6).
    #[must_use]
    pub fn analysis_opt_in_recorded(&self) -> bool {
        let Some(environment) = self.environment.as_ref() else {
            return false;
        };
        self.state.analysis_opt_in.contains(&environment.repo.key())
    }

    /// Records the opt-in for this repository (FR-4.6).
    pub fn record_analysis_opt_in(&mut self) {
        let Some(environment) = self.environment.as_ref() else {
            return;
        };
        let key = environment.repo.key();
        if !self.state.analysis_opt_in.contains(&key) {
            self.state.analysis_opt_in.push(key);
        }
        self.panel.confirmed = false;
        // Agreeing once is the whole point of a one-time notice (FR-4.6): the record has
        // to reach the disk, or the next run asks again.
        self.state_dirty = true;
    }

    /// Whether the state file needs writing, clearing the flag.
    pub(crate) fn take_state_dirty(&mut self) -> bool {
        std::mem::take(&mut self.state_dirty)
    }

    /// Streams a piece of the answer into the preview (FR-4.4, FR-5.2).
    ///
    /// Both panes receive from the one progress channel and each takes the updates
    /// addressed to its own job, so text that arrives after `Esc`, or after the user
    /// started a different question, is discarded by job id rather than by hoping the
    /// timing works out.
    pub fn apply_progress(&mut self, progress: jobs::Progress) {
        if progress.job == self.chat.job {
            if let jobs::ProgressUpdate::Chat(update) = progress.update {
                self.apply_chat_progress(update);
            }
            return;
        }
        if progress.job != self.panel.job {
            return;
        }
        let jobs::ProgressUpdate::Analysis(update) = progress.update else {
            return;
        };
        match update {
            crate::application::analysis::Progress::Stage(stage) => {
                self.panel.state = if self.panel.stream.text().is_empty() {
                    AnalysisState::Running { stage }
                } else {
                    AnalysisState::Streaming { stage }
                };
            }
            crate::application::analysis::Progress::Delta(delta) => {
                let stage = match &self.panel.state {
                    AnalysisState::Running { stage } | AnalysisState::Streaming { stage } => {
                        stage.clone()
                    }
                    other => other.label(),
                };
                self.panel.stream.push(&delta);
                self.panel.state = AnalysisState::Streaming { stage };
            }
        }
    }

    /// Streams a chat answer into the pane (FR-5.2).
    fn apply_chat_progress(&mut self, update: crate::application::chat::Progress) {
        use crate::application::chat::Progress;
        use crate::tui::chat::ChatStatus;
        match update {
            Progress::Stage(stage) => {
                self.chat.status = if self.chat.stream.text().is_empty() {
                    ChatStatus::Sending { stage }
                } else {
                    ChatStatus::Streaming { stage }
                };
            }
            Progress::Delta(delta) => {
                let stage = match &self.chat.status {
                    ChatStatus::Sending { stage } | ChatStatus::Streaming { stage } => {
                        stage.clone()
                    }
                    other => other.label(),
                };
                self.chat.stream.push(&delta);
                self.chat.status = ChatStatus::Streaming { stage };
                // The newest text is what the user is waiting for, so the pane follows
                // it down unless they have scrolled up to read something else.
                self.chat.scroll = 0;
            }
        }
    }

    /// Builds the request an analysis would send (FR-4.1, FR-4.6).
    ///
    /// Returns `None` when there is nothing to analyse or nobody to ask: both are
    /// normal states, and the caller says which one it is.
    #[must_use]
    pub fn analysis_request(&self) -> Option<crate::application::analysis::AnalysisRequest> {
        let detail = self.detail.as_ref()?;
        let key = self.analysis_key()?;
        let resolved = self.active_model.as_ref()?;
        let secret = self
            .secret_store
            .get(&resolved.provider, resolved.env_var.as_deref())
            .ok()
            .flatten()?;
        // The output cap comes from the catalog when it declares one (FR-4.7).
        let output_limit = self
            .catalog
            .as_ref()
            .and_then(|state| {
                state
                    .load
                    .catalog
                    .model(&resolved.provider, &resolved.model)
            })
            .and_then(crate::domain::model::CatalogModel::output_limit);
        let chat = crate::application::models::analysis_chat(resolved, secret, output_limit);
        let policy = BundlePolicy {
            max_context_tokens: crate::application::models::context_budget(
                self.catalog
                    .as_ref()
                    .and_then(|state| {
                        state
                            .load
                            .catalog
                            .model(&resolved.provider, &resolved.model)
                    })
                    .and_then(crate::domain::model::CatalogModel::context_limit),
                self.config.llm.max_context_tokens,
                chat.max_tokens,
            ),
            max_file_bytes: self.config.llm.max_file_bytes,
            ..BundlePolicy::default()
        };
        Some(crate::application::analysis::AnalysisRequest {
            key,
            chat,
            detail: Box::new(detail.clone()),
            patch: self
                .review
                .as_ref()
                .map(|view| Box::new(view.patch.clone())),
            checkout: self.checkout(),
            policy,
        })
    }

    /// Where the files can be read, when a worktree exists (FR-3.1).
    #[must_use]
    fn checkout(&self) -> Option<crate::application::analysis::Checkout> {
        let workspace = self.workspace.as_ref()?;
        if !self.workspace_ready() {
            return None;
        }
        Some(crate::application::analysis::Checkout {
            path: workspace.path.clone(),
            head_sha: workspace.head_sha.clone(),
        })
    }

    /// Takes the gathered bundle when it belongs to the pull request on screen.
    ///
    /// A bundle gathered for another commit or another pull request is discarded
    /// rather than sent: it would describe a change the user is not looking at, which
    /// is the one thing a cached context must never do (FR-4.6).
    pub(crate) fn take_context_bundle_for_current_head(
        &mut self,
    ) -> Option<crate::domain::context::Bundle> {
        let current = self
            .detail
            .as_ref()
            .map(|detail| (detail.summary.number, detail.summary.head_sha.clone()));
        match (&self.panel.bundle_for, current) {
            (Some(built), Some(now)) if *built == now => {
                self.panel.bundle.take().map(|bundle| *bundle)
            }
            _ => {
                self.panel.bundle = None;
                self.panel.bundle_for = None;
                None
            }
        }
    }

    /// Starts a run: the stream is cleared and the state says so (FR-4.4).
    ///
    /// The panel opens with it, because a stream nobody is looking at is a spinner
    /// with extra steps.
    pub(crate) fn begin_analysis(&mut self) {
        self.panel.stream.clear();
        self.panel.raw = None;
        self.panel.state = AnalysisState::Running {
            stage: "asking the provider".to_owned(),
        };
        self.open_overlay(Overlay::Analysis);
    }

    /// Gives up on a run, keeping whatever text arrived (FR-4.4).
    pub(crate) fn cancelled_analysis(&mut self) {
        if self.panel.state.is_running() {
            self.panel.state = AnalysisState::Cancelled;
        }
    }

    /// Drops the current analysis so the next run recomputes it (FR-4.3).
    pub(crate) fn forget_analysis(&mut self) {
        self.panel.analysis = None;
        self.panel.stale = None;
        self.panel.warnings.clear();
        self.panel.raw = None;
        self.panel.stream.clear();
    }

    /// Remembers the job id of a stored-analysis read (FR-4.3).
    pub fn record_stored_job(&mut self, id: u64) {
        self.panel.stored_job = id;
    }

    /// Remembers the job id of a context gather (FR-4.6).
    pub fn record_context_job(&mut self, id: u64) {
        self.panel.context_job = id;
    }

    /// Remembers the job id of an analysis run (FR-4.4).
    pub fn record_analysis_job(&mut self, id: u64) {
        self.panel.job = id;
    }

    /// The environment, once detection has resolved one (FR-1.1).
    #[must_use]
    pub fn environment(&self) -> Option<&Environment> {
        self.environment.as_ref()
    }

    /// What the analysis is doing (FR-4.4).
    #[must_use]
    pub fn analysis_state(&self) -> &AnalysisState {
        &self.panel.state
    }

    /// The text that has streamed in so far (FR-4.4).
    #[must_use]
    pub fn analysis_stream(&self) -> &str {
        self.panel.stream.text()
    }

    /// Whether the streamed preview was cut short.
    #[must_use]
    pub fn analysis_stream_truncated(&self) -> bool {
        self.panel.stream.is_truncated()
    }

    /// Whether the stored analysis needed a repair pass (FR-4.1).
    #[must_use]
    pub fn analysis_repaired(&self) -> bool {
        self.panel
            .analysis
            .as_ref()
            .is_some_and(|stored| stored.repaired)
    }

    /// What was corrected while normalizing the analysis (FR-4.1).
    #[must_use]
    pub fn analysis_warnings(&self) -> &[String] {
        &self.panel.warnings
    }

    /// The gathered context bundle, if there is one (FR-4.6).
    #[must_use]
    pub fn context_bundle(&self) -> Option<&crate::domain::context::Bundle> {
        self.panel.bundle.as_deref()
    }

    /// The review plan in force (FR-4.2).
    #[must_use]
    pub fn plan(&self) -> Option<&crate::domain::plan::Plan> {
        self.panel.plan.as_ref()
    }

    /// What the analysis said about the file under the cursor (FR-4.1).
    ///
    /// Built from the note's parts so an empty field does not produce a stray dash;
    /// `notes` is the sentence worth reading, `change` the summary of what changed.
    #[must_use]
    pub fn current_file_note(&self) -> Option<String> {
        let stored = self.panel.analysis.as_ref()?;
        let path = self.review.as_ref()?.current_path()?.to_string();
        let note = stored.analysis.note(&path)?;
        let text = if note.notes.trim().is_empty() {
            note.change.trim()
        } else {
            note.notes.trim()
        };
        if text.is_empty() {
            // A note with a review_focus list and nothing else still says something.
            let focus = note.review_focus.join("; ");
            return (!focus.is_empty()).then(|| format!("check: {focus}"));
        }
        Some(text.to_owned())
    }

    /// The review plan, for editing (FR-4.2).
    pub fn plan_mut(&mut self) -> Option<&mut crate::domain::plan::Plan> {
        self.panel.plan.as_mut()
    }

    /// Replaces the review plan and re-orders the view to match (FR-4.2).
    pub fn set_plan(&mut self, plan: crate::domain::plan::Plan) {
        self.panel.plan = Some(plan);
        self.apply_plan_to_review();
    }

    /// Rebuilds the view after an override, keeping the file the cursor is in.
    pub(crate) fn after_plan_change(&mut self) {
        self.apply_plan_to_review();
    }

    /// The group the tree cursor is on, when it is on a group heading (FR-4.2).
    #[must_use]
    pub fn selected_plan_group(&self) -> Option<String> {
        let view = self.review.as_ref()?;
        if !view.tree_focused {
            return None;
        }
        match view.tree.get(view.tree_cursor).map(|row| &row.kind) {
            Some(crate::tui::diff_view::TreeKind::Group { name, .. }) => Some(name.clone()),
            _ => None,
        }
    }

    /// One line describing the review order, for `:plan` (FR-4.2).
    #[must_use]
    pub fn plan_summary(&self) -> String {
        let Some(plan) = self.panel.plan.as_ref() else {
            return "no review plan yet: <leader>a analyses the pull request".to_owned();
        };
        format!(
            "{} order, {} ({}){}",
            if self
                .review
                .as_ref()
                .is_some_and(|view| view.order == crate::domain::plan::OrderMode::Recommended)
            {
                "recommended"
            } else {
                "path"
            },
            plan.groups
                .iter()
                .map(|group| format!("{}.{} ({})", group.order, group.group, group.files.len()))
                .collect::<Vec<_>>()
                .join(", "),
            plan.source.label(),
            if plan.overridden {
                "; your manual order is in force"
            } else {
                ""
            }
        )
    }

    /// The model's unusable answer, with the reason (FR-4.1).
    #[must_use]
    pub fn raw_answer(&self) -> Option<&(String, String)> {
        self.panel.raw.as_ref()
    }

    /// Whether a model has been chosen and can be used (FR-4.5).
    #[must_use]
    pub fn has_model(&self) -> bool {
        self.active_model.is_some()
    }

    /// The analysis panel's contents (FR-4.1).
    #[must_use]
    pub fn analysis_panel(&self) -> Option<crate::application::analysis::PanelModel> {
        let stored = self.panel.analysis.as_ref()?;
        Some(crate::application::analysis::PanelModel::of(
            &stored.analysis,
        ))
    }

    /// The provenance line: which model, which prompt version, how old (FR-4.3).
    #[must_use]
    pub fn analysis_provenance(&self) -> Option<String> {
        let stored = self.panel.analysis.as_ref()?;
        let mut label = stored
            .analysis
            .provenance(crate::domain::time::from_unix_secs(
                i64::try_from(self.now_unix_secs).unwrap_or(i64::MAX),
            ));
        if !stored.analysis.matches_prompt() {
            let _ = std::fmt::Write::write_fmt(
                &mut label,
                format_args!(
                    " · made with prompt v{} (now v{})",
                    stored.analysis.prompt_version,
                    crate::domain::analysis::PROMPT_VERSION
                ),
            );
        }
        Some(label)
    }

    /// Whether the analysis on screen describes an older commit than the pull request
    /// is at (DEC-15).
    #[must_use]
    pub fn analysis_is_stale(&self) -> bool {
        let Some(stored) = self.panel.analysis.as_ref() else {
            return false;
        };
        self.detail
            .as_ref()
            .is_some_and(|detail| detail.summary.head_sha != stored.analysis.head_sha)
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
        if job == self.chat.job {
            self.chat.status = crate::tui::chat::ChatStatus::Failed {
                reason: message.to_owned(),
            };
            self.chat.pending = None;
            self.notice(
                NoticeLevel::Error,
                format!("the question failed: {message}"),
            );
            return;
        }
        if job == self.chat.load_job {
            self.chat.load_job = 0;
            return;
        }
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
                    if column < self.geometry.tree_width {
                        view.scroll_tree_by(delta * 3);
                    } else {
                        view.tree_focused = false;
                        view.scroll_by(delta * 3);
                    }
                }
            }
            Pane::Chat => {
                // The conversation scrolls *up* from its end (FR-5.2), which is why a
                // downward wheel reduces the offset rather than growing it.
                let step = usize::try_from(delta.abs() * 3).unwrap_or(3);
                if delta < 0 {
                    self.chat.scroll = self.chat.scroll.saturating_add(step);
                } else {
                    self.chat.scroll = self.chat.scroll.saturating_sub(step);
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
        self.sync_mode_to_focus();
        match pane {
            Pane::PullRequests => {
                // The visible row under the pointer, turned into an absolute index by
                // the same offset the frame drew with.
                if let Some(index) = crate::tui::components::pr_list::row_at(
                    self.geometry.list,
                    self.list.scroll,
                    row,
                ) && index < self.list.visible_len()
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
                    if column < self.geometry.tree_width {
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
            // A click in the chat pane focuses it and puts the cursor in the compose
            // box, which is the only thing there that can be edited.
            Pane::Chat => {
                self.chat.scroll = 0;
            }
        }
    }

    /// The row of a review pane a terminal row is over, as an offset into the visible
    /// rows, or `None` when it is over a border, the tab bar, or nothing.
    fn review_row(&self, row: u16) -> Option<usize> {
        let first = self.geometry.review_top.saturating_add(1);
        let last = self.geometry.body.bottom().saturating_sub(2);
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
            // The chat pane, when it is open, is below the review panes; the tree and
            // the diff share the rest.
            if let Some(chat) = self.geometry.chat
                && chat.contains(ratatui::layout::Position::from((column, row)))
            {
                return Some(Pane::Chat);
            }
            return (row >= self.geometry.review_top).then_some(Pane::Diff);
        }
        self.geometry
            .list
            .contains(ratatui::layout::Position::from((column, row)))
            .then_some(Pane::PullRequests)
    }

    /// Records the pane geometry of the last frame, so a mouse event can be placed.
    ///
    /// Called from `render`, which is where the sizes are known. It is arithmetic
    /// only: the render path still performs no IO.
    fn record_geometry(&mut self, body: ratatui::layout::Rect) {
        self.geometry.width = body.width;
        self.geometry.body = body;
        self.geometry.tree_width = crate::tui::components::review::TREE_WIDTH;
        self.geometry.review_top = body.y + 1;
        // The chat split comes from the same function the renderer uses, for the same
        // reason the list's does (FR-7.5).
        let (review_area, chat) =
            crate::tui::components::chat::chat_split(body, self.review.is_some() && self.chat.open);
        self.geometry.chat = chat;
        self.geometry.review_top = review_area.y + 1;
        // The filter bar sits above the list; the pane below it is the one the mouse
        // is tested against, and the same rectangle the renderer draws into.
        let (filter_bar, list) = components::panes::body_split(body);
        self.geometry.filter_bar = filter_bar;
        self.geometry.list = list;
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
        let height = crate::tui::components::pr_list::layout(self.geometry.list).height;
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
        self.geometry
            .list
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
        self.geometry.width
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
            // The compose box takes every key that the keymap has not claimed for
            // insert mode: a bare `j` is a letter in a question, and Enter sends
            // (FR-5.2). The keymap is consulted first so `<S-Enter>`, `<C-j>` and
            // `<Esc>` keep their meanings — they are the keys that are *not* text.
            Mode::Insert if self.chat_is_composing() => self.on_chat_input_key(combo),
            Mode::Normal | Mode::Insert | Mode::Visual => self.on_normal_key(combo),
        }
    }

    /// Keeps the input mode in step with which pane has the keyboard (FR-7.1).
    ///
    /// Only insert and normal mode move: a command line or a search box that is open
    /// while the pointer lands somewhere is not a mode change the user asked for.
    pub(crate) fn sync_mode_to_focus(&mut self) {
        // The leader menu is not a modal: it is a hint that disappears on the next key,
        // and it is *open* at the moment a `<leader>x` binding fires. Treating it as an
        // overlay here is what made `<leader>c` open the chat pane with the keyboard
        // still in normal mode.
        if !matches!(self.overlay, Overlay::None | Overlay::Leader) {
            return;
        }
        if self.focus == Pane::Chat {
            self.mode = Mode::Insert;
        } else if self.mode == Mode::Insert {
            self.mode = Mode::Normal;
        }
    }

    /// Whether the chat compose box is the thing the keyboard belongs to.
    ///
    /// A half-typed key sequence suspends that: `chat.open` moves the focus (and so the
    /// mode) as soon as its leader key is pressed, and if the *completing* key were then
    /// treated as text, `<leader>c` would open the pane and type a `c` into it.
    #[must_use]
    pub fn chat_is_composing(&self) -> bool {
        self.review.is_some()
            && self.chat.open
            && self.focus == Pane::Chat
            && self.overlay == Overlay::None
            && self.pending.is_empty()
    }

    /// A key pressed while the compose box has the keyboard (FR-5.2).
    ///
    /// The rule is the one the command line and the search box already use, extended to
    /// a multi-line box: **a printable character is text**, and everything else is a
    /// key. Without that rule the leader — a bare space by default — would start a key
    /// sequence in the middle of a question, and `<C-j>` would be a letter.
    fn on_chat_input_key(&mut self, combo: KeyCombo) -> Effect {
        if combo.code == KeyCode::Esc {
            return self.on_normal_key(combo);
        }
        let printable = matches!(combo.code, KeyCode::Char(_))
            && !combo
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if !printable && let Some(effect) = self.resolve_insert_binding(combo) {
            return effect;
        }
        match combo.code {
            KeyCode::Char(character) => {
                self.chat.input.insert(character);
                Effect::None
            }
            KeyCode::Backspace => {
                self.chat.input.backspace();
                Effect::None
            }
            KeyCode::Delete => {
                self.chat.input.delete();
                Effect::None
            }
            KeyCode::Left => {
                self.chat.input.left();
                Effect::None
            }
            KeyCode::Right => {
                self.chat.input.right();
                Effect::None
            }
            KeyCode::Up => {
                self.chat.input.up();
                Effect::None
            }
            KeyCode::Down => {
                self.chat.input.down();
                Effect::None
            }
            KeyCode::Home => {
                self.chat.input.home();
                Effect::None
            }
            KeyCode::End => {
                self.chat.input.end();
                Effect::None
            }
            // A key that is neither text nor bound goes to the normal path: `Tab` to
            // leave the pane, and a modified key the user tried — inserting "x" for an
            // unbound `Alt-x` would hide that it did nothing (FR-7.2).
            _ => self.on_normal_key(combo),
        }
    }

    /// The effect for a key bound in insert mode, if it is bound.
    fn resolve_insert_binding(&mut self, combo: KeyCombo) -> Option<Effect> {
        // `<C-c>` and the like are global; a bare letter is not bound in insert mode, so
        // this returns `None` for it and it becomes text.
        let action = match self.keymap.resolve(Mode::Insert, &[combo]) {
            // A match is the binding; ambiguous means this key is both a binding and the
            // start of a longer one, and in insert mode there is no longer one to wait
            // for — a bare letter is text, so anything bound at all fires now.
            keymap::Resolution::Match(binding) | keymap::Resolution::Ambiguous(binding) => {
                binding.action.clone()
            }
            keymap::Resolution::Prefix | keymap::Resolution::None => return None,
        };
        Some(update::dispatch(self, &action))
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

    /// The chat pane's state, when a pull request is open (FR-5.1).
    ///
    /// `None` outside the review screen: a conversation is *about* a pull request, and
    /// there is nothing for one to be about in the list.
    #[must_use]
    pub fn chat_state(&self) -> Option<&crate::tui::chat::ChatState> {
        (self.review.is_some() && self.chat.open).then_some(&self.chat)
    }

    /// The open pull request's detail.
    #[must_use]
    pub fn detail(&self) -> Option<&crate::domain::pr::PullRequestDetail> {
        self.detail.as_ref()
    }

    /// The current time, for the components that show an age.
    #[must_use]
    pub fn now_unix(&self) -> u64 {
        self.now_unix_secs
    }

    /// Whether the diff on screen contains a path, which is what makes a reference in
    /// an answer jumpable (FR-5.1).
    #[must_use]
    pub fn path_in_diff(&self, path: &str) -> bool {
        self.review.as_ref().is_some_and(|view| {
            view.patch.files.iter().any(|file| {
                file.path()
                    .is_some_and(|candidate| candidate.as_str() == path)
            })
        })
    }

    /// What the catalog says the active model costs, for the per-answer estimate
    /// (FR-5.4).
    #[must_use]
    pub fn model_cost(&self) -> Option<crate::domain::model::Cost> {
        let resolved = self.active_model.as_ref()?;
        self.catalog
            .as_ref()?
            .load
            .catalog
            .model(&resolved.provider, &resolved.model)
            .and_then(|model| model.cost.clone())
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

/// How long a chat answer may take before it is given up on.
///
/// Shorter than an analysis (600 s): a question about a pull request is answered in
/// seconds, so a request still open after two minutes is a dead connection rather than
/// a long answer, and the user is watching it.
pub const CHAT_TIMEOUT_SECS: u64 = 120;

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
    fn a_configured_model_gets_the_catalog_fetched_without_the_picker() {
        let (_dir, mut app) = app();
        // No cache, so the startup load failed; a model is configured, so the fetch
        // follows on its own rather than waiting for the picker to be opened.
        app.config.llm.active = Some(ModelSelection {
            provider: "fake".to_owned(),
            model: "fake-analysis-1".to_owned(),
            temperature: None,
            max_tokens: None,
            reasoning: None,
        });
        app.record_catalog_job(9);
        assert!(matches!(
            app.catalog_unavailable(9),
            Some(Effect::LoadCatalog(CatalogPolicy::Refresh))
        ));
        // Without a configured model the picker is where a model is chosen, so no
        // fetch is started behind the user's back.
        let (_dir2, mut bare) = super::tests::app();
        bare.record_catalog_job(9);
        assert!(bare.catalog_unavailable(9).is_none());
        // And a catalog that arrived is never fetched again by this path.
        let (_dir3, mut loaded) = super::tests::app();
        loaded.config.llm.active = Some(ModelSelection {
            provider: "fake".to_owned(),
            model: "fake-analysis-1".to_owned(),
            temperature: None,
            max_tokens: None,
            reasoning: None,
        });
        loaded.record_catalog_job(9);
        loaded.catalog = Some(crate::application::models::CatalogState {
            load: crate::ports::catalog::CatalogLoad {
                catalog: crate::domain::model::Catalog::from_json("{}").expect("empty"),
                source: crate::ports::catalog::CatalogSource::Fetched,
            },
            providers: Vec::new(),
        });
        assert!(loaded.catalog_unavailable(9).is_none());
    }

    #[test]
    fn the_automatic_catalog_fetch_happens_once() {
        let (_dir, mut app) = app();
        app.config.llm.active = Some(ModelSelection {
            provider: "fake".to_owned(),
            model: "fake-analysis-1".to_owned(),
            temperature: None,
            max_tokens: None,
            reasoning: None,
        });
        app.record_catalog_job(9);
        assert!(app.catalog_unavailable(9).is_some());
        // A second failure is a real failure: fetching again would be a loop.
        app.record_catalog_job(10);
        assert!(app.catalog_unavailable(10).is_none());
    }

    #[test]
    fn a_gathered_bundle_is_not_a_run_in_flight() {
        let (_dir, mut app) = app();
        app.panel.state = crate::tui::app::AnalysisState::Gathering;
        let bundle = crate::domain::context::build(
            &crate::domain::context::BundleInputs {
                metadata: "PR #141",
                ..crate::domain::context::BundleInputs::default()
            },
            &crate::domain::context::BundlePolicy::default(),
        );
        app.apply_context(
            bundle,
            crate::application::analysis::AnalysisIntent::Estimate,
        );
        assert_eq!(app.panel.state, crate::tui::app::AnalysisState::Confirming);
        assert!(!app.analysis_state().is_running());
        assert!(app.analysis_state().is_confirming());
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
        app.on_mouse(click(10, app.geometry.filter_bar.y));
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

        app.on_mouse(wheel(60, app.geometry.review_top + 4, true));
        let after = app.review.as_ref().unwrap().scroll;
        assert!(after > 0, "the wheel moves the diff text");

        app.on_mouse(wheel(60, app.geometry.review_top + 4, false));
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
        app.on_mouse(click(5, app.geometry.review_top + 12));
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

        app.on_mouse(click(5, app.geometry.review_top + 1));
        let view = app.review.as_ref().unwrap();
        assert!(view.tree_focused, "the tree has the cursor");
        assert!(view.folded_dirs.contains("src"), "the folder folded");
        assert!(
            !view.tree.iter().any(|row| row.label == "one.rs"),
            "and its files are hidden"
        );
    }

    /// An app with a review open, a model chosen, a fake provider and a fake store.
    ///
    /// The chat pane needs three things the other screens do not: something to talk
    /// about, somebody to talk to, and somewhere to put the conversation.
    fn chat_app() -> (
        TempHome,
        App,
        std::sync::Arc<crate::test_support::FakeChatStore>,
    ) {
        use crate::test_support::{FakeChatStore, sample_detail};
        // Built through `open_review` rather than `set_review`, because that is the call
        // that puts the focus in the review screen — and the focus is half of what these
        // tests are about.
        let (dir, mut app) = list_app();
        app.open_review(
            sample_detail(),
            crate::tui::diff_view::DiffView::new(crate::domain::diff::parse_patch(
                "diff --git a/src/one.rs b/src/one.rs\n--- a/src/one.rs\n+++ b/src/one.rs\n@@ -1 +1 @@\n-a\n+b\n",
            )),
        );
        app.active_model = Some(crate::application::models::ResolvedSelection {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
            route: crate::domain::model::Route::Native(
                crate::domain::model::NativeBackend::DeepSeek,
            ),
            base_url: None,
            env_var: Some("DEEPSEEK_API_KEY".to_owned()),
            env_source: None,
            from_file: true,
            thinking: None,
            warnings: Vec::new(),
        });
        let store = std::sync::Arc::new(FakeChatStore::default());
        app.chat_store = store.clone();
        app.set_environment(crate::test_support::environment());
        // A key, so `chat_request` resolves: a chat that cannot reach a provider cannot
        // be asked anything, and the tests below are about what happens when it can.
        app.secret_store = std::sync::Arc::new(crate::test_support::FixedSecrets::new("sk-test"));
        (dir, app, store)
    }

    #[test]
    fn the_chat_pane_opens_and_the_compose_box_takes_the_keyboard() {
        let (_dir, mut app, _store) = chat_app();
        press(&mut app, "<Space>c");
        assert!(app.chat.open, "the pane is on screen");
        assert_eq!(app.focus(), Pane::Chat);
        assert_eq!(
            app.mode(),
            Mode::Insert,
            "the keyboard belongs to the question"
        );
        // A letter is a letter in the compose box, not a navigation key: the failure
        // this guards is `j` moving a list under a box the user is typing in.
        press(&mut app, "why does it round?");
        assert_eq!(app.chat.input.text(), "why does it round?");
    }

    #[test]
    fn enter_sends_and_a_modified_enter_adds_a_line() {
        let (_dir, mut app, _store) = chat_app();
        press(&mut app, "<Space>c");
        press(&mut app, "does ");
        press(&mut app, "<S-Enter>");
        press(&mut app, "it round?");
        assert_eq!(app.chat.input.text(), "does \nit round?");
        // Enter is `chat.send`, which is an effect rather than an edit: the buffer is
        // untouched until the answer comes back.
        let effect = press(&mut app, "<Enter>");
        assert_eq!(effect, Effect::AskChat);
        assert_eq!(app.chat.input.text(), "does \nit round?");
    }

    #[test]
    fn a_gathered_context_is_what_the_question_sends() {
        // FR-4.6: the second press sends the bundle the first press estimated, so what
        // the user agreed to is what leaves the machine.
        let (_dir, mut app, _store) = chat_app();
        press(&mut app, "<Space>c");
        press(&mut app, "why?");
        assert_eq!(press(&mut app, "<Enter>"), Effect::AskChat);
        // Without an opt-in for this repository the first question waits for agreement.
        app.await_chat_confirmation();
        let bundle = crate::domain::context::build(
            &crate::domain::context::BundleInputs {
                metadata: "PR 141",
                commits: "one commit",
                conventions: Vec::new(),
                diff: None,
                files: Vec::new(),
            },
            &crate::domain::context::BundlePolicy::default(),
        );
        app.chat_bundle = Some(Box::new(bundle));
        app.chat_bundle_for = app
            .detail
            .as_ref()
            .map(|detail| (detail.summary.number, detail.summary.head_sha.clone()));
        assert!(app.chat.is_confirming());
        assert_eq!(press(&mut app, "<Enter>"), Effect::AskChat);
        // The second press takes the bundle instead of gathering another one.
        assert!(app.take_chat_bundle().is_some());
    }

    #[test]
    fn recording_the_opt_in_asks_for_the_state_file_to_be_written() {
        // FR-4.6's notice is once per repository, which is only true if the record
        // reaches the disk: this flag is what the loop writes it from.
        let (_dir, mut app, _store) = chat_app();
        assert!(!app.take_state_dirty(), "nothing to write at startup");
        app.record_analysis_opt_in();
        assert!(app.analysis_opt_in_recorded());
        assert!(app.take_state_dirty(), "the opt-in is worth writing");
        assert!(!app.take_state_dirty(), "and only once");
    }

    #[test]
    fn a_late_diff_does_not_pull_the_keyboard_out_of_the_compose_box() {
        // The order the validator produced: open a pull request, Tab to the chat pane,
        // and *then* let the worktree's local diff arrive. `apply_patch` calls
        // `open_review`, which used to re-focus the diff pane — so the next characters
        // typed became key bindings instead of words.
        let (_dir, mut app, _store) = chat_app();
        press(&mut app, "<Tab>");
        assert_eq!(app.focus(), Pane::Chat);
        app.diff_loading = true;

        let patch = crate::domain::diff::parse_patch(
            "diff --git a/src/one.rs b/src/one.rs\n--- a/src/one.rs\n+++ b/src/one.rs\n@@ -1 +1 @@\n-a\n+b\n",
        );
        let effect = app.apply_completion(crate::tui::jobs::Completion {
            job: app.patch_job,
            outcome: crate::tui::jobs::Outcome::Patch {
                outcome: Box::new(crate::application::prs::FetchOutcome::Fresh(patch)),
                source: crate::domain::diff::DiffSource::Worktree,
            },
        });
        assert!(effect.is_none());
        assert_eq!(app.focus(), Pane::Chat, "the compose box kept the keyboard");
        assert_eq!(app.mode(), Mode::Insert);
        press(&mut app, "why?");
        assert_eq!(app.chat.input.text(), "why?");
    }

    #[test]
    fn opening_a_review_takes_the_focus_from_the_list() {
        // And the other half of it: arriving at a review screen *does* move the focus.
        let (_dir, mut app) = list_app();
        assert_eq!(app.focus(), Pane::PullRequests);
        app.open_review(
            crate::test_support::sample_detail(),
            crate::tui::diff_view::DiffView::new(crate::domain::diff::parse_patch(
                "diff --git a/src/one.rs b/src/one.rs\n--- a/src/one.rs\n+++ b/src/one.rs\n@@ -1 +1 @@\n-a\n+b\n",
            )),
        );
        assert_eq!(app.focus(), Pane::Diff);
        assert_eq!(app.mode(), Mode::Normal);
    }

    #[test]
    fn the_first_question_shows_an_estimate_and_waits() {
        // The sequence the validator drives: type, press Enter, and the pane asks —
        // with the *size* of what would be sent, which is the whole point of asking.
        let (_dir, mut app, _store) = chat_app();
        press(&mut app, "<Space>c");
        press(&mut app, "why does it round?");
        assert_eq!(
            press(&mut app, "<Enter>"),
            Effect::AskChat,
            "the effect asks"
        );
        app.await_chat_confirmation();
        app.record_chat_load(42);

        // The gather job comes back with a bundle.
        let bundle = crate::domain::context::build(
            &crate::domain::context::BundleInputs {
                metadata: "PR 141",
                commits: "one commit",
                conventions: Vec::new(),
                diff: None,
                files: Vec::new(),
            },
            &crate::domain::context::BundlePolicy::default(),
        );
        let session = app.chat_request().expect("a request").1;
        app.apply_completion(crate::tui::jobs::Completion {
            job: 42,
            outcome: crate::tui::jobs::Outcome::ChatGathered {
                bundle: Box::new(bundle),
                session: Box::new(session),
                question: "why does it round?".to_owned(),
            },
        });
        assert!(app.chat.is_confirming(), "still waiting for agreement");
        let estimate = app.chat.estimate.as_ref().expect("an estimate");
        assert!(estimate.context_bytes > 0);
        assert!(estimate.label().contains("context"), "{}", estimate.label());
        assert!(
            estimate.label().starts_with('~'),
            "the label is an estimate and says so: {}",
            estimate.label()
        );
        // And the bundle is kept, so agreeing sends what was estimated.
        assert!(app.take_chat_bundle().is_some());
    }

    #[test]
    fn a_cancelled_answer_is_still_stored_when_its_job_finishes() {
        // The worker reports a stopped answer *after* `Esc`, so the completion has to
        // still be matched: the partial text is worth keeping (FR-5.2) and the store is
        // where a restart finds it.
        let (_dir, mut app, store) = chat_app();
        press(&mut app, "<Space>c");
        let session = crate::application::chat::new_session(
            "1-0".to_owned(),
            &crate::domain::repo::RepoId::parse("github.com/acme/service").expect("valid"),
            141,
            "abc123",
            "m",
            None,
            1_000,
        );
        app.begin_chat_answer("why?", session);
        app.chat.job = 12;
        app.chat.status = crate::tui::chat::ChatStatus::Streaming {
            stage: "asking".to_owned(),
        };
        app.stop_chat();
        assert_eq!(
            app.chat.status,
            crate::tui::chat::ChatStatus::Stopped,
            "the pane says so immediately"
        );

        let mut partial =
            crate::domain::chat::Message::assistant("half an ans", 1_000, None, Vec::new());
        partial.partial = true;
        app.apply_completion(crate::tui::jobs::Completion {
            job: 12,
            outcome: crate::tui::jobs::Outcome::ChatAnswered(Box::new(
                crate::tui::jobs::ChatAnswered {
                    run: crate::application::chat::ChatRun::Cancelled(Box::new(partial)),
                    session: "1-0".to_owned(),
                    question: "why?".to_owned(),
                },
            )),
        });
        let session = app.chat.session.as_ref().expect("session");
        assert_eq!(
            session.messages.len(),
            2,
            "the question and the partial answer"
        );
        assert!(session.messages[1].partial);
        assert_eq!(store.all().len(), 1);
        assert!(
            store.all()[0].messages[1].partial,
            "the stored conversation says the answer was stopped"
        );
    }

    #[test]
    fn escape_stops_an_answer_before_it_leaves_the_pane() {
        let (_dir, mut app, _store) = chat_app();
        press(&mut app, "<Space>c");
        app.chat.status = crate::tui::chat::ChatStatus::Streaming {
            stage: "asking".to_owned(),
        };
        assert_eq!(press(&mut app, "<Esc>"), Effect::CancelChat);
        assert!(app.chat.open, "the pane stays: the answer was stopped");
        // Once nothing is running, `Esc` leaves the pane instead.
        app.chat.status = crate::tui::chat::ChatStatus::Idle;
        assert_eq!(press(&mut app, "<Esc>"), Effect::None);
        assert!(!app.chat.open);
    }

    #[test]
    fn a_stopped_answer_is_stored_and_shown_as_stopped() {
        let (_dir, mut app, store) = chat_app();
        press(&mut app, "<Space>c");
        let session = crate::application::chat::new_session(
            "1-0".to_owned(),
            &crate::domain::repo::RepoId::parse("github.com/acme/service").expect("valid"),
            141,
            "abc123",
            "deepseek/deepseek-v4-pro",
            None,
            1_000,
        );
        app.chat.session = Some(session);
        app.chat.job = 7;
        let mut partial =
            crate::domain::chat::Message::assistant("half an ans", 1_000, None, Vec::new());
        partial.partial = true;
        app.apply_completion(crate::tui::jobs::Completion {
            job: 7,
            outcome: crate::tui::jobs::Outcome::ChatAnswered(Box::new(
                crate::tui::jobs::ChatAnswered {
                    run: crate::application::chat::ChatRun::Cancelled(Box::new(partial)),
                    session: "1-0".to_owned(),
                    question: "why?".to_owned(),
                },
            )),
        });
        let session = app.chat.session.as_ref().expect("the session");
        assert_eq!(session.messages.len(), 1);
        assert!(session.messages[0].partial);
        assert_eq!(
            store.all().len(),
            1,
            "a stopped answer is stored: restarting must not pretend it completed"
        );
        assert_eq!(app.chat.status, crate::tui::chat::ChatStatus::Stopped);
    }

    #[test]
    fn a_successful_answer_lands_in_the_conversation_the_question_opened() {
        let (_dir, mut app, store) = chat_app();
        press(&mut app, "<Space>c");
        let session = crate::application::chat::new_session(
            "1-0".to_owned(),
            &crate::domain::repo::RepoId::parse("github.com/acme/service").expect("valid"),
            141,
            "abc123",
            "deepseek/deepseek-v4-pro",
            None,
            1_000,
        );
        // The question is asked: this is where the conversation is created and stored.
        app.chat.input.set_text("does it round?");
        app.begin_chat_answer("does it round?", session);
        assert_eq!(app.chat.session.as_ref().expect("session").turns(), 1);
        assert_eq!(
            store.all().len(),
            1,
            "the question is stored before its answer exists"
        );
        app.chat.job = 9;
        app.chat.input.set_text("a question typed while waiting");
        app.apply_completion(crate::tui::jobs::Completion {
            job: 9,
            outcome: crate::tui::jobs::Outcome::ChatAnswered(Box::new(
                crate::tui::jobs::ChatAnswered {
                    run: crate::application::chat::ChatRun::Answered(Box::new(
                        crate::application::chat::Answered {
                            message: crate::domain::chat::Message::assistant(
                                "It rounds half up.",
                                1_000,
                                None,
                                Vec::new(),
                            ),
                            usage: None,
                            context_bytes: 100,
                        },
                    )),
                    session: "1-0".to_owned(),
                    question: "does it round?".to_owned(),
                },
            )),
        });
        assert_eq!(app.chat.status, crate::tui::chat::ChatStatus::Idle);
        assert_eq!(
            app.chat.input.text(),
            "a question typed while waiting",
            "the answer landing does not touch what the user has typed since"
        );
        let session = app.chat.session.as_ref().expect("session");
        assert_eq!(session.messages.len(), 2, "the question and its answer");
        assert_eq!(session.messages[0].role, crate::domain::chat::Role::User);
        assert_eq!(
            session.messages[1].role,
            crate::domain::chat::Role::Assistant
        );
        assert_eq!(store.all().len(), 1, "one conversation");
        assert_eq!(store.all()[0].messages.len(), 2, "written with both turns");
    }

    #[test]
    fn an_answer_for_a_conversation_that_is_no_longer_open_is_dropped() {
        // `:chat new` while an answer is arriving must not append it to the new
        // conversation: the id is what the answer is checked against.
        let (_dir, mut app, store) = chat_app();
        press(&mut app, "<Space>c");
        app.chat.session = Some(crate::application::chat::new_session(
            "2-0".to_owned(),
            &crate::domain::repo::RepoId::parse("github.com/acme/service").expect("valid"),
            141,
            "abc123",
            "m",
            None,
            1_000,
        ));
        app.chat.job = 3;
        app.apply_completion(crate::tui::jobs::Completion {
            job: 3,
            outcome: crate::tui::jobs::Outcome::ChatAnswered(Box::new(
                crate::tui::jobs::ChatAnswered {
                    run: crate::application::chat::ChatRun::Cancelled(Box::new(
                        crate::domain::chat::Message::assistant("late", 1, None, Vec::new()),
                    )),
                    session: "1-0".to_owned(),
                    question: "why?".to_owned(),
                },
            )),
        });
        assert!(
            app.chat
                .session
                .as_ref()
                .expect("session")
                .messages
                .is_empty(),
            "the late answer went nowhere"
        );
        assert!(store.all().is_empty());
    }

    #[test]
    fn adding_a_file_the_pull_request_does_not_have_is_refused_with_the_reason() {
        let (_dir, mut app, _store) = chat_app();
        let error = app.add_context_file("src/nowhere.rs").expect_err("refused");
        assert!(error.contains("src/nowhere.rs"), "{error}");
        assert!(app.chat.added.is_empty());
        // A changed file is accepted, and adding it twice is not two files.
        app.add_context_file("src/one.rs").expect("accepted");
        app.add_context_file("src/one.rs").expect("idempotent");
        assert_eq!(app.chat.added, vec!["src/one.rs".to_owned()]);
        assert!(app.remove_context_file("src/one.rs").is_ok());
        assert!(app.chat.added.is_empty());
    }

    #[test]
    fn a_secret_path_is_never_added_to_the_context() {
        // FR-4.6: the refusal happens when the user asks by name, because a bundle that
        // silently leaves out what they asked for is the one thing the requirement
        // exists to prevent.
        let (_dir, mut app, _store) = chat_app();
        let error = app.add_context_file(".env.local").expect_err("refused");
        assert!(error.contains("secret"), "{error}");
        assert!(app.chat.added.is_empty());
    }

    #[test]
    fn the_context_of_a_pull_request_survives_a_restart() {
        let (dir, mut app, _store) = chat_app();
        app.add_context_file("src/one.rs").expect("accepted");
        app.remember_context_files();
        // The loop writes the state file when an effect asks it to; here the write is
        // explicit, because "survives a restart" is a claim about the file.
        crate::state::save(&dir.path().join("state.toml"), &app.state).expect("saves");
        let key = app.context_files_key().expect("a key");
        assert_eq!(
            app.state.context_files.get(&key).map(Vec::as_slice),
            Some(["src/one.rs".to_owned()].as_slice())
        );
        // A new app on the same home reads it back, which is what makes the preference
        // a preference rather than a session detail.
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
        let mut fresh = App::new(Startup::load(&cli).unwrap()).unwrap();
        fresh.open_review(
            crate::test_support::sample_detail(),
            crate::tui::diff_view::DiffView::new(crate::domain::diff::parse_patch(
                "diff --git a/src/one.rs b/src/one.rs\n--- a/src/one.rs\n+++ b/src/one.rs\n@@ -1 +1 @@\n-a\n+b\n",
            )),
        );
        fresh.set_environment(crate::test_support::environment());
        fresh.load_context_files();
        assert_eq!(fresh.chat.added, vec!["src/one.rs".to_owned()]);
    }

    #[test]
    fn a_click_in_the_diff_moves_the_diff_cursor() {
        let (_dir, mut app) = review_app();
        frame(&mut app, 120, 30);

        app.on_mouse(click(80, app.geometry.review_top + 3));
        let view = app.review.as_ref().unwrap();
        assert!(!view.tree_focused);
        assert_eq!(view.cursor, 2, "the row under the pointer");
    }

    #[test]
    fn tab_walks_the_three_stops_of_a_review_screen() {
        // The diff text, the conversation, then the tree: the first version of this cycle
        // could not reach the chat pane at all, which is the kind of bug a test that only
        // checks "the focus changed" passes straight through.
        let (_dir, mut app, _store) = chat_app();
        assert_eq!(app.focus(), Pane::Diff);
        assert!(
            !app.review.as_ref().expect("review").tree_focused,
            "a review opens on the diff text"
        );

        let effect = press(&mut app, "<Tab>");
        assert_eq!(app.focus(), Pane::Chat, "second stop: the conversation");
        assert!(app.chat.open);
        assert_eq!(app.mode(), Mode::Insert);
        assert_eq!(
            effect,
            Effect::LoadChat,
            "a conversation is loaded on arrival"
        );

        press(&mut app, "<Tab>");
        assert_eq!(app.focus(), Pane::Diff);
        assert!(app.review.as_ref().expect("review").tree_focused);

        press(&mut app, "<Tab>");
        assert_eq!(app.focus(), Pane::Diff);
        assert!(
            !app.review.as_ref().expect("review").tree_focused,
            "and around"
        );

        // Backwards is the same cycle in reverse, and the pane is already open so
        // arriving at it asks for nothing.
        press(&mut app, "<S-Tab>");
        assert!(app.review.as_ref().expect("review").tree_focused);
        assert_eq!(press(&mut app, "<S-Tab>"), Effect::SaveState);
        assert_eq!(app.focus(), Pane::Chat);
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
