//! Background jobs: the only place in the interface that touches the outside world
//! (ARCH-5, NFR-1.2).
//!
//! The reducer returns an [`Effect`](crate::tui::app::Effect); this module turns it
//! into a job, runs the use case on a worker thread, and hands the result back to the
//! event loop as a completion. Three properties make the interface keep feeling
//! immediate:
//!
//! - **bounded**: at most [`MAX_IN_FLIGHT`] jobs run at once, and the rest wait in a
//!   queue rather than spawning threads without limit;
//! - **cancellable**: every job has a [`Cancel`], and a process job kills its child
//!   within one poll interval, so `Esc` is not a lie (NFR-1.4);
//! - **superseded results are dropped**: one job per *slot*, so a second list request
//!   cancels the first and the first's answer is discarded by job id rather than
//!   painting a stale list over a fresh one.
//!
//! Process-bound work runs on plain threads, not async tasks: `gh` and `git` block,
//! and a runtime would only add a dependency before M2 needs one for the LLM.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use crate::application::analysis::{
    AnalysisIntent, AnalysisRequest, AnalysisRun, Analyst, Progress as AnalysisProgress,
};
use crate::application::environment::{DetectRequest, detect};
use crate::application::prs::{FetchOutcome, Prs};
use crate::doctor::{self, Check, Context};
use crate::domain::diff::{DiffSource, Patch};
use crate::domain::environment::Environment;
use crate::domain::pr::PullRequestDetail;
use crate::domain::query::PrQuery;
use crate::domain::repo::RepoId;
use crate::logging::{self, Level};
use crate::ports::StateStore;
use crate::ports::analysis::{AnalysisCachePort, AnalysisKey, StoredAnalysis};
use crate::ports::cache::CacheStore;
use crate::ports::catalog::{CatalogLoad, CatalogPolicy, ModelCatalogPort};
use crate::ports::forge::{ForgePort, ForgeProbe, PullRequestPage};
use crate::ports::llm::{ChatOutcome, ChatRequest, LlmPort};
use crate::ports::workspace::{
    DiffOptions, DiffRequest, Workspace, WorkspacePort, WorkspaceRequest,
};
use crate::ports::{Cancel, Clock};
use crate::state::AppState;
use crate::tui::app::Effect;
use crate::tui::app::ReviewSession;
use crate::tui::list_view::PrListState;

/// How many progress messages may wait for the interface.
///
/// Producers wait when this is full instead of silently discarding text fragments.
/// The bound applies to every streaming job together, preventing a fast provider from
/// consuming unbounded memory (IR-08).
pub const MAX_QUEUED_PROGRESS: usize = 256;

/// How many progress messages the event loop accepts in one pass.
///
/// The remainder stays queued in order, so keyboard, mouse and completions cannot be
/// starved by a provider that produces faster than the terminal can draw (IR-08).
pub const MAX_PROGRESS_MESSAGES: usize = 64;

/// How long a worker waits before retrying a full progress queue.
const PROGRESS_BACKPRESSURE_POLL: Duration = Duration::from_millis(1);

/// How many jobs may run at once. Process jobs are the expensive ones, and four is
/// what ARCH-5 allows.
pub const MAX_IN_FLIGHT: usize = 4;

/// Disambiguates operation records created in one process. The process id added by
/// [`mutation_operation_id`] extends that distinction across rapid restarts (IR-07).
static NEXT_MUTATION_ID: AtomicU64 = AtomicU64::new(1);

fn mutation_operation_id(seconds: u64, process_id: u32, serial: u64) -> String {
    // The serial is process-local. Including the PID prevents two short-lived app
    // processes started in the same second from producing the same durable operation
    // id and incorrectly blocking the second confirmed mutation (IR-07).
    format!("{seconds}-{process_id}-{serial}")
}

/// Which kind of work a job is, one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// Small user preferences written to `state.toml`.
    ///
    /// State writes are serialized rather than cancelled: replacing a write that has
    /// already started would make an older snapshot win after a newer edit (IR-14).
    State,
    /// Detection, which happens once.
    Environment,
    /// The cached page, painted while the network is asked (FR-2.3).
    CachedList,
    /// A page of the list.
    List,
    /// The count of matches.
    Count,
    /// One pull request's detail.
    Detail,
    /// One pull request's diff.
    Patch,
    /// The environment report.
    Doctor,
    /// The model catalog (FR-4.7).
    Catalog,
    /// One pull request's managed worktree (FR-3.1).
    Workspace,
    /// The provider's answer to the picker's connection check (FR-4.5).
    ModelCheck,
    /// Reading a stored analysis, and gathering the context bundle (FR-4.3, FR-4.6).
    Analysis,
    /// The analysis request itself (FR-4.1).
    Analyze,
    /// Serial durable review-plan writes (FR-4.2, IR-14).
    PlanSave,
    /// Reading the chat sessions for a pull request (FR-5.1).
    Chat,
    /// One chat answer (FR-5.2).
    ChatAsk,
    /// Publishing a review (FR-6.3).
    Review,
    /// Replying into a thread, and commenting on the conversation (FR-6.4).
    ///
    /// One slot for both because they are the same resource — words added to the pull
    /// request — and the modal already prevents a second post from starting while one
    /// is in flight.
    Post,
    /// Resolving or unresolving a thread (FR-6.4).
    ///
    /// Its own slot, deliberately: resolving is not a post, and superseding a reply
    /// with a resolve would cancel a request that may already have reached GitHub.
    Thread,
    /// Reading the durable mutation journal (IR-07).
    Mutation,
    /// Checking an optional context path in the immutable workspace (IR-14).
    ContextPath,
    /// Reads a local draft for the open pull request.
    Draft,
    /// Serial durable draft writes and deletes (IR-14).
    DraftSave,
}

/// What a job was asked to do.
#[derive(Debug, Clone)]
pub enum Job {
    /// Persist a snapshot of the small application state (FR-8.5, IR-14).
    SaveState {
        /// The reducer revision this snapshot represents.
        revision: u64,
        /// The immutable snapshot to write.
        state: Box<AppState>,
    },
    /// Checks whether a user-added context path exists at the opened revision.
    ValidateContextPath {
        /// App-owned worktree path.
        workspace: std::path::PathBuf,
        /// Immutable revision to inspect.
        head_sha: String,
        /// Candidate relative path.
        path: String,
    },
    /// Reads a pull request's local draft.
    LoadDraft { pr: u64 },
    /// Writes an immutable draft snapshot.
    SaveDraft {
        draft: Box<crate::domain::draft::Draft>,
        revision: u64,
        reload_after: bool,
    },
    /// Deletes an already-cleared draft.
    DeleteDraft { pr: u64, revision: u64 },
    /// Work out where we are running (FR-1.1).
    Detect,
    /// Read the cached page, if there is one (FR-2.3).
    CachedList {
        /// The query.
        query: PrQuery,
    },
    /// Fetch a page of the list (FR-2.1).
    List {
        /// The query.
        query: PrQuery,
    },
    /// Count the matches (FR-2.1).
    Count {
        /// The query.
        query: PrQuery,
    },
    /// Fetch one pull request (FR-2.4).
    Detail {
        /// Which one.
        number: u64,
    },
    /// Fetch one pull request's diff (FR-3.2).
    Patch {
        /// Which one.
        number: u64,
        /// The head commit, which the cache is keyed by.
        head_sha: String,
        /// Display options captured before the background projection starts.
        options: DiffOptions,
        /// Immutable review context folded into the worker-built projection.
        view: Box<ViewContext>,
    },
    /// Fetch the model catalog (FR-4.7).
    Catalog {
        /// How hard to try the network.
        policy: CatalogPolicy,
    },
    /// Materialise a pull request in a managed worktree (FR-3.1).
    Workspace {
        /// What to materialise. Boxed for the same reason as the report: it is much
        /// larger than the other variants and every job crosses a channel.
        request: Box<WorkspaceRequest>,
    },
    /// Ask the provider whether a selection works (FR-4.5).
    ModelCheck {
        /// The check request, already built from the resolved selection.
        request: Box<ChatRequest>,
    },
    /// Remove managed worktrees (`:workspace clean`, FR-3.1).
    CleanWorkspaces {
        /// Remove every worktree, not only the ones past their age.
        all: bool,
        /// How old a worktree must be to be removed when `all` is false.
        max_age_secs: u64,
    },
    /// Diff the open pull request in its own worktree (FR-3.2).
    LocalPatch {
        /// Where the worktree is and which flags to use.
        request: Box<DiffRequest>,
        /// Which pull request, for the cache key.
        number: u64,
        /// The head commit, which the cached diff is keyed by.
        head_sha: String,
        /// Immutable review context folded into the worker-built projection.
        view: Box<ViewContext>,
    },
    /// Read whatever is already stored for a pull request (FR-4.3).
    LoadAnalysis {
        /// The question to look for, and the head it belongs to.
        key: Box<AnalysisKey>,
    },
    /// Gather the context bundle, which is the expensive local half of an analysis
    /// (FR-4.6).
    GatherContext {
        /// What to gather for.
        request: Box<AnalysisRequest>,
        /// What to do with the bundle when it arrives.
        intent: AnalysisIntent,
    },
    /// Ask the provider for an analysis (FR-4.1).
    RunAnalysis {
        /// What to ask.
        request: Box<AnalysisRequest>,
        /// The bundle that was gathered and shown as an estimate, so what the user
        /// agreed to send is what is sent.
        bundle: Box<crate::domain::context::Bundle>,
    },
    /// Writes one immutable review-plan snapshot off the event-loop thread.
    SavePlan {
        /// Pull request whose durable review state is being written.
        pr: u64,
        /// Immutable state captured by the reducer.
        plan: Box<crate::domain::plan::Plan>,
    },
    /// Read a pull request's chat sessions (FR-5.1).
    LoadChat {
        /// Which pull request.
        pr: u64,
        /// The conversation to open, when the caller has one in mind.
        open: Option<String>,
    },
    /// Gather the bundle one question would send (FR-5.3, FR-4.6).
    GatherChat {
        /// What to ask with, and about.
        spec: Box<crate::application::chat::ChatSpec>,
        /// The conversation as it stood when the question was asked.
        session: Box<crate::domain::chat::Session>,
        /// The question, carried through so the caller does not have to remember it.
        question: String,
    },
    /// Ask one question (FR-5.2, FR-5.3).
    ///
    /// The session is carried in the job rather than read inside it: the store is the
    /// caller's, and the conversation the answer belongs to must be the one the user was
    /// looking at when they pressed Enter.
    AskChat {
        /// What to ask.
        request: Box<ChatAsk>,
    },
    /// Publish the staged review (FR-6.3).
    ///
    /// The draft travels in the job rather than being read from the store inside it:
    /// the review that is sent must be the one the user was looking at when they
    /// confirmed it, whatever they type next.
    SubmitReview {
        /// The document to send.
        draft: Box<crate::domain::draft::Draft>,
    },
    /// Collect the environment report (FR-9.3).
    Report {
        /// What to check. Boxed because the context is much larger than any other
        /// variant's payload, and every job crosses a channel.
        context: Box<Context>,
    },
    /// Reply into a review thread (FR-6.4).
    ///
    /// The body travels in the job rather than being read from the interface inside
    /// it: the words that are sent must be the words the user confirmed, whatever they
    /// type next.
    PostReply {
        /// Which pull request.
        number: u64,
        /// The comment being answered, which is where GitHub attaches the reply.
        comment_id: u64,
        /// What to say.
        body: String,
    },
    /// Comment on the pull request's conversation (FR-6.4, DEC-16).
    PostConversation {
        /// Which pull request.
        number: u64,
        /// What to say.
        body: String,
    },
    /// Resolve or unresolve a thread (FR-6.4).
    ResolveThread {
        /// The pull request whose thread is changed.
        number: u64,
        /// GitHub's thread id.
        thread_id: String,
        /// Which way.
        resolved: bool,
    },
    /// Reads unresolved mutation records for a pull request (IR-07).
    RecoverMutations {
        /// The pull request whose journal is read.
        pr: u64,
    },
}

/// State known before a diff job starts and needed to prepare its first drawable view.
#[derive(Debug, Clone, Default)]
pub struct ViewContext {
    pub head_sha: String,
    pub comments: Vec<crate::domain::pr::ReviewComment>,
    pub plan: Option<crate::domain::plan::Plan>,
}

impl Job {
    /// Stable non-content label used by job timing records (NFR-5.3, IR-17).
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::SaveState { .. } => "save-state",
            Self::ValidateContextPath { .. } => "validate-context-path",
            Self::LoadDraft { .. } => "load-draft",
            Self::SaveDraft { .. } => "save-draft",
            Self::DeleteDraft { .. } => "delete-draft",
            Self::Detect => "detect-environment",
            Self::CachedList { .. } => "load-cached-list",
            Self::List { .. } => "list-pull-requests",
            Self::Count { .. } => "count-pull-requests",
            Self::Detail { .. } => "pull-request-detail",
            Self::Patch { .. } => "remote-patch",
            Self::Catalog { .. } => "model-catalog",
            Self::Workspace { .. } => "ensure-workspace",
            Self::ModelCheck { .. } => "model-check",
            Self::CleanWorkspaces { .. } => "clean-workspaces",
            Self::LocalPatch { .. } => "local-patch",
            Self::LoadAnalysis { .. } => "load-analysis",
            Self::GatherContext { .. } => "gather-context",
            Self::RunAnalysis { .. } => "run-analysis",
            Self::SavePlan { .. } => "save-plan",
            Self::LoadChat { .. } => "load-chat",
            Self::GatherChat { .. } => "gather-chat-context",
            Self::AskChat { .. } => "ask-chat",
            Self::SubmitReview { .. } => "submit-review",
            Self::Report { .. } => "doctor-report",
            Self::PostReply { .. } => "post-reply",
            Self::PostConversation { .. } => "post-conversation",
            Self::ResolveThread { .. } => "resolve-thread",
            Self::RecoverMutations { .. } => "recover-mutations",
        }
    }

    /// Which slot the job occupies.
    #[must_use]
    pub fn slot(&self) -> Slot {
        match self {
            Self::SaveState { .. } => Slot::State,
            Self::ValidateContextPath { .. } => Slot::ContextPath,
            Self::LoadDraft { .. } => Slot::Draft,
            Self::SaveDraft { .. } | Self::DeleteDraft { .. } => Slot::DraftSave,
            Self::Detect => Slot::Environment,
            Self::CachedList { .. } => Slot::CachedList,
            Self::List { .. } => Slot::List,
            Self::Count { .. } => Slot::Count,
            Self::Detail { .. } => Slot::Detail,
            // The local and remote diffs share a slot: only one can be the current
            // one, so starting either cancels the other.
            Self::Patch { .. } | Self::LocalPatch { .. } => Slot::Patch,
            Self::Report { .. } => Slot::Doctor,
            Self::Catalog { .. } => Slot::Catalog,
            // A worktree and its cleanup share a slot: they are the same resource.
            Self::Workspace { .. } | Self::CleanWorkspaces { .. } => Slot::Workspace,
            Self::ModelCheck { .. } => Slot::ModelCheck,
            // Reading and gathering are one slot: they are both "preparing the
            // analysis", and a second request should replace the first.
            Self::LoadAnalysis { .. } | Self::GatherContext { .. } => Slot::Analysis,
            Self::RunAnalysis { .. } => Slot::Analyze,
            Self::SavePlan { .. } => Slot::PlanSave,
            // Reading and writing are one slot: the list and the conversation are the
            // same resource, and a second request should replace the first.
            // Reading, gathering and asking are three slots: the first is about the
            // list, and the last two are about one question — a gather belongs to the
            // question that asked for it, so asking again replaces it, while the list
            // is unaffected.
            // Reading the list and gathering one question's bundle are the same slot
            // for different reasons: neither is the answer, and both are replaced by a
            // newer request for the same thing.
            Self::LoadChat { .. } | Self::GatherChat { .. } => Slot::Chat,
            Self::AskChat { .. } => Slot::ChatAsk,
            Self::SubmitReview { .. } => Slot::Review,
            Self::PostReply { .. } | Self::PostConversation { .. } => Slot::Post,
            Self::ResolveThread { .. } => Slot::Thread,
            Self::RecoverMutations { .. } => Slot::Mutation,
        }
    }

    /// Whether this job runs a process and can therefore be cancelled usefully.
    #[must_use]
    pub fn is_cancellable(&self) -> bool {
        !matches!(self, Self::Report { .. })
    }

    /// Whether writes in this slot must complete in submission order.
    fn is_ordered_save(&self) -> bool {
        matches!(
            self,
            Self::SaveState { .. }
                | Self::SaveDraft { .. }
                | Self::DeleteDraft { .. }
                | Self::SavePlan { .. }
        )
    }
}

/// One chat question, with the conversation it belongs to (FR-5.2, FR-5.3).
///
/// Separate from [`crate::application::chat::ChatSpec`] because a job is what crosses a
/// channel: the session is the state the answer will be appended to, and the question is
/// what the user typed. Both are owned here, so the worker thread never reaches back
/// into the interface.
#[derive(Debug, Clone)]
pub struct ChatAsk {
    /// What to ask with, and about.
    pub spec: crate::application::chat::ChatSpec,
    /// The conversation as it stood when the question was asked.
    pub session: Box<crate::domain::chat::Session>,
    /// The question.
    pub question: String,
    /// The gathered bundle, so what was estimated is what is sent (FR-4.6).
    pub bundle: Box<crate::domain::context::Bundle>,
}

/// What a job produced.
///
/// The heavy variants are boxed: `Outcome` is sent through a channel for every job,
/// and a patch or a detail can be large, so an unboxed enum would make every message
/// as big as the biggest one.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// A `state.toml` snapshot was durably written (IR-14).
    StateSaved {
        /// The revision the acknowledged snapshot represents.
        revision: u64,
    },
    /// The immutable workspace did or did not contain a requested context path.
    ContextPathValidated {
        /// Candidate path, echoed so a late result cannot add a different file.
        path: String,
        /// Whether it existed at the requested revision.
        exists: bool,
    },
    /// A draft read completed.
    DraftLoaded {
        pr: u64,
        draft: Option<Box<crate::domain::draft::Draft>>,
    },
    /// A draft snapshot was durably written.
    DraftSaved { revision: u64, reload_after: bool },
    /// A cleared draft was removed from durable storage.
    DraftDeleted { revision: u64 },
    /// Detection succeeded.
    Environment(Box<Environment>),
    /// Detection failed, with the reason to show.
    EnvironmentFailed(Box<crate::domain::environment::EnvironmentError>),
    /// A page arrived, or the cached one did.
    Page(Box<FetchOutcome<PullRequestPage>>),
    /// A cached page, painted before the network was asked (FR-2.3).
    CachedPage(Box<crate::application::prs::Cached<PullRequestPage>>),
    /// A count arrived.
    Count(u32),
    /// A detail arrived, or the cached one did.
    Detail(Box<FetchOutcome<PullRequestDetail>>),
    /// A diff arrived, or the cached one did.
    Patch {
        /// The diff with its expensive immutable display projection already prepared.
        outcome: Box<FetchOutcome<crate::tui::diff_view::DiffView>>,
        /// Where it was read from (FR-3.2).
        source: DiffSource,
        /// The revision the patch job requested.
        head_sha: String,
    },
    /// The environment report.
    Checks(Vec<Check>),
    /// The model catalog arrived (FR-4.7).
    Catalog(Box<CatalogLoad>),
    /// A worktree is ready (FR-3.1).
    Workspace(Box<Workspace>),
    /// The provider answered the connection check (FR-4.5).
    ModelChecked(Box<ChatOutcome>),
    /// What the analysis cache held for a pull request (FR-4.3, DEC-15).
    Stored {
        /// The entry matching the current question, if any.
        current: Option<Box<StoredAnalysis>>,
        /// An entry for the same pull request at an older commit, if any.
        stale: Option<Box<StoredAnalysis>>,
        /// The review-plan overrides stored for this pull request (FR-4.2).
        plan: Option<Box<crate::domain::plan::Plan>>,
    },
    /// A gathered context bundle (FR-4.6).
    Context {
        /// The bundle.
        bundle: Box<crate::domain::context::Bundle>,
        /// What the caller wanted it for.
        intent: AnalysisIntent,
        /// The exact context specification used to build it (IR-12).
        identity: crate::application::context::ContextIdentity,
    },
    /// The analysis finished, one way or another, with its submitted provenance.
    Analyzed {
        /// The outcome from the provider/normalizer.
        run: Box<AnalysisRun>,
        /// Immutable cache identity captured before the job started (IR-12).
        key: Box<crate::ports::AnalysisKey>,
    },
    /// A review-plan snapshot was durably written.
    PlanSaved {
        /// Pull request whose document was acknowledged.
        pr: u64,
        /// Store revision assigned to the acknowledged snapshot.
        document_revision: u64,
    },
    /// A pull request's sessions, and the one the caller asked to open (FR-5.1).
    ChatLoaded {
        /// The sessions that exist, newest first.
        sessions: Vec<crate::domain::chat::SessionMeta>,
        /// The conversation to show, when there is one to show.
        session: Option<Box<crate::domain::chat::Session>>,
        /// Whether the caller wanted a specific one and it was not there.
        missing: Option<String>,
    },
    /// A chat answer finished, one way or another (FR-5.2).
    ChatAnswered(Box<ChatAnswered>),
    /// The bundle a chat question would send, and the question it was gathered for
    /// (FR-5.3).
    ChatGathered {
        /// What was gathered.
        bundle: Box<crate::domain::context::Bundle>,
        /// The conversation it belongs to.
        session: Box<crate::domain::chat::Session>,
        /// The question that asked for it.
        question: String,
        /// The exact context specification used to build the bundle (IR-12).
        identity: crate::application::context::ContextIdentity,
    },
    /// A reply or a conversation comment was posted, or recorded by a dry run
    /// (FR-6.4, FR-6.5).
    CommentPosted(Box<crate::ports::CommentPosted>),
    /// A thread's state was changed (FR-6.4).
    ThreadResolved {
        /// Which thread, so the drawn discussion can be updated without waiting for the
        /// refresh that will confirm it.
        thread_id: String,
        /// What the thread is now.
        resolved: bool,
    },
    /// The durable mutation journal was checked for the current pull request (IR-07).
    MutationsRecovered {
        /// Whether another remote mutation must remain blocked.
        blocked: bool,
        /// An actionable warning when recovery could not prove safety.
        warning: Option<String>,
    },
    /// The review was posted, or recorded by a dry run (FR-6.3, FR-6.5).
    ReviewPosted(Box<crate::ports::ReviewPosted>),
    /// Worktrees were removed (FR-3.1).
    WorkspacesCleaned {
        /// How many were removed.
        removed: usize,
        /// How many were kept.
        kept: usize,
        /// The ones that could not be removed, with the reason.
        failed: Vec<String>,
    },
    /// The job failed, with the message to show.
    Failed(String),
    /// A remote mutation failed; its delivery may require reconciliation (IR-07).
    MutationFailed {
        /// The user-facing failure reason.
        message: String,
        /// Whether GitHub may have received the mutation.
        outcome_unknown: bool,
    },
    /// The job was replaced or abandoned before it finished.
    Abandoned,
}

fn prepared_patch(
    outcome: FetchOutcome<Patch>,
    source: DiffSource,
    head_sha: &str,
    options: DiffOptions,
    view: &ViewContext,
) -> Outcome {
    Outcome::Patch {
        outcome: Box::new(prepare_view(outcome, options, view)),
        source,
        head_sha: head_sha.to_owned(),
    }
}

fn prepare_view(
    outcome: FetchOutcome<Patch>,
    options: DiffOptions,
    context: &ViewContext,
) -> FetchOutcome<crate::tui::diff_view::DiffView> {
    let build = |patch| {
        crate::tui::diff_view::DiffView::new_with_context(
            patch,
            options.context,
            options.ignore_whitespace,
            Some(&context.head_sha),
            context.plan.clone(),
            context.comments.clone(),
        )
    };
    match outcome {
        FetchOutcome::Fresh(patch) => FetchOutcome::Fresh(build(patch)),
        FetchOutcome::Offline { value, reason } => FetchOutcome::Offline {
            value: build(value),
            reason,
        },
    }
}

impl Outcome {
    const fn result_label(&self) -> &'static str {
        match self {
            Self::EnvironmentFailed(_) | Self::Failed(_) | Self::MutationFailed { .. } => "failed",
            Self::Abandoned => "cancelled",
            _ => "ok",
        }
    }

    fn count_label(&self) -> String {
        match self {
            Self::Patch { outcome, .. } => {
                format!(" files={}", outcome.value().patch.files.len())
            }
            Self::Context { bundle, .. } | Self::ChatGathered { bundle, .. } => format!(
                " segments={} included={}",
                bundle.segments.len(),
                bundle
                    .segments
                    .iter()
                    .filter(|segment| segment.included)
                    .count()
            ),
            Self::Page(page) => format!(" pull_requests={}", page.value().items.len()),
            Self::Checks(checks) => format!(" checks={}", checks.len()),
            Self::WorkspacesCleaned {
                removed,
                kept,
                failed,
            } => format!(
                " removed={removed} kept={kept} failed_count={}",
                failed.len()
            ),
            _ => String::new(),
        }
    }
}

/// Something a running job wants the interface to know now (FR-4.4).
///
/// A separate channel from completions because the invariant that makes the runner
/// simple — one message per job, exactly once — is what makes it trustworthy, and
/// streaming text has no business breaking it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// Which job this is about.
    pub job: u64,
    /// Monotonic position within this job's progress stream.
    ///
    /// A terminal completion waits until this position has reached the interface, so
    /// it cannot overtake text or a repair reset still queued behind a busy frame.
    pub sequence: u64,
    /// The review session that requested this update, when it is PR-scoped (IR-05).
    pub owner: JobOwner,
    /// What it wants to say.
    pub update: ProgressUpdate,
}

/// What a job wants the interface to know while it runs.
///
/// One channel carries every job's updates, so the vocabulary has to be one type: an
/// analysis streams a JSON document and a chat answer streams prose, and the two
/// messages mean different things to the pane that receives them ("repairing" vs "the
/// model is typing"). Wrapping rather than sharing the variants keeps that difference
/// visible where it matters — in the pane — instead of in a comment about how the same
/// string means two things.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressUpdate {
    /// An analysis run (FR-4.4).
    Analysis(AnalysisProgress),
    /// A chat answer (FR-5.2).
    Chat(crate::application::chat::Progress),
}

/// A chat answer as a job result, with the conversation it belongs to (FR-5.2).
#[derive(Debug, Clone)]
pub struct ChatAnswered {
    /// What the answer was, or that it was stopped.
    pub run: crate::application::chat::ChatRun,
    /// Which conversation it belongs to, so a session switch discards it.
    pub session: String,
    /// The question it answered, which is what `r` repeats.
    pub question: String,
}

/// A finished job, on its way back to the event loop.
#[derive(Debug, Clone)]
pub struct Completion {
    /// Which job this is the answer to.
    pub job: u64,
    /// The last progress sequence sent before this completion.
    pub progress_through: u64,
    /// The review session that requested this work, when it is PR-scoped (IR-05).
    pub owner: JobOwner,
    /// What it produced.
    pub outcome: Outcome,
}

/// The state allowed to consume a background result (IR-05).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum JobOwner {
    /// Application-wide work such as environment detection or the model catalog.
    #[default]
    Global,
    /// Work derived from one visit to one pull request.
    Review(ReviewSession),
}

/// The ports one repository's jobs need (ARCH-2).
///
/// A struct rather than eight arguments: they are built together, replaced together,
/// and a call site with eight `Arc`s in a row is one where a swap is invisible.
#[derive(Debug)]
pub struct ExecutorPorts {
    /// The forge, once a repository is known.
    pub forge: Arc<dyn ForgePort>,
    /// The answer cache (FR-2.3).
    pub cache: Arc<dyn CacheStore>,
    /// Injected time.
    pub clock: Arc<dyn Clock>,
    /// The checkout and its worktrees (FR-3.1).
    pub workspace: Arc<dyn WorkspacePort>,
    /// Where analyses live (FR-4.3).
    pub analysis: Arc<dyn AnalysisCachePort>,
    /// Where chat sessions live (FR-5.1).
    pub chat: Arc<dyn crate::ports::ChatStorePort>,
    /// Where review drafts live (FR-6.1). The publishing job reads and clears the
    /// document it just sent, and that must not happen on the interface's thread.
    pub drafts: Arc<dyn crate::ports::DraftStorePort>,
    /// Durable remote-mutation records (IR-07).
    pub mutations: Arc<dyn crate::ports::MutationStorePort>,
    /// The provider (FR-4.1).
    pub llm: Arc<dyn LlmPort>,
    /// Which repository these are scoped to.
    pub repo: RepoId,
    /// How long the forge answers are reused.
    pub policy: crate::application::prs::CachePolicy,
    /// Whether forge mutations are recorded rather than dispatched (FR-6.5).
    pub dry_run: bool,
}

/// A job that is running.
#[derive(Debug)]
struct Running {
    id: u64,
    slot: Slot,
    cancel: Cancel,
}

/// The ports a job needs, once the repository is known.
#[derive(Debug)]
pub struct Executor {
    forge: Arc<dyn ForgePort>,
    cache: Arc<dyn CacheStore>,
    clock: Arc<dyn Clock>,
    workspace: Arc<dyn WorkspacePort>,
    /// Where analyses and their review-plan overrides are kept (FR-4.3).
    analysis: Arc<dyn AnalysisCachePort>,
    /// Where chat sessions are read and written (FR-5.1).
    chat: Arc<dyn crate::ports::ChatStorePort>,
    /// Where review drafts are read and written (FR-6.1).
    drafts: Arc<dyn crate::ports::DraftStorePort>,
    /// Durable remote-mutation records (IR-07).
    mutations: Arc<dyn crate::ports::MutationStorePort>,
    /// The provider, for the analysis request itself (FR-4.1).
    llm: Arc<dyn LlmPort>,
    repo: RepoId,
    policy: crate::application::prs::CachePolicy,
    /// Whether mutation outcomes are simulated (FR-6.5).
    dry_run: bool,
}

impl Executor {
    /// Binds the executor to a repository and the ports it reads through.
    #[must_use]
    pub fn new(ports: ExecutorPorts) -> Self {
        let ExecutorPorts {
            forge,
            cache,
            clock,
            workspace,
            analysis,
            chat,
            drafts,
            mutations,
            llm,
            repo,
            policy,
            dry_run,
        } = ports;
        Self {
            forge,
            cache,
            clock,
            workspace,
            analysis,
            chat,
            drafts,
            mutations,
            llm,
            repo,
            policy,
            dry_run,
        }
    }

    /// Runs one job to completion.
    fn run(&self, job: &Job, cancel: &Cancel, sink: &ProgressSink<'_>) -> Outcome {
        let prs = Prs::new(
            self.forge.as_ref(),
            self.cache.as_ref(),
            self.clock.as_ref(),
            &self.repo,
        )
        .with_policy(self.policy);

        match job {
            Job::CachedList { query } => match prs.cached_list(query) {
                Ok(Some(cached)) => Outcome::CachedPage(Box::new(cached)),
                // A miss is not a finding to report: the fetch that follows is what
                // the user is waiting for.
                Ok(None) => Outcome::Abandoned,
                Err(error) => Outcome::Failed(error.to_string()),
            },
            Job::List { query } => match prs.load_list(query, cancel) {
                Ok(outcome) => Outcome::Page(Box::new(outcome)),
                Err(error) => Outcome::Failed(error.to_string()),
            },
            Job::Count { query } => match prs.count(query, cancel) {
                Ok(count) => Outcome::Count(count),
                Err(error) => Outcome::Failed(error.to_string()),
            },
            Job::Detail { number } => match prs.load_detail(*number, cancel) {
                Ok(outcome) => Outcome::Detail(Box::new(outcome)),
                Err(error) => Outcome::Failed(error.to_string()),
            },
            Job::Patch {
                number,
                head_sha,
                options,
                view,
            } => match prs.load_patch(*number, head_sha, cancel) {
                Ok(outcome) => prepared_patch(outcome, DiffSource::Forge, head_sha, *options, view),
                Err(error) => Outcome::Failed(error.to_string()),
            },
            // A local diff is read from the worktree rather than from the forge, and
            // is parsed by the same total parser the remote patch goes through
            // (FR-3.2), so the review screen cannot tell them apart.
            Job::LocalPatch {
                request,
                number,
                head_sha,
                view,
            } => self.local_patch(&prs, request, *number, head_sha, view, cancel),
            // Reading what is stored, gathering what would be sent, and asking for
            // the analysis: all three need the repository, the clock and the cache,
            // which is exactly what the executor holds (FR-4.3, FR-4.6).
            Job::LoadAnalysis { key } => {
                let analyst = self.analyst();
                let plan = self.analysis.plan(&self.repo, key.pr).ok().flatten();
                match (analyst.cached(key), analyst.stale(key)) {
                    (Ok(current), Ok(stale)) => Outcome::Stored {
                        current: current.map(Box::new),
                        stale: stale.map(Box::new),
                        plan: plan.map(Box::new),
                    },
                    (Err(error), _) | (_, Err(error)) => Outcome::Failed(error.to_string()),
                }
            }
            Job::GatherContext { request, intent } => {
                sink.send(ProgressUpdate::Analysis(AnalysisProgress::Stage(format!(
                    "reading {} changed file(s) in bounded Git batches",
                    request.context.identity.changed_paths.len()
                ))));
                let (bundle, _) = self.analyst().gather(request, cancel);
                sink.send(ProgressUpdate::Analysis(AnalysisProgress::Stage(format!(
                    "prepared {} context segment(s)",
                    bundle.segments.len()
                ))));
                Outcome::Context {
                    bundle: Box::new(bundle),
                    intent: *intent,
                    identity: request.context.identity.clone(),
                }
            }
            Job::RunAnalysis { request, bundle } => {
                self.run_analysis(request, bundle, cancel, sink)
            }
            Job::SavePlan { pr, plan } => self.save_plan(*pr, plan),
            Job::LoadChat { .. } | Job::GatherChat { .. } | Job::AskChat { .. } => {
                self.chat_job(job, cancel, sink)
            }
            Job::SubmitReview { draft } => self.submit_review(draft, cancel),
            Job::PostReply {
                number,
                comment_id,
                body,
            } => self.post_reply(*number, *comment_id, body, cancel),
            Job::PostConversation { number, body } => self.post_conversation(*number, body, cancel),
            Job::ResolveThread {
                number,
                thread_id,
                resolved,
            } => self.resolve_thread(*number, thread_id, *resolved, cancel),
            Job::RecoverMutations { pr } => self.recover_mutations(*pr),
            // Detection, the report, the catalog, the worktree and the connection
            // check do not need a repository resolved through the forge, so
            // `JobRunner` handles them directly.
            Job::Detect
            | Job::SaveState { .. }
            | Job::ValidateContextPath { .. }
            | Job::LoadDraft { .. }
            | Job::SaveDraft { .. }
            | Job::DeleteDraft { .. }
            | Job::Report { .. }
            | Job::Catalog { .. }
            | Job::Workspace { .. }
            | Job::ModelCheck { .. }
            | Job::CleanWorkspaces { .. } => Outcome::Abandoned,
        }
    }

    /// Reads the durable mutation journal without allowing an unreadable record to
    /// become permission to post again (IR-07).
    fn recover_mutations(&self, pr: u64) -> Outcome {
        match self.mutations.unresolved(&self.repo, pr) {
            Ok(operations) if operations.is_empty() => Outcome::MutationsRecovered {
                blocked: false,
                warning: None,
            },
            Ok(operations) => Outcome::MutationsRecovered {
                blocked: true,
                warning: Some(format!(
                    "{} earlier GitHub mutation(s) have an unknown outcome; check the pull request before posting again",
                    operations.len()
                )),
            },
            Err(error) => Outcome::MutationsRecovered {
                blocked: true,
                warning: Some(format!(
                    "could not recover earlier GitHub mutations: {error}; do not retry until it is fixed"
                )),
            },
        }
    }

    /// Sends one review and clears the draft it came from (FR-6.3).
    ///
    /// Clearing happens here, in the job, and only after the forge said yes: the
    /// review is on GitHub by then, and a draft that outlived its own publication
    /// would be sent a second time by the next person who pressed Enter.
    ///
    /// A dry run clears nothing — nothing was sent (FR-6.5) — and the loop writes the
    /// recorded calls out where the user can read them.
    fn submit_review(&self, draft: &crate::domain::draft::Draft, cancel: &Cancel) -> Outcome {
        if cancel.is_cancelled() {
            return cancelled_before_dispatch();
        }
        let mut operation = match self.prepare_mutation(
            draft.pr,
            draft.head_sha.clone(),
            crate::domain::mutation::MutationKind::Review {
                draft: draft.clone(),
            },
        ) {
            Ok(operation) => operation,
            Err(error) => return Outcome::Failed(error),
        };
        let service =
            crate::application::drafts::Drafts::new(Arc::clone(&self.drafts), self.repo.clone());
        match service.publish(self.forge.as_ref(), draft, cancel) {
            Ok(posted) => {
                let cleanup = (!posted.dry_run)
                    .then(|| service.remove_if_matches(draft))
                    .transpose();
                match cleanup {
                    Ok(_) => self.record_success(
                        &mut operation,
                        posted.id,
                        posted.url.clone(),
                        posted.dry_run,
                    ),
                    Err(error) => self.record_failure(
                        &mut operation,
                        format!("GitHub accepted the review, but its draft could not be cleared safely: {error}"),
                        true,
                    ),
                }
                Outcome::ReviewPosted(Box::new(posted))
            }
            Err(error) => {
                let outcome_unknown = publish_failure_is_unknown(&error);
                self.record_failure(&mut operation, error.to_string(), outcome_unknown);
                Outcome::MutationFailed {
                    message: error.to_string(),
                    outcome_unknown,
                }
            }
        }
    }

    /// Posts a reply, through the service that validates it (FR-6.4).
    fn post_reply(&self, number: u64, comment_id: u64, body: &str, cancel: &Cancel) -> Outcome {
        if cancel.is_cancelled() {
            return cancelled_before_dispatch();
        }
        let mut operation = match self.prepare_mutation(
            number,
            None,
            crate::domain::mutation::MutationKind::Reply {
                comment_id,
                body: body.to_owned(),
            },
        ) {
            Ok(operation) => operation,
            Err(error) => return Outcome::Failed(error),
        };
        let posts = crate::application::posts::Posts::new(self.forge.as_ref(), self.repo.clone());
        match posts.reply(number, comment_id, body, cancel) {
            Ok(posted) => {
                self.record_success(
                    &mut operation,
                    posted.id,
                    posted.url.clone(),
                    posted.dry_run,
                );
                Outcome::CommentPosted(Box::new(posted))
            }
            Err(error) => {
                let outcome_unknown = reply_failure_is_unknown(&error);
                self.record_failure(&mut operation, error.to_string(), outcome_unknown);
                Outcome::MutationFailed {
                    message: error.to_string(),
                    outcome_unknown,
                }
            }
        }
    }

    /// Posts a comment on the pull request's conversation (FR-6.4).
    fn post_conversation(&self, number: u64, body: &str, cancel: &Cancel) -> Outcome {
        if cancel.is_cancelled() {
            return cancelled_before_dispatch();
        }
        let mut operation = match self.prepare_mutation(
            number,
            None,
            crate::domain::mutation::MutationKind::Conversation {
                body: body.to_owned(),
            },
        ) {
            Ok(operation) => operation,
            Err(error) => return Outcome::Failed(error),
        };
        let posts = crate::application::posts::Posts::new(self.forge.as_ref(), self.repo.clone());
        match posts.comment(number, body, cancel) {
            Ok(posted) => {
                self.record_success(
                    &mut operation,
                    posted.id,
                    posted.url.clone(),
                    posted.dry_run,
                );
                Outcome::CommentPosted(Box::new(posted))
            }
            Err(error) => {
                let outcome_unknown = reply_failure_is_unknown(&error);
                self.record_failure(&mut operation, error.to_string(), outcome_unknown);
                Outcome::MutationFailed {
                    message: error.to_string(),
                    outcome_unknown,
                }
            }
        }
    }

    /// Resolves or unresolves a thread (FR-6.4).
    fn resolve_thread(
        &self,
        number: u64,
        thread_id: &str,
        resolved: bool,
        cancel: &Cancel,
    ) -> Outcome {
        if cancel.is_cancelled() {
            return cancelled_before_dispatch();
        }
        let mut operation = match self.prepare_mutation(
            number,
            None,
            crate::domain::mutation::MutationKind::ThreadResolution {
                thread_id: thread_id.to_owned(),
                resolved,
            },
        ) {
            Ok(operation) => operation,
            Err(error) => return Outcome::Failed(error),
        };
        let posts = crate::application::posts::Posts::new(self.forge.as_ref(), self.repo.clone());
        match posts.resolve(thread_id, resolved, cancel) {
            Ok(()) => {
                self.record_success(&mut operation, None, None, self.dry_run);
                Outcome::ThreadResolved {
                    thread_id: thread_id.to_owned(),
                    resolved,
                }
            }
            Err(error) => {
                let outcome_unknown = reply_failure_is_unknown(&error);
                self.record_failure(&mut operation, error.to_string(), outcome_unknown);
                Outcome::MutationFailed {
                    message: error.to_string(),
                    outcome_unknown,
                }
            }
        }
    }

    /// Creates and marks a mutation durable before the forge can receive it (IR-07).
    fn prepare_mutation(
        &self,
        pr: u64,
        head_sha: Option<String>,
        kind: crate::domain::mutation::MutationKind,
    ) -> Result<crate::domain::mutation::MutationOperation, String> {
        let seconds = self.clock.now_unix_secs();
        let now = crate::domain::time::from_unix_secs(i64::try_from(seconds).unwrap_or(i64::MAX));
        let serial = NEXT_MUTATION_ID.fetch_add(1, Ordering::Relaxed);
        let mut operation = crate::domain::mutation::MutationOperation::queued(
            mutation_operation_id(seconds, std::process::id(), serial),
            self.repo.clone(),
            pr,
            head_sha,
            kind,
            now,
        );
        // Crossing this durable boundary means the worker may next invoke the forge.
        // A queued operation is therefore never left on disk: a crash before this point
        // is definitely unsent, while every persisted record blocks a blind retry.
        operation.mark_dispatching(now);
        self.mutations.begin(&operation).map_err(|error| {
            format!("could not record this operation; it was not sent: {error}")
        })?;
        Ok(operation)
    }

    /// Keeps a confirmed remote result even if persisting its receipt fails.
    fn record_success(
        &self,
        operation: &mut crate::domain::mutation::MutationOperation,
        id: Option<u64>,
        url: Option<String>,
        dry_run: bool,
    ) {
        let now = crate::domain::time::from_unix_secs(
            i64::try_from(self.clock.now_unix_secs()).unwrap_or(i64::MAX),
        );
        if dry_run {
            operation.mark_simulated(now);
        } else {
            operation.mark_succeeded(id, url, now);
        }
        if let Err(error) = self.mutations.save(operation) {
            logging::log(
                Level::Warn,
                format!(
                    "remote mutation succeeded but its recovery record could not be updated: {error}"
                ),
            );
        }
    }

    /// GitHub's explicit refusal is safe to retry. A canceled child, timeout or an
    /// unclassified transport failure may have reached GitHub and remains unknown.
    fn record_failure(
        &self,
        operation: &mut crate::domain::mutation::MutationOperation,
        reason: String,
        outcome_unknown: bool,
    ) {
        let now = crate::domain::time::from_unix_secs(
            i64::try_from(self.clock.now_unix_secs()).unwrap_or(i64::MAX),
        );
        if outcome_unknown {
            operation.mark_outcome_unknown(reason, now);
        } else {
            operation.mark_rejected(reason, now);
        }
        if let Err(error) = self.mutations.save(operation) {
            logging::log(
                Level::Warn,
                format!("could not update the remote mutation record: {error}"),
            );
        }
    }

    /// Diffs the open pull request in its own worktree (FR-3.2).
    ///
    /// Extracted from [`Executor::run`] because it is the one arm with three
    /// fallbacks — cache, worktree, cached-with-a-reason — and reading them inside a
    /// twenty-arm match hides exactly the part that matters.
    fn local_patch(
        &self,
        prs: &Prs<'_>,
        request: &DiffRequest,
        number: u64,
        head_sha: &str,
        view: &ViewContext,
        cancel: &Cancel,
    ) -> Outcome {
        let options = request.options;
        // Cache first, with the flags in the key: re-opening a file with the
        // same toggles must not re-run git (FR-3.2).
        match prs.cached_patch_with(number, head_sha, options, DiffSource::Worktree) {
            Ok(Some(cached)) if !cached.stale => {
                return Outcome::Patch {
                    outcome: Box::new(prepare_view(
                        FetchOutcome::Fresh(cached.value),
                        options,
                        view,
                    )),
                    source: DiffSource::Worktree,
                    head_sha: head_sha.to_owned(),
                };
            }
            Ok(_) | Err(_) => {}
        }
        logging::log(
            Level::Debug,
            format!(
                "diffing {} locally at {} (context {}, whitespace {})",
                request.path.display(),
                head_sha,
                options.context,
                if options.ignore_whitespace {
                    "ignored"
                } else {
                    "shown"
                }
            ),
        );
        match self.workspace.diff(request, cancel) {
            Ok(text) => {
                let patch = crate::domain::diff::parse_patch(&text);
                let _ = prs.store_local_patch(number, head_sha, options, &patch);
                Outcome::Patch {
                    outcome: Box::new(prepare_view(FetchOutcome::Fresh(patch), options, view)),
                    source: DiffSource::Worktree,
                    head_sha: head_sha.to_owned(),
                }
            }
            // The worktree is gone: the caller falls back to the forge, and
            // says so rather than showing an empty diff.
            Err(error) => {
                match prs.cached_patch_with(number, head_sha, options, DiffSource::Worktree) {
                    Ok(Some(cached)) => Outcome::Patch {
                        outcome: Box::new(prepare_view(
                            FetchOutcome::Offline {
                                value: cached.value,
                                reason: error.to_string(),
                            },
                            options,
                            view,
                        )),
                        source: DiffSource::Worktree,
                        head_sha: head_sha.to_owned(),
                    },
                    _ => Outcome::Failed(error.to_string()),
                }
            }
        }
    }

    /// Runs one of the three chat jobs (FR-5.1–FR-5.3).
    ///
    /// Extracted from [`Executor::run`] because it is the one group whose arms are about
    /// a *conversation* rather than about a request, and because the match there is at
    /// the line limit clippy enforces.
    fn chat_job(&self, job: &Job, cancel: &Cancel, sink: &ProgressSink<'_>) -> Outcome {
        match job {
            Job::LoadChat { pr, open } => self.load_chat(*pr, open.as_deref()),
            Job::GatherChat {
                spec,
                session,
                question,
            } => {
                let bundle = self.chatter().gather(spec, cancel);
                Outcome::ChatGathered {
                    bundle: Box::new(bundle),
                    session: session.clone(),
                    question: question.clone(),
                    identity: spec.context.identity.clone(),
                }
            }
            Job::AskChat { request } => {
                let chatter = self.chatter();
                // The chat has its own progress vocabulary, wrapped rather than
                // flattened into the analysis one: one channel, one variant, one place
                // to look when the pane shows the wrong thing.
                let mut report = |update: crate::application::chat::Progress| {
                    sink.send(ProgressUpdate::Chat(update));
                };
                match chatter.ask(
                    &request.spec,
                    &request.session,
                    &request.bundle,
                    &request.question,
                    cancel,
                    &mut report,
                ) {
                    Ok(run) => Outcome::ChatAnswered(Box::new(ChatAnswered {
                        run,
                        session: request.session.id.clone(),
                        question: request.question.clone(),
                    })),
                    Err(error) => Outcome::Failed(error.to_string()),
                }
            }
            _ => Outcome::Abandoned,
        }
    }

    /// Reads a pull request's conversations (FR-5.1).
    ///
    /// Never fails: a chat store that cannot be read is a store with no conversations
    /// in it as far as the interface is concerned, and the error the user needs to see
    /// is the one about *sending*, not about listing.
    fn load_chat(&self, pr: u64, open: Option<&str>) -> Outcome {
        let sessions = self.chat.list(&self.repo, pr).unwrap_or_default();
        let wanted = match open {
            Some(id) => Some(id.to_owned()),
            // The newest conversation is what the pane opens by default, which is what
            // makes restarting the app land on the conversation you were having.
            None => sessions.first().map(|meta| meta.id.clone()),
        };
        match wanted {
            Some(id) => match self.chat.load(&self.repo, pr, &id) {
                Ok(Some(session)) => Outcome::ChatLoaded {
                    sessions,
                    session: Some(Box::new(session)),
                    missing: None,
                },
                Ok(None) => Outcome::ChatLoaded {
                    sessions,
                    session: None,
                    missing: Some(id),
                },
                Err(error) => Outcome::Failed(error.to_string()),
            },
            None => Outcome::ChatLoaded {
                sessions,
                session: None,
                missing: None,
            },
        }
    }

    /// The chat use case, bound to this repository's ports.
    fn chatter(&self) -> crate::application::chat::Chatter<'_> {
        crate::application::chat::Chatter::new(
            self.workspace.as_ref(),
            self.llm.as_ref(),
            self.clock.as_ref(),
            &self.repo,
        )
    }

    /// The analysis use case, bound to this repository's ports.
    fn analyst(&self) -> Analyst<'_> {
        Analyst::new(
            self.workspace.as_ref(),
            self.analysis.as_ref(),
            self.llm.as_ref(),
            self.clock.as_ref(),
            &self.repo,
        )
    }

    /// Runs and reports one analysis request.
    fn run_analysis(
        &self,
        request: &AnalysisRequest,
        bundle: &crate::domain::context::Bundle,
        cancel: &Cancel,
        sink: &ProgressSink<'_>,
    ) -> Outcome {
        let analyst = self.analyst();
        let mut report = |update: AnalysisProgress| {
            sink.send(ProgressUpdate::Analysis(update));
        };
        match analyst.run(request, bundle, cancel, &mut report) {
            Ok(run) => Outcome::Analyzed {
                run: Box::new(run),
                key: Box::new(request.key.clone()),
            },
            Err(error) => Outcome::Failed(error.to_string()),
        }
    }

    /// Persists one ordered plan snapshot away from the event loop.
    fn save_plan(&self, pr: u64, plan: &crate::domain::plan::Plan) -> Outcome {
        match self.analysis.put_plan(&self.repo, pr, plan) {
            Ok(saved) => Outcome::PlanSaved {
                pr,
                document_revision: saved.document_revision,
            },
            Err(error) => Outcome::Failed(format!("could not save review progress: {error}")),
        }
    }
}

fn is_definite_forge_refusal(error: &crate::Error) -> bool {
    error.forge_delivery() == Some(crate::error::ForgeDelivery::Refused)
}

fn cancelled_before_dispatch() -> Outcome {
    Outcome::MutationFailed {
        message: "cancelled before the request was sent".to_owned(),
        outcome_unknown: false,
    }
}

fn publish_failure_is_unknown(error: &crate::application::drafts::PublishError) -> bool {
    match error {
        crate::application::drafts::PublishError::Refused(_) => false,
        crate::application::drafts::PublishError::Forge(error) => !is_definite_forge_refusal(error),
    }
}

fn reply_failure_is_unknown(error: &crate::application::posts::ReplyError) -> bool {
    match error {
        crate::application::posts::ReplyError::Refused(_) => false,
        crate::application::posts::ReplyError::Forge(error) => !is_definite_forge_refusal(error),
    }
}

/// Runs jobs, at most [`MAX_IN_FLIGHT`] at a time, one per slot.
pub struct JobRunner {
    /// The ports, once detection has resolved a repository.
    executor: Option<Arc<Executor>>,
    /// The workspace, for detection.
    workspace: Arc<dyn WorkspacePort>,
    /// The forge probe, for detection.
    probe: Arc<dyn ForgeProbe>,
    /// The model catalog, for the picker (FR-4.7).
    catalog: Arc<dyn ModelCatalogPort>,
    /// The LLM client, for the picker's connection check (FR-4.5).
    llm: Arc<dyn LlmPort>,
    /// The one small global document whose writes must not run on the UI thread.
    state_store: Arc<dyn StateStore>,
    /// What the command line asked for.
    request: DetectRequest,
    /// Jobs waiting for a free slot.
    queue: VecDeque<(u64, JobOwner, Job)>,
    /// Jobs that are running.
    running: Vec<Running>,
    /// The next job id.
    next_id: u64,
    sender: Sender<Completion>,
    receiver: Receiver<Completion>,
    /// Completions whose earlier progress has not reached the reducer yet.
    pending_completions: VecDeque<Completion>,
    /// Highest progress sequence removed from the channel for each job.
    delivered_progress: HashMap<u64, u64>,
    /// Progress from jobs that stream.
    progress_sender: SyncSender<Progress>,
    progress_receiver: Receiver<Progress>,
}

impl std::fmt::Debug for JobRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobRunner")
            .field("queued", &self.queue.len())
            .field("running", &self.running.len())
            .field("has_executor", &self.executor.is_some())
            .finish_non_exhaustive()
    }
}

impl JobRunner {
    /// Builds a runner with the ports detection needs.
    #[must_use]
    pub fn new(
        workspace: Arc<dyn WorkspacePort>,
        probe: Arc<dyn ForgeProbe>,
        catalog: Arc<dyn ModelCatalogPort>,
        llm: Arc<dyn LlmPort>,
        state_store: Arc<dyn StateStore>,
        request: DetectRequest,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let (progress_sender, progress_receiver) = mpsc::sync_channel(MAX_QUEUED_PROGRESS);
        Self {
            executor: None,
            workspace,
            probe,
            catalog,
            llm,
            state_store,
            request,
            queue: VecDeque::new(),
            running: Vec::new(),
            // Job ids start at one so that the `Option`-free "no job yet" sentinel of
            // zero in the app can never collide with a real id.
            next_id: 1,
            sender,
            receiver,
            pending_completions: VecDeque::new(),
            delivered_progress: HashMap::new(),
            progress_sender,
            progress_receiver,
        }
    }

    /// Supplies the ports a repository-scoped job needs.
    pub fn set_executor(&mut self, executor: Arc<Executor>) {
        self.executor = Some(executor);
    }

    /// The LLM client, which the executor needs once a repository is known.
    ///
    /// Handed out rather than duplicated: the runner already owns it, and two `Arc`s
    /// to the same client is one fewer thing to keep in step.
    #[must_use]
    pub fn llm(&self) -> Arc<dyn LlmPort> {
        Arc::clone(&self.llm)
    }

    /// Whether a repository has been resolved.
    #[must_use]
    pub fn has_executor(&self) -> bool {
        self.executor.is_some()
    }

    /// Whether anything is running or waiting.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        !self.running.is_empty() || !self.queue.is_empty()
    }

    /// Schedules a job, returning its id.
    ///
    /// A job in the same slot as one that is already running supersedes it: the older
    /// job is cancelled, which for a process job means its child is killed rather than
    /// left to finish into a result nobody will read.
    pub fn submit(&mut self, job: Job) -> u64 {
        self.submit_owned(JobOwner::Global, job)
    }

    /// Schedules work owned by a review session.
    pub fn submit_owned(&mut self, owner: JobOwner, job: Job) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let slot = job.slot();

        if job.is_ordered_save() {
            // A write that is already running must finish before the latest queued
            // snapshot starts. Only a not-yet-started snapshot is safely coalesced.
            self.queue.retain(|(_, _, queued)| queued.slot() != slot);
        } else {
            self.cancel_slot(slot);
            self.queue.retain(|(_, _, queued)| queued.slot() != slot);
        }
        self.queue.push_back((id, owner, job));
        self.pump();
        id
    }

    /// Cancels the job in a slot, if there is one.
    fn cancel_slot(&mut self, slot: Slot) {
        for running in &mut self.running {
            if running.slot == slot {
                running.cancel.cancel();
            }
        }
    }

    /// Cancels everything, which is what quitting does.
    pub fn cancel_all(&mut self) {
        for running in &mut self.running {
            running.cancel.cancel();
        }
        self.queue.clear();
    }

    /// Stops ordinary work and gives durable snapshots and an analysis cache commit a
    /// bounded opportunity to finish before process exit (IR-14).
    ///
    /// Returns `false` when storage did not acknowledge every queued snapshot before
    /// the deadline. Callers may then restore the terminal without waiting forever.
    pub fn flush_persistence(&mut self, timeout: Duration) -> bool {
        for running in &mut self.running {
            if !matches!(
                running.slot,
                Slot::State | Slot::DraftSave | Slot::PlanSave | Slot::Analyze
            ) {
                running.cancel.cancel();
            }
        }
        self.queue
            .retain(|(_, _, job)| job.is_ordered_save() || matches!(job.slot(), Slot::Analyze));

        let deadline = Instant::now() + timeout;
        while self.has_pending_persistence() && Instant::now() < deadline {
            let _ = self.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        !self.has_pending_persistence()
    }

    /// Whether an ordered durable snapshot or cache-writing analysis is queued or running.
    fn has_pending_persistence(&self) -> bool {
        self.running.iter().any(|running| {
            matches!(
                running.slot,
                Slot::State | Slot::DraftSave | Slot::PlanSave | Slot::Analyze
            )
        }) || self
            .queue
            .iter()
            .any(|(_, _, job)| job.is_ordered_save() || matches!(job.slot(), Slot::Analyze))
    }

    /// Whether a slot has a job running or waiting.
    #[must_use]
    pub fn is_busy_in(&self, slot: Slot) -> bool {
        self.running.iter().any(|running| running.slot == slot)
            || self.queue.iter().any(|(_, _, job)| job.slot() == slot)
    }

    /// Whether a submitted job is waiting for capacity rather than running.
    #[must_use]
    pub fn is_queued(&self, id: u64) -> bool {
        self.queue.iter().any(|(queued, _, _)| *queued == id)
    }

    /// Cancels every job in a slot, for `Esc` on the screen that owns it.
    pub fn cancel(&mut self, slot: Slot) {
        self.cancel_slot(slot);
        let queued = std::mem::take(&mut self.queue);
        for (id, owner, job) in queued {
            if job.slot() == slot {
                // Queued work owns no worker slot, but its pane still needs the same
                // cancellation acknowledgement as a running job. Otherwise it would
                // remain "cancelling" forever because no worker can report back.
                let _ = self.sender.send(Completion {
                    job: id,
                    progress_through: 0,
                    owner,
                    outcome: Outcome::Abandoned,
                });
            } else {
                self.queue.push_back((id, owner, job));
            }
        }
    }

    /// Starts queued jobs while there is room, and collects finished ones.
    ///
    /// Returns the completions that arrived, in arrival order. It never blocks.
    pub fn poll(&mut self) -> Vec<Completion> {
        while let Ok(completion) = self.receiver.try_recv() {
            self.running.retain(|running| running.id != completion.job);
            self.advance_queued_plan_save(&completion);
            self.pending_completions.push_back(completion);
        }

        let mut completions = Vec::new();
        let pending = std::mem::take(&mut self.pending_completions);
        for completion in pending {
            let delivered = self
                .delivered_progress
                .get(&completion.job)
                .copied()
                .unwrap_or_default();
            if delivered >= completion.progress_through {
                self.delivered_progress.remove(&completion.job);
                completions.push(completion);
            } else {
                self.pending_completions.push_back(completion);
            }
        }

        // Every job sends exactly one completion, including one that panicked, so
        // the running list is exactly "jobs that have not answered yet".
        self.pump();
        completions
    }

    /// Chains a queued snapshot to the revision assigned to the write before it.
    ///
    /// Plan saves are coalesced while one write is running, but the queued immutable
    /// snapshot was captured before that write received its durable revision. Updating
    /// it here keeps optimistic concurrency intact without returning filesystem work to
    /// the event loop. The pull-request key prevents one document's revision from being
    /// applied to another queued save.
    fn advance_queued_plan_save(&mut self, completion: &Completion) {
        let Outcome::PlanSaved {
            pr,
            document_revision,
        } = &completion.outcome
        else {
            return;
        };
        for (_, _, job) in &mut self.queue {
            if let Job::SavePlan {
                pr: queued_pr,
                plan,
            } = job
                && queued_pr == pr
            {
                plan.document_revision = plan.document_revision.max(*document_revision);
            }
        }
    }

    /// Everything the running jobs have said since the last call.
    ///
    /// Bounded: a provider that streams faster than the interface draws cannot grow
    /// the queue without limit or make this poll loop drain forever. Messages left in
    /// the channel retain their order for the next frame (IR-08).
    pub fn poll_progress(&mut self) -> Vec<Progress> {
        let mut progress = Vec::new();
        while progress.len() < MAX_PROGRESS_MESSAGES {
            let Ok(update) = self.progress_receiver.try_recv() else {
                break;
            };
            self.delivered_progress.insert(update.job, update.sequence);
            progress.push(update);
        }
        progress
    }

    /// Starts as many queued jobs as there is room for.
    fn pump(&mut self) {
        while self.running.len() < MAX_IN_FLIGHT {
            // An ordered snapshot must wait for the previous write of *that document
            // class* to acknowledge. Other jobs may still use a free worker; blocking
            // the whole queue here would turn a slow disk into a frozen application.
            let Some(index) = self.queue.iter().position(|(_, _, job)| {
                !job.is_ordered_save()
                    || !self
                        .running
                        .iter()
                        .any(|running| running.slot == job.slot())
            }) else {
                return;
            };
            let Some((id, owner, job)) = self.queue.remove(index) else {
                return;
            };
            self.start(id, owner, job);
        }
    }

    /// Runs one job on a worker thread.
    fn start(&mut self, id: u64, owner: JobOwner, job: Job) {
        let slot = job.slot();
        let kind = job.kind();
        // Every job gets a flag; a report simply never blocks long enough for it to
        // matter, and giving one class of job no handle would mean a special case in
        // the code that cancels.
        let cancel = Cancel::new();
        let worker_cancel = cancel.clone();
        let sender = self.sender.clone();
        let progress_sender = self.progress_sender.clone();
        let executor = self.executor.clone();
        let workspace = Arc::clone(&self.workspace);
        let probe = Arc::clone(&self.probe);
        let catalog = Arc::clone(&self.catalog);
        let llm = Arc::clone(&self.llm);
        let state_store = Arc::clone(&self.state_store);
        let request = self.request.clone();
        let worker_owner = owner.clone();
        let progress_sequence = Arc::new(AtomicU64::new(0));
        let worker_progress_sequence = Arc::clone(&progress_sequence);

        let spawned = std::thread::Builder::new()
            .name(format!("smart-review-job-{id}"))
            .spawn(move || {
                let started = Instant::now();
                // A job that unwinds would never send a completion, and its slot
                // would be occupied for the rest of the session: four such workers
                // and nothing is ever fetched again, with nothing on screen to say
                // why. Catching it here means every job sends exactly one answer.
                let ports = JobPorts {
                    workspace: workspace.as_ref(),
                    probe: probe.as_ref(),
                    catalog: catalog.as_ref(),
                    llm: llm.as_ref(),
                    state_store: state_store.as_ref(),
                    executor: executor.as_deref(),
                    request: &request,
                };
                let sink = ProgressSink {
                    sender: &progress_sender,
                    job: id,
                    owner: worker_owner.clone(),
                    cancel: &worker_cancel,
                    sequence: worker_progress_sequence,
                };
                let body =
                    std::panic::AssertUnwindSafe(|| run_job(&job, &ports, &worker_cancel, &sink));
                let outcome = std::panic::catch_unwind(body).unwrap_or_else(|_| {
                    logging::log(
                        Level::Error,
                        format!("job {id} panicked; reporting it as a failure"),
                    );
                    Outcome::Failed("the job crashed; this is a bug".to_owned())
                });

                // A cancelled job reports itself as abandoned rather than as a
                // failure: the user asked for it to stop, which is not an error.
                let outcome = if worker_cancel.is_cancelled() {
                    match outcome {
                        Outcome::EnvironmentFailed(_) | Outcome::Failed(_) => Outcome::Abandoned,
                        other => other,
                    }
                } else {
                    outcome
                };

                logging::log(
                    Level::Info,
                    format!(
                        "job id={id} kind={kind} duration_ms={} result={}{}",
                        started.elapsed().as_millis(),
                        outcome.result_label(),
                        outcome.count_label()
                    ),
                );

                let _ = sender.send(Completion {
                    job: id,
                    progress_through: progress_sequence.load(Ordering::Acquire),
                    owner: worker_owner,
                    outcome,
                });
            });

        match spawned {
            Ok(_handle) => {
                // The handle is deliberately dropped: the completion channel is what
                // says a job finished, and joining here would block the event loop.
                self.running.push(Running { id, slot, cancel });
            }
            Err(error) => {
                logging::log(
                    Level::Error,
                    format!("could not start a worker thread: {error}"),
                );
                let _ = self.sender.send(Completion {
                    job: id,
                    progress_through: 0,
                    owner,
                    outcome: Outcome::Failed(format!("could not start a worker thread: {error}")),
                });
            }
        }
    }
}

/// Where a streaming job reports what it is doing, and which job it is (FR-4.4).
struct ProgressSink<'a> {
    sender: &'a SyncSender<Progress>,
    job: u64,
    owner: JobOwner,
    cancel: &'a Cancel,
    sequence: Arc<AtomicU64>,
}

impl ProgressSink<'_> {
    /// Sends an update in order, applying backpressure rather than dropping text.
    ///
    /// A cancellation unblocks a producer waiting on a full queue. Completion travels
    /// over a separate unbounded channel, so it can never be stranded behind preview
    /// traffic (IR-08).
    fn send(&self, update: ProgressUpdate) {
        // Advance the completion watermark only after the message entered the channel.
        // If cancellation wins while this producer is backpressured, an allocated but
        // unsent sequence must not leave its completion waiting for a delta that the UI
        // can never receive.
        let sequence = self.sequence.load(Ordering::Acquire) + 1;
        let mut progress = Progress {
            job: self.job,
            sequence,
            owner: self.owner.clone(),
            update,
        };
        loop {
            match self.sender.try_send(progress) {
                Ok(()) => {
                    self.sequence.store(sequence, Ordering::Release);
                    return;
                }
                Err(TrySendError::Disconnected(_)) => return,
                Err(TrySendError::Full(queued)) => {
                    if self.cancel.is_cancelled() {
                        return;
                    }
                    progress = queued;
                    std::thread::sleep(PROGRESS_BACKPRESSURE_POLL);
                }
            }
        }
    }
}

/// The ports one job may need.
///
/// Borrowed rather than owned so a job can be run without cloning anything: the
/// worker thread already owns the `Arc`s, and this is the view it passes down.
struct JobPorts<'a> {
    workspace: &'a dyn WorkspacePort,
    probe: &'a dyn ForgeProbe,
    catalog: &'a dyn ModelCatalogPort,
    llm: &'a dyn LlmPort,
    state_store: &'a dyn StateStore,
    executor: Option<&'a Executor>,
    request: &'a DetectRequest,
}

/// Runs a job, whatever kind it is.
///
/// Separate from the thread that runs it, so the thread plumbing (`start`) and the
/// work (`run_job`) can be read and tested apart.
fn run_job(job: &Job, ports: &JobPorts<'_>, cancel: &Cancel, sink: &ProgressSink<'_>) -> Outcome {
    match job {
        Job::SaveState { revision, state } => match ports.state_store.save(state) {
            Ok(()) => Outcome::StateSaved {
                revision: *revision,
            },
            Err(error) => Outcome::Failed(format!("could not save state: {error}")),
        },
        Job::ValidateContextPath {
            workspace,
            head_sha,
            path,
        } => Outcome::ContextPathValidated {
            path: path.clone(),
            exists: ports
                .workspace
                .read_file(workspace, head_sha, path, cancel)
                .is_ok(),
        },
        Job::LoadDraft { pr } => match ports.executor {
            Some(executor) => match executor.drafts.load(&executor.repo, *pr) {
                Ok(draft) => Outcome::DraftLoaded {
                    pr: *pr,
                    draft: draft.map(Box::new),
                },
                Err(error) => Outcome::Failed(format!("could not load the draft: {error}")),
            },
            None => Outcome::Failed(
                "the repository is not known yet; run :doctor to see why".to_owned(),
            ),
        },
        Job::SaveDraft {
            draft,
            revision,
            reload_after,
        } => match ports.executor {
            Some(executor) => match crate::application::drafts::Drafts::new(
                Arc::clone(&executor.drafts),
                executor.repo.clone(),
            )
            .save(draft)
            {
                Ok(()) => Outcome::DraftSaved {
                    revision: *revision,
                    reload_after: *reload_after,
                },
                Err(error) => Outcome::Failed(format!("the draft could not be saved: {error}")),
            },
            None => Outcome::Failed(
                "the repository is not known yet; run :doctor to see why".to_owned(),
            ),
        },
        Job::DeleteDraft { pr, revision } => match ports.executor {
            Some(executor) => match executor.drafts.remove(&executor.repo, *pr) {
                Ok(()) => Outcome::DraftDeleted {
                    revision: *revision,
                },
                Err(error) => Outcome::Failed(format!("the draft could not be removed: {error}")),
            },
            None => Outcome::Failed(
                "the repository is not known yet; run :doctor to see why".to_owned(),
            ),
        },
        Job::Detect => match detect(ports.workspace, ports.probe, ports.request, cancel) {
            Ok(environment) => Outcome::Environment(Box::new(environment)),
            Err(error) => Outcome::EnvironmentFailed(Box::new(error)),
        },
        Job::Report { context } => Outcome::Checks(doctor::collect(context)),
        Job::Catalog { policy } => match ports.catalog.load(*policy, cancel) {
            Ok(load) => Outcome::Catalog(Box::new(load)),
            Err(error) => Outcome::Failed(error.to_string()),
        },
        Job::CleanWorkspaces { all, max_age_secs } => {
            clean_workspaces(ports.workspace, *all, *max_age_secs, cancel)
        }
        Job::Workspace { request } => match ports.workspace.ensure(request, cancel) {
            Ok(workspace) => Outcome::Workspace(Box::new(workspace)),
            Err(error) => Outcome::Failed(error.to_string()),
        },
        Job::ModelCheck { request } => match ports.llm.complete(request, cancel) {
            Ok(outcome) => Outcome::ModelChecked(Box::new(outcome)),
            Err(error) => Outcome::Failed(error.to_string()),
        },
        other => match ports.executor {
            Some(executor) => executor.run(other, cancel, sink),
            None => Outcome::Failed(
                "the repository is not known yet; run :doctor to see why".to_owned(),
            ),
        },
    }
}

/// Removes managed worktrees (`:workspace clean`, FR-3.1).
fn clean_workspaces(
    workspace: &dyn WorkspacePort,
    all: bool,
    max_age_secs: u64,
    cancel: &Cancel,
) -> Outcome {
    let entries = match workspace.list() {
        Ok(entries) => entries,
        Err(error) => return Outcome::Failed(error.to_string()),
    };
    let total = entries.len();
    let mut removed = 0;
    let mut failed = Vec::new();
    for entry in entries {
        let old_enough = entry.age_secs.is_none_or(|age| age >= max_age_secs);
        if !all && !old_enough {
            continue;
        }
        match (&entry.repo, &entry.legacy_cleanup) {
            (Some(repo), _) => match workspace.remove(repo, entry.number, cancel) {
                Ok(()) => removed += 1,
                Err(error) => failed.push(format!("pr-{}: {error}", entry.number)),
            },
            (None, Some(instruction)) => failed.push(instruction.clone()),
            (None, None) => failed.push(format!(
                "pr-{}: legacy workspace at {} cannot be removed automatically",
                entry.number,
                entry.path.display()
            )),
        }
    }
    Outcome::WorkspacesCleaned {
        removed,
        kept: total.saturating_sub(removed + failed.len()),
        failed,
    }
}

/// Turns an effect into the job it asks for, if it asks for one.
///
/// Keeping this mapping here rather than in the reducer is what lets the reducer stay
/// a pure function of state: it says *what* it wants, and this decides *how*.
#[must_use]
pub fn job_for(
    effect: &Effect,
    list: &PrListState,
    context: Context,
    draft: &crate::domain::draft::Draft,
) -> Option<Job> {
    match effect {
        Effect::DetectEnvironment => Some(Job::Detect),
        Effect::LoadPullRequests => Some(Job::List {
            query: list.query(),
        }),
        Effect::LoadMore => Some(Job::List {
            query: list
                .load_more_limit()
                .map_or_else(|| list.query(), |limit| list.query().with_more(limit)),
        }),
        Effect::CountPullRequests => Some(Job::Count {
            query: list.query(),
        }),
        Effect::OpenPullRequest(number) => Some(Job::Detail { number: *number }),
        // The draft travels in the job: the review that is sent is the one the user
        // confirmed, whatever they type while it is in flight.
        Effect::PublishDraft => Some(Job::SubmitReview {
            draft: Box::new(draft.clone()),
        }),
        Effect::PostReply {
            number,
            comment_id,
            body,
        } => Some(Job::PostReply {
            number: *number,
            comment_id: *comment_id,
            body: body.clone(),
        }),
        Effect::PostConversation { number, body } => Some(Job::PostConversation {
            number: *number,
            body: body.clone(),
        }),
        Effect::ResolveThread {
            number,
            thread_id,
            resolved,
        } => Some(Job::ResolveThread {
            number: *number,
            thread_id: thread_id.clone(),
            resolved: *resolved,
        }),
        Effect::RunDoctor => Some(Job::Report {
            context: Box::new(context),
        }),
        Effect::LoadCatalog(policy) => Some(Job::Catalog { policy: *policy }),
        // A diff reload needs the head SHA, which the caller knows and this does not.
        Effect::ReloadDiff
        | Effect::ValidateContextPath(_)
        | Effect::LoadMutations
        | Effect::EnsureWorkspace(_)
        | Effect::CheckModel
        | Effect::ClearKey(_)
        | Effect::SaveKey { .. }
        | Effect::SaveSelection(_)
        | Effect::CleanWorkspaces(_)
        | Effect::LoadAnalysis
        | Effect::LoadAnalysisAndDraft
        | Effect::GatherContext(_)
        | Effect::RunAnalysis { .. }
        | Effect::CancelAnalysis
        | Effect::SavePlan(_)
        | Effect::LoadChat
        | Effect::NewChat
        | Effect::OpenChat(_)
        | Effect::AskChat
        | Effect::CancelChat
        | Effect::RetryChat
        | Effect::ExportChat(_)
        | Effect::PruneChat
        | Effect::SaveContextFiles
        | Effect::LoadDraft
        | Effect::SaveDraft
        | Effect::SaveDraftAndReload
        | Effect::ClearDraft
        | Effect::CancelPublish
        | Effect::WriteDryRun
        | Effect::ListDrafts
        | Effect::ExportDraft(_)
        | Effect::ListWorktrees
        | Effect::CancelInFlight
        | Effect::SetMouse(_)
        | Effect::None
        | Effect::KeepPending
        | Effect::SaveState
        | Effect::CopyPath(_)
        | Effect::OpenUrl(_)
        | Effect::EditComposer(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ir_07_mutation_ids_differ_across_processes_started_in_the_same_second() {
        assert_ne!(
            mutation_operation_id(100, 41, 1),
            mutation_operation_id(100, 42, 1)
        );
    }
    use crate::domain::pr::{CheckRun, CheckSummary, PrState, PullRequestSummary};
    use crate::ports::forge::ForgeCapabilities;
    use crate::ports::workspace::RepoInfo;
    use crate::test_support::InMemoryCache;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    fn fake_workspace() -> crate::test_support::FakeWorkspace {
        crate::test_support::FakeWorkspace::new(RepoInfo {
            root: Some(std::path::PathBuf::from("/src/service")),
            remotes: vec![crate::ports::workspace::Remote {
                name: "origin".to_owned(),
                url: "git@github.com:acme/service.git".to_owned(),
            }],
            default_branch: Some("main".to_owned()),
            git_version: "2.43.0".to_owned(),
        })
    }

    #[derive(Debug)]
    struct FakeProbe {
        delay: Duration,
    }

    impl ForgeProbe for FakeProbe {
        fn probe(&self, cancel: &Cancel) -> Result<crate::ports::forge::ForgeStatus, String> {
            let waited = Duration::from_millis(5);
            let mut waited_total = Duration::ZERO;
            while waited_total < self.delay {
                if cancel.is_cancelled() {
                    return Err("cancelled".to_owned());
                }
                std::thread::sleep(waited);
                waited_total += waited;
            }
            Ok(crate::ports::forge::ForgeStatus::Ready(
                crate::domain::environment::GhInstall {
                    path: std::path::PathBuf::from("/usr/bin/gh"),
                    version: "2.45.0".to_owned(),
                    account: Some("bruno".to_owned()),
                    scopes: vec!["repo".to_owned()],
                },
            ))
        }
    }

    /// A forge that answers slowly, so a job can be observed while it runs.
    #[derive(Debug)]
    struct SlowForge {
        delay: Duration,
        calls: AtomicU64,
        cancelled: Mutex<u32>,
        page: Mutex<Option<PullRequestPage>>,
    }

    impl SlowForge {
        fn new(delay: Duration) -> Self {
            let page = PullRequestPage::complete(vec![summary(1)], 50);
            Self {
                delay,
                calls: AtomicU64::new(0),
                cancelled: Mutex::new(0),
                page: Mutex::new(Some(page)),
            }
        }

        fn calls(&self) -> u64 {
            self.calls.load(Ordering::SeqCst)
        }

        fn cancelled(&self) -> u32 {
            *self.cancelled.lock().unwrap()
        }
    }

    fn summary(number: u64) -> PullRequestSummary {
        PullRequestSummary {
            number,
            title: "x".to_owned(),
            author: "alice".to_owned(),
            state: PrState::Open,
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
            checks: CheckSummary::default(),
            url: String::new(),
            is_cross_repository: false,
        }
    }

    impl ForgePort for SlowForge {
        fn capabilities(&self) -> ForgeCapabilities {
            ForgeCapabilities::default()
        }

        fn reply_to_review_comment(
            &self,
            _number: u64,
            _comment_id: u64,
            _body: &str,
            _cancel: &Cancel,
        ) -> crate::Result<crate::ports::CommentPosted> {
            Err(crate::Error::forge("gh", "this fake does not post replies"))
        }

        fn comment_on_conversation(
            &self,
            _number: u64,
            _body: &str,
            _cancel: &Cancel,
        ) -> crate::Result<crate::ports::CommentPosted> {
            Err(crate::Error::forge(
                "gh",
                "this fake does not post comments",
            ))
        }

        fn set_thread_resolved(
            &self,
            _thread_id: &str,
            _resolved: bool,
            _cancel: &Cancel,
        ) -> crate::Result<()> {
            Err(crate::Error::forge("gh", "this fake cannot resolve"))
        }

        fn list_conversation(
            &self,
            _number: u64,
            _cancel: &Cancel,
        ) -> crate::Result<Vec<crate::domain::pr::ConversationComment>> {
            Ok(Vec::new())
        }

        fn submit_review(
            &self,
            number: u64,
            _draft: &crate::domain::draft::Draft,
            _cancel: &Cancel,
        ) -> crate::Result<crate::ports::ReviewPosted> {
            Err(crate::Error::forge(
                format!("review #{number}"),
                "this fake does not publish",
            ))
        }

        fn list_pull_requests(
            &self,
            _query: &PrQuery,
            cancel: &Cancel,
        ) -> crate::Result<PullRequestPage> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut waited = Duration::ZERO;
            while waited < self.delay {
                if cancel.is_cancelled() {
                    *self.cancelled.lock().unwrap() += 1;
                    return Err(crate::Error::forge("gh", "cancelled"));
                }
                std::thread::sleep(Duration::from_millis(5));
                waited += Duration::from_millis(5);
            }
            Ok(self.page.lock().unwrap().clone().unwrap_or_default())
        }

        fn count_pull_requests(&self, _query: &PrQuery, _cancel: &Cancel) -> crate::Result<u32> {
            Ok(0)
        }

        fn get_pull_request(
            &self,
            _number: u64,
            _cancel: &Cancel,
        ) -> crate::Result<PullRequestDetail> {
            Err(crate::Error::forge("gh", "not needed"))
        }

        fn list_reviews(
            &self,
            _number: u64,
            _cancel: &Cancel,
        ) -> crate::Result<Vec<crate::domain::pr::Review>> {
            Ok(Vec::new())
        }

        fn list_review_comments(
            &self,
            _number: u64,
            _cancel: &Cancel,
        ) -> crate::Result<Vec<crate::domain::pr::ReviewComment>> {
            Ok(Vec::new())
        }

        fn list_checks(&self, _number: u64, _cancel: &Cancel) -> crate::Result<Vec<CheckRun>> {
            Ok(Vec::new())
        }

        fn pull_request_diff(&self, _number: u64, _cancel: &Cancel) -> crate::Result<String> {
            Err(crate::Error::forge("gh", "not needed"))
        }
    }

    #[derive(Debug, Default)]
    struct FakeClock;

    impl Clock for FakeClock {
        fn now_unix_secs(&self) -> u64 {
            1_000
        }
    }

    /// A state store that holds its first write open, making save ordering observable.
    #[derive(Debug, Default)]
    struct HoldingStateStore {
        entered: std::sync::atomic::AtomicBool,
        release: std::sync::atomic::AtomicBool,
        values: Mutex<Vec<AppState>>,
    }

    impl StateStore for HoldingStateStore {
        fn load(&self) -> crate::Result<AppState> {
            Ok(self
                .values
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .last()
                .cloned()
                .unwrap_or_default())
        }

        fn save(&self, value: &AppState) -> crate::Result<()> {
            self.entered.store(true, Ordering::Release);
            while !self.release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.values
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(value.clone());
            Ok(())
        }
    }

    /// An analysis store that holds its first plan write so shutdown ordering is visible.
    #[derive(Debug, Default)]
    struct HoldingAnalysisStore {
        inner: crate::test_support::InMemoryAnalysis,
        entered: std::sync::atomic::AtomicBool,
        release: std::sync::atomic::AtomicBool,
        writes: AtomicU64,
    }

    impl AnalysisCachePort for HoldingAnalysisStore {
        fn get(
            &self,
            key: &AnalysisKey,
        ) -> Result<Option<StoredAnalysis>, crate::ports::AnalysisCacheError> {
            self.inner.get(key)
        }

        fn list(
            &self,
            repo: &RepoId,
            pr: u64,
        ) -> Result<Vec<StoredAnalysis>, crate::ports::AnalysisCacheError> {
            self.inner.list(repo, pr)
        }

        fn put(&self, stored: &StoredAnalysis) -> Result<(), crate::ports::AnalysisCacheError> {
            self.inner.put(stored)
        }

        fn plan(
            &self,
            repo: &RepoId,
            pr: u64,
        ) -> Result<Option<crate::domain::plan::Plan>, crate::ports::AnalysisCacheError> {
            self.inner.plan(repo, pr)
        }

        fn put_plan(
            &self,
            repo: &RepoId,
            pr: u64,
            plan: &crate::domain::plan::Plan,
        ) -> Result<crate::domain::plan::Plan, crate::ports::AnalysisCacheError> {
            if self.writes.fetch_add(1, Ordering::AcqRel) == 0 {
                self.entered.store(true, Ordering::Release);
                while !self.release.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            let current_revision = self
                .inner
                .plan(repo, pr)?
                .map_or(0, |stored| stored.document_revision);
            if current_revision != plan.document_revision {
                return Err(crate::ports::AnalysisCacheError::Conflict);
            }
            let mut saved = plan.clone();
            saved.document_revision = current_revision.saturating_add(1);
            self.inner.put_plan(repo, pr, &saved)?;
            Ok(saved)
        }
    }

    fn runner_with(delay: Duration) -> (JobRunner, Arc<SlowForge>) {
        runner_with_analysis(
            delay,
            Arc::new(crate::test_support::InMemoryAnalysis::default()),
        )
    }

    fn runner_with_analysis(
        delay: Duration,
        analysis: Arc<dyn AnalysisCachePort>,
    ) -> (JobRunner, Arc<SlowForge>) {
        let mut runner = crate::test_support::test_job_runner(
            Arc::new(fake_workspace()),
            Arc::new(FakeProbe {
                delay: Duration::ZERO,
            }),
            DetectRequest::default(),
        );
        let forge = Arc::new(SlowForge::new(delay));
        runner.set_executor(Arc::new(Executor::new(ExecutorPorts {
            chat: Arc::new(crate::test_support::FakeChatStore::default()),
            drafts: Arc::new(crate::test_support::FakeDraftStore::default()),
            mutations: Arc::new(crate::test_support::FakeMutationStore::default()),
            forge: Arc::clone(&forge) as Arc<dyn ForgePort>,
            cache: Arc::new(InMemoryCache::default()),
            clock: Arc::new(FakeClock),
            workspace: Arc::new(fake_workspace()),
            analysis,
            llm: Arc::new(crate::test_support::NoLlm),
            repo: RepoId::parse("acme/service").unwrap(),
            policy: crate::application::prs::CachePolicy::default(),
            dry_run: false,
        })));
        (runner, forge)
    }

    /// Waits for the first completion, giving up after a generous timeout.
    fn wait_for_completion(runner: &mut JobRunner) -> Vec<Completion> {
        wait_for(runner, |completions| !completions.is_empty())
    }

    /// Waits until `job` has answered, collecting everything that arrived first.
    fn wait_for_job(runner: &mut JobRunner, job: u64) -> Vec<Completion> {
        wait_for(runner, |completions| {
            completions.iter().any(|completion| completion.job == job)
        })
    }

    /// Polls until `done` holds for everything collected so far.
    fn wait_for(runner: &mut JobRunner, done: impl Fn(&[Completion]) -> bool) -> Vec<Completion> {
        let mut collected = Vec::new();
        for _ in 0..2000 {
            collected.extend(runner.poll());
            if done(&collected) {
                return collected;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        collected
    }

    #[test]
    fn a_detection_job_reports_the_environment() {
        let (mut runner, _forge) = runner_with(Duration::ZERO);
        let id = runner.submit(Job::Detect);
        let completions = wait_for_completion(&mut runner);

        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].job, id);
        match &completions[0].outcome {
            Outcome::Environment(environment) => {
                assert_eq!(environment.repo.slug(), "acme/service");
                assert_eq!(environment.gh.account.as_deref(), Some("bruno"));
            }
            other => panic!("expected an environment, got {other:?}"),
        }
        assert!(!runner.is_busy());
    }

    #[test]
    fn a_list_job_fetches_a_page() {
        let (mut runner, forge) = runner_with(Duration::ZERO);
        runner.submit(Job::List {
            query: PrQuery::default(),
        });
        let completions = wait_for_completion(&mut runner);

        match &completions[0].outcome {
            Outcome::Page(outcome) => assert_eq!(outcome.value().items.len(), 1),
            other => panic!("expected a page, got {other:?}"),
        }
        assert_eq!(forge.calls(), 1);
    }

    #[test]
    fn a_failing_job_comes_back_as_a_message_rather_than_a_panic() {
        let mut runner = crate::test_support::test_job_runner(
            Arc::new(fake_workspace()),
            Arc::new(FakeProbe {
                delay: Duration::ZERO,
            }),
            DetectRequest::default(),
        );
        // No executor: a repository-scoped job cannot run.
        runner.submit(Job::List {
            query: PrQuery::default(),
        });
        let completions = wait_for_completion(&mut runner);
        match &completions[0].outcome {
            Outcome::Failed(message) => assert!(message.contains("not known yet"), "{message}"),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_second_job_in_a_slot_supersedes_the_first() {
        let (mut runner, forge) = runner_with(Duration::from_millis(300));
        runner.submit(Job::List {
            query: PrQuery::default(),
        });
        std::thread::sleep(Duration::from_millis(30));
        // The second request cancels the first rather than waiting for it.
        let second = runner.submit(Job::List {
            query: PrQuery::default(),
        });

        let completions = wait_for_job(&mut runner, second);
        assert_eq!(forge.cancelled(), 1, "the first job was cancelled");
        assert!(
            completions
                .iter()
                .any(|completion| completion.job == second),
            "the newest job still answers"
        );
        assert!(
            !completions.iter().any(|completion| {
                matches!(completion.outcome, Outcome::Page(_)) && completion.job != second
            }),
            "a superseded page must not come back: {completions:?}"
        );
    }

    #[test]
    fn jobs_in_different_slots_run_together() {
        let (mut runner, _forge) = runner_with(Duration::from_millis(60));
        runner.submit(Job::List {
            query: PrQuery::default(),
        });
        runner.submit(Job::Count {
            query: PrQuery::default(),
        });
        assert!(runner.is_busy());

        let started = std::time::Instant::now();
        let _ = wait_for_completion(&mut runner);
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "they should overlap, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn cancelling_a_slot_stops_its_work() {
        let (mut runner, forge) = runner_with(Duration::from_millis(200));
        runner.submit(Job::List {
            query: PrQuery::default(),
        });
        std::thread::sleep(Duration::from_millis(20));
        runner.cancel(Slot::List);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(forge.cancelled(), 1);
    }

    #[test]
    fn ir_08_cancelling_queued_work_acknowledges_without_a_worker() {
        let (mut runner, _forge) = runner_with(Duration::ZERO);
        let id = runner.next_id;
        runner.next_id += 1;
        runner.queue.push_back((
            id,
            JobOwner::Global,
            Job::Report {
                context: Box::new(context()),
            },
        ));

        runner.cancel(Slot::Doctor);
        let completions = runner.poll();
        assert!(
            completions.iter().any(|completion| {
                completion.job == id && matches!(completion.outcome, Outcome::Abandoned)
            }),
            "a queued cancellation must still release its pane: {completions:?}"
        );
        assert!(!runner.is_busy_in(Slot::Doctor));
    }

    #[test]
    fn cancelling_everything_clears_the_queue_and_the_workers() {
        let (mut runner, _forge) = runner_with(Duration::from_millis(200));
        for index in 0..4 {
            runner.submit(Job::List {
                query: PrQuery::default(),
            });
            runner.submit(Job::Count {
                query: PrQuery::default(),
            });
            let _ = index;
        }
        runner.cancel_all();
        assert!(runner.queue.is_empty(), "nothing is left waiting");

        // The workers that were already running finish as abandoned rather than as
        // failures: the user asked them to stop, which is not an error.
        let mut collected = Vec::new();
        for _ in 0..200 {
            collected.extend(runner.poll());
            if !runner.is_busy() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!runner.is_busy(), "everything stopped: {collected:?}");
        assert!(
            collected
                .iter()
                .all(|completion| !matches!(completion.outcome, Outcome::Failed(_))),
            "{collected:?}"
        );
    }

    #[test]
    fn ir_08_progress_is_lossless_bounded_and_precedes_its_completion() {
        let (mut runner, _forge) = runner_with(Duration::ZERO);
        let total = MAX_QUEUED_PROGRESS;
        for index in 1..=total {
            let sent = runner.progress_sender.send(Progress {
                sequence: u64::try_from(index).unwrap_or(u64::MAX),
                job: 17,
                owner: JobOwner::Global,
                update: ProgressUpdate::Analysis(AnalysisProgress::Delta(index.to_string())),
            });
            assert!(sent.is_ok());
        }
        let sent = runner.sender.send(Completion {
            job: 17,
            progress_through: u64::try_from(total).unwrap_or(u64::MAX),
            owner: JobOwner::Global,
            outcome: Outcome::Abandoned,
        });
        assert!(sent.is_ok());

        let mut progress = Vec::new();
        for batch_number in 0..(total / MAX_PROGRESS_MESSAGES) {
            let batch = runner.poll_progress();
            assert_eq!(batch.len(), MAX_PROGRESS_MESSAGES);
            progress.extend(batch);
            let completions = runner.poll();
            if batch_number + 1 == total / MAX_PROGRESS_MESSAGES {
                assert_eq!(completions.len(), 1, "completion follows every delta");
            } else {
                assert!(
                    completions.is_empty(),
                    "completion cannot overtake queued progress: {completions:?}"
                );
            }
        }
        let labels: Vec<String> = progress
            .into_iter()
            .map(|progress| match progress.update {
                ProgressUpdate::Analysis(AnalysisProgress::Delta(delta)) => delta,
                other => panic!("expected analysis delta, got {other:?}"),
            })
            .collect();
        assert_eq!(
            labels,
            (1..=total)
                .map(|index| index.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn ir_08_a_full_progress_queue_backpressures_the_producer() {
        let (mut runner, _forge) = runner_with(Duration::ZERO);
        for index in 0..MAX_QUEUED_PROGRESS {
            let sent = runner.progress_sender.send(Progress {
                sequence: u64::try_from(index + 1).unwrap_or(u64::MAX),
                job: 17,
                owner: JobOwner::Global,
                update: ProgressUpdate::Analysis(AnalysisProgress::Delta(index.to_string())),
            });
            assert!(sent.is_ok());
        }

        let sender = runner.progress_sender.clone();
        let cancel = Cancel::new();
        let sequence = Arc::new(AtomicU64::new(
            u64::try_from(MAX_QUEUED_PROGRESS).unwrap_or(u64::MAX),
        ));
        let (started_sender, started_receiver) = mpsc::channel();
        let (finished_sender, finished_receiver) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _ = started_sender.send(());
                ProgressSink {
                    sender: &sender,
                    job: 17,
                    owner: JobOwner::Global,
                    cancel: &cancel,
                    sequence,
                }
                .send(ProgressUpdate::Analysis(AnalysisProgress::Delta(
                    "blocked".to_owned(),
                )));
                let _ = finished_sender.send(());
            });
            assert!(
                started_receiver
                    .recv_timeout(Duration::from_secs(1))
                    .is_ok()
            );
            assert!(
                finished_receiver.try_recv().is_err(),
                "the producer must wait"
            );

            let drained = runner.poll_progress();
            assert_eq!(drained.len(), MAX_PROGRESS_MESSAGES);
            assert!(
                finished_receiver
                    .recv_timeout(Duration::from_secs(1))
                    .is_ok(),
                "making capacity available releases the producer"
            );
        });
    }

    #[test]
    fn no_more_than_the_limit_run_at_once() {
        let (mut runner, _forge) = runner_with(Duration::from_millis(40));
        // Nine slots' worth: four run, the rest wait.
        runner.submit(Job::List {
            query: PrQuery::default(),
        });
        runner.submit(Job::Count {
            query: PrQuery::default(),
        });
        runner.submit(Job::Detail { number: 1 });
        runner.submit(Job::Patch {
            number: 1,
            head_sha: "abc".to_owned(),
            options: DiffOptions::default(),
            view: Box::new(ViewContext::default()),
        });
        runner.submit(Job::Report {
            context: Box::new(context()),
        });
        assert!(
            runner.running.len() <= MAX_IN_FLIGHT,
            "{} jobs running",
            runner.running.len()
        );
        assert_eq!(runner.queue.len(), 1, "the fifth waits its turn");

        let _ = wait_for_completion(&mut runner);
    }

    #[test]
    #[ignore = "opt-in IR-17 reference workload; run in release mode"]
    fn ir_17_reference_four_jobs_and_rapid_navigation_workload() {
        let (mut runner, forge) = runner_with(Duration::from_millis(25));
        runner.submit(Job::List {
            query: PrQuery::default(),
        });
        runner.submit(Job::Count {
            query: PrQuery::default(),
        });
        runner.submit(Job::Detail { number: 1 });
        runner.submit(Job::Patch {
            number: 1,
            head_sha: "abc".to_owned(),
            options: DiffOptions::default(),
            view: Box::new(ViewContext::default()),
        });
        assert_eq!(runner.running.len(), MAX_IN_FLIGHT);

        let started = Instant::now();
        let mut newest = 0;
        for _ in 0..20 {
            newest = runner.submit(Job::List {
                query: PrQuery::default(),
            });
        }
        let completions = wait_for_job(&mut runner, newest);
        let elapsed = started.elapsed();
        let stale_pages = completions
            .iter()
            .filter(|completion| {
                completion.job != newest && matches!(completion.outcome, Outcome::Page(_))
            })
            .count();
        eprintln!(
            "IR17_METRIC profile={} workload=four_jobs_rapid_navigation replacements=20 duration_ms={} cancelled={} stale_pages={stale_pages}",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            elapsed.as_millis(),
            forge.cancelled(),
        );
        assert_eq!(stale_pages, 0);
    }

    /// A job whose body panics, to prove the slot is not lost.
    #[derive(Debug)]
    struct PanicForge;

    impl ForgePort for PanicForge {
        fn capabilities(&self) -> ForgeCapabilities {
            ForgeCapabilities::default()
        }

        fn reply_to_review_comment(
            &self,
            _number: u64,
            _comment_id: u64,
            _body: &str,
            _cancel: &Cancel,
        ) -> crate::Result<crate::ports::CommentPosted> {
            Err(crate::Error::forge("gh", "this fake does not post replies"))
        }

        fn comment_on_conversation(
            &self,
            _number: u64,
            _body: &str,
            _cancel: &Cancel,
        ) -> crate::Result<crate::ports::CommentPosted> {
            Err(crate::Error::forge(
                "gh",
                "this fake does not post comments",
            ))
        }

        fn set_thread_resolved(
            &self,
            _thread_id: &str,
            _resolved: bool,
            _cancel: &Cancel,
        ) -> crate::Result<()> {
            Err(crate::Error::forge("gh", "this fake cannot resolve"))
        }

        fn list_conversation(
            &self,
            _number: u64,
            _cancel: &Cancel,
        ) -> crate::Result<Vec<crate::domain::pr::ConversationComment>> {
            Ok(Vec::new())
        }

        fn submit_review(
            &self,
            number: u64,
            _draft: &crate::domain::draft::Draft,
            _cancel: &Cancel,
        ) -> crate::Result<crate::ports::ReviewPosted> {
            Err(crate::Error::forge(
                format!("review #{number}"),
                "this fake does not publish",
            ))
        }

        fn list_pull_requests(&self, _q: &PrQuery, _c: &Cancel) -> crate::Result<PullRequestPage> {
            panic!("this forge panics on purpose");
        }

        fn count_pull_requests(&self, _q: &PrQuery, _c: &Cancel) -> crate::Result<u32> {
            Ok(0)
        }

        fn get_pull_request(&self, _n: u64, _c: &Cancel) -> crate::Result<PullRequestDetail> {
            Err(crate::Error::forge("gh", "not needed"))
        }

        fn list_reviews(
            &self,
            _n: u64,
            _c: &Cancel,
        ) -> crate::Result<Vec<crate::domain::pr::Review>> {
            Ok(Vec::new())
        }

        fn list_review_comments(
            &self,
            _n: u64,
            _c: &Cancel,
        ) -> crate::Result<Vec<crate::domain::pr::ReviewComment>> {
            Ok(Vec::new())
        }

        fn list_checks(&self, _n: u64, _c: &Cancel) -> crate::Result<Vec<CheckRun>> {
            Ok(Vec::new())
        }

        fn pull_request_diff(&self, _n: u64, _c: &Cancel) -> crate::Result<String> {
            Err(crate::Error::forge("gh", "not needed"))
        }
    }

    #[test]
    fn a_panicking_job_still_answers_and_frees_its_slot() {
        let mut runner = crate::test_support::test_job_runner(
            Arc::new(fake_workspace()),
            Arc::new(FakeProbe {
                delay: Duration::ZERO,
            }),
            DetectRequest::default(),
        );
        runner.set_executor(Arc::new(Executor::new(ExecutorPorts {
            chat: Arc::new(crate::test_support::FakeChatStore::default()),
            drafts: Arc::new(crate::test_support::FakeDraftStore::default()),
            mutations: Arc::new(crate::test_support::FakeMutationStore::default()),
            forge: Arc::new(PanicForge) as Arc<dyn ForgePort>,
            cache: Arc::new(InMemoryCache::default()),
            clock: Arc::new(FakeClock),
            workspace: Arc::new(fake_workspace()),
            analysis: Arc::new(crate::test_support::InMemoryAnalysis::default()),
            llm: Arc::new(crate::test_support::NoLlm),
            repo: RepoId::parse("acme/service").unwrap(),
            policy: crate::application::prs::CachePolicy::default(),
            dry_run: false,
        })));

        // Three panicking jobs in a row: if a panic lost its slot, the third submit
        // would never start and this would time out.
        for round in 1..=3 {
            runner.submit(Job::List {
                query: PrQuery::default(),
            });

            let mut collected = Vec::new();
            for _ in 0..400 {
                collected.extend(runner.poll());
                if !collected.is_empty() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }

            assert_eq!(collected.len(), 1, "round {round}: one answer per job");
            assert!(
                matches!(collected[0].outcome, Outcome::Failed(_)),
                "round {round}: the panic is reported rather than swallowed: {collected:?}"
            );
            assert!(!runner.is_busy(), "round {round}: the slot is free again");
        }
    }

    #[test]
    fn a_report_job_runs_without_a_repository() {
        let (mut runner, _forge) = runner_with(Duration::ZERO);
        runner.executor = None;
        runner.submit(Job::Report {
            context: Box::new(context()),
        });
        let completions = wait_for_completion(&mut runner);
        match &completions[0].outcome {
            Outcome::Checks(checks) => {
                assert!(
                    checks.iter().any(|check| check.name == "home"),
                    "the report should have run: {checks:?}"
                );
                assert!(
                    checks.iter().any(|check| check.name == "gh"),
                    "and it should have reported gh: {checks:?}"
                );
                assert!(
                    !checks
                        .iter()
                        .any(|check| check.name == "gh" && check.detail.contains("(authenticated)")),
                    "the probe must not reach the real gh: {checks:?}"
                );
            }
            other => panic!("expected a report, got {other:?}"),
        }
    }

    #[test]
    fn slots_are_one_of_each_kind() {
        assert_eq!(Job::Detect.slot(), Slot::Environment);
        assert_eq!(
            Job::List {
                query: PrQuery::default()
            }
            .slot(),
            Slot::List
        );
        assert_eq!(Job::Detail { number: 1 }.slot(), Slot::Detail);
        assert!(Job::Detail { number: 1 }.is_cancellable());
        assert!(
            !Job::Report {
                context: Box::new(context())
            }
            .is_cancellable()
        );
        let plan_save = Job::SavePlan {
            pr: 141,
            plan: Box::new(crate::domain::plan::Plan::heuristic(
                "head",
                &["src/a.rs".to_owned()],
            )),
        };
        assert_eq!(plan_save.slot(), Slot::PlanSave);
        assert!(plan_save.is_ordered_save());
    }

    #[test]
    fn ir_14_an_ordered_state_save_waits_for_the_previous_write() {
        let (mut runner, _) = runner_with(Duration::ZERO);
        let store = Arc::new(HoldingStateStore::default());
        runner.state_store = Arc::clone(&store) as Arc<dyn StateStore>;

        let first = runner.submit(Job::SaveState {
            revision: 1,
            state: Box::new(AppState {
                theme: Some("dark".to_owned()),
                ..AppState::default()
            }),
        });
        for _ in 0..100 {
            if store.entered.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(store.entered.load(Ordering::Acquire), "first save started");

        let second = runner.submit(Job::SaveState {
            revision: 2,
            state: Box::new(AppState {
                theme: Some("light".to_owned()),
                ..AppState::default()
            }),
        });
        assert_eq!(
            runner
                .running
                .iter()
                .filter(|running| running.slot == Slot::State)
                .count(),
            1,
            "the second snapshot stays queued until the first write acknowledges"
        );

        store.release.store(true, Ordering::Release);
        let completions = wait_for_job(&mut runner, second);
        assert!(
            completions.iter().any(|completion| completion.job == first),
            "the first save completed before the second"
        );
        assert_eq!(
            store.load().unwrap().theme.as_deref(),
            Some("light"),
            "the newer snapshot wins on disk"
        );
    }

    #[test]
    fn ir_16_shutdown_flushes_the_latest_plan_edit_queued_behind_a_held_write() {
        let store = Arc::new(HoldingAnalysisStore::default());
        let (mut runner, _) = runner_with_analysis(
            Duration::ZERO,
            Arc::clone(&store) as Arc<dyn AnalysisCachePort>,
        );
        let repo = RepoId::parse("acme/service").expect("valid repository");
        let first_plan = crate::domain::plan::Plan::heuristic("head", &["src/a.rs".to_owned()]);
        runner.submit(Job::SavePlan {
            pr: 141,
            plan: Box::new(first_plan.clone()),
        });
        for _ in 0..100 {
            if store.entered.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(store.entered.load(Ordering::Acquire), "first save started");

        let mut latest_plan = first_plan;
        latest_plan.overridden = true;
        runner.submit(Job::SavePlan {
            pr: 141,
            plan: Box::new(latest_plan),
        });
        assert!(
            runner
                .queue
                .iter()
                .any(|(_, _, job)| matches!(job, Job::SavePlan { pr: 141, .. })),
            "the latest snapshot is visible to the persistence queue before exit"
        );

        let release_store = Arc::clone(&store);
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            release_store.release.store(true, Ordering::Release);
        });
        assert!(
            runner.flush_persistence(Duration::from_secs(1)),
            "shutdown waits for both plan snapshots"
        );
        release.join().expect("release thread finished");

        let saved = store
            .plan(&repo, 141)
            .expect("plan store is readable")
            .expect("latest plan was stored");
        assert!(saved.overridden, "the second edit, not the first, wins");
        assert_eq!(saved.document_revision, 2);
        assert_eq!(store.writes.load(Ordering::Acquire), 2);
    }

    #[test]
    fn effects_map_to_the_jobs_they_ask_for() {
        let list = PrListState::new(50, 120);
        assert!(matches!(
            job_for(&Effect::DetectEnvironment, &list, context(), &draft()),
            Some(Job::Detect)
        ));
        assert!(matches!(
            job_for(&Effect::LoadPullRequests, &list, context(), &draft()),
            Some(Job::List { .. })
        ));
        assert!(matches!(
            job_for(&Effect::OpenPullRequest(141), &list, context(), &draft()),
            Some(Job::Detail { number: 141 })
        ));
        assert!(matches!(
            job_for(&Effect::RunDoctor, &list, context(), &draft()),
            Some(Job::Report { .. })
        ));
        assert!(job_for(&Effect::None, &list, context(), &draft()).is_none());
        assert!(
            job_for(
                &Effect::CopyPath("a".to_owned()),
                &list,
                context(),
                &draft()
            )
            .is_none()
        );

        // `:load-more` asks for a bigger page, not the same one again.
        let mut grown = PrListState::new(50, 120);
        grown.limit = 50;
        match job_for(&Effect::LoadMore, &grown, context(), &draft()) {
            Some(Job::List { query }) => assert_eq!(query.limit, 100),
            other => panic!("expected a list job, got {other:?}"),
        }
    }

    fn draft() -> crate::domain::draft::Draft {
        crate::domain::draft::Draft::new(141, crate::domain::time::from_unix_secs(0))
    }

    fn context() -> Context {
        let dir = crate::test_support::temp_home();
        let cli = crate::cli::Cli {
            repo: None,
            pr: None,
            path: None,
            remote: None,
            config: None,
            theme: None,
            home: Some(dir.path().to_path_buf()),
            log_level: None,
            check: false,
            dry_run: false,
        };
        let mut startup = crate::Startup::load(&cli).unwrap();
        // The report probes git and gh; pointing it at a binary that cannot exist
        // keeps the test off the network and independent of the machine's own gh
        // (AGENTS §8, NFR-5.2).
        startup.config.forge.gh_path = "smart-review-no-such-binary".to_owned();
        startup.doctor_context()
    }
}
