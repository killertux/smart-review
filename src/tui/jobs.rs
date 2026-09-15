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

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

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
use crate::ports::analysis::{AnalysisCachePort, AnalysisKey, StoredAnalysis};
use crate::ports::cache::CacheStore;
use crate::ports::catalog::{CatalogLoad, CatalogPolicy, ModelCatalogPort};
use crate::ports::forge::{ForgePort, ForgeProbe, PullRequestPage};
use crate::ports::llm::{ChatOutcome, ChatRequest, LlmPort};
use crate::ports::workspace::{
    DiffOptions, DiffRequest, Workspace, WorkspacePort, WorkspaceRequest,
};
use crate::ports::{Cancel, Clock};
use crate::tui::app::Effect;
use crate::tui::app::ReviewSession;
use crate::tui::list_view::PrListState;

/// How many progress messages are handed to the interface in one poll. Anything
/// beyond it is dropped: the answer arrives whole in the completion, so a dropped
/// preview costs nothing but a redrawn frame that is already stale.
pub const MAX_PROGRESS_MESSAGES: usize = 256;

/// How many jobs may run at once. Process jobs are the expensive ones, and four is
/// what ARCH-5 allows.
pub const MAX_IN_FLIGHT: usize = 4;

/// Which kind of work a job is, one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
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
}

/// What a job was asked to do.
#[derive(Debug, Clone)]
pub enum Job {
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
        /// The flags that produced it, also part of the cache key.
        options: DiffOptions,
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
        /// GitHub's thread id.
        thread_id: String,
        /// Which way.
        resolved: bool,
    },
}

impl Job {
    /// Which slot the job occupies.
    #[must_use]
    pub fn slot(&self) -> Slot {
        match self {
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
        }
    }

    /// Whether this job runs a process and can therefore be cancelled usefully.
    #[must_use]
    pub fn is_cancellable(&self) -> bool {
        !matches!(self, Self::Report { .. })
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
        /// The diff.
        outcome: Box<FetchOutcome<Patch>>,
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
    },
    /// The analysis finished, one way or another (FR-4.1).
    Analyzed(Box<AnalysisRun>),
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
    /// The job was replaced or abandoned before it finished.
    Abandoned,
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
    /// The provider (FR-4.1).
    pub llm: Arc<dyn LlmPort>,
    /// Which repository these are scoped to.
    pub repo: RepoId,
    /// How long the forge answers are reused.
    pub policy: crate::application::prs::CachePolicy,
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
    /// The provider, for the analysis request itself (FR-4.1).
    llm: Arc<dyn LlmPort>,
    repo: RepoId,
    policy: crate::application::prs::CachePolicy,
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
            llm,
            repo,
            policy,
        } = ports;
        Self {
            forge,
            cache,
            clock,
            workspace,
            analysis,
            chat,
            drafts,
            llm,
            repo,
            policy,
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
            Job::Patch { number, head_sha } => match prs.load_patch(*number, head_sha, cancel) {
                Ok(outcome) => Outcome::Patch {
                    outcome: Box::new(outcome),
                    source: DiffSource::Forge,
                    head_sha: head_sha.clone(),
                },
                Err(error) => Outcome::Failed(error.to_string()),
            },
            // A local diff is read from the worktree rather than from the forge, and
            // is parsed by the same total parser the remote patch goes through
            // (FR-3.2), so the review screen cannot tell them apart.
            Job::LocalPatch {
                request,
                number,
                head_sha,
                options,
            } => self.local_patch(&prs, request, *number, head_sha, *options, cancel),
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
                let (bundle, _) = self.analyst().gather(request, cancel);
                Outcome::Context {
                    bundle: Box::new(bundle),
                    intent: *intent,
                }
            }
            Job::RunAnalysis { request, bundle } => {
                let analyst = self.analyst();
                let mut report = |update: AnalysisProgress| {
                    sink.send(ProgressUpdate::Analysis(update));
                };
                match analyst.run(request, bundle, cancel, &mut report) {
                    Ok(run) => Outcome::Analyzed(Box::new(run)),
                    Err(error) => Outcome::Failed(error.to_string()),
                }
            }
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
                thread_id,
                resolved,
            } => self.resolve_thread(thread_id, *resolved, cancel),
            // Detection, the report, the catalog, the worktree and the connection
            // check do not need a repository resolved through the forge, so
            // `JobRunner` handles them directly.
            Job::Detect
            | Job::Report { .. }
            | Job::Catalog { .. }
            | Job::Workspace { .. }
            | Job::ModelCheck { .. }
            | Job::CleanWorkspaces { .. } => Outcome::Abandoned,
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
        let service =
            crate::application::drafts::Drafts::new(Arc::clone(&self.drafts), self.repo.clone());
        match service.publish(self.forge.as_ref(), draft, cancel) {
            Ok(posted) => {
                if !posted.dry_run
                    && let Err(error) = service.remove(draft.pr)
                {
                    // The review is on GitHub: a failure to tidy up afterwards is a
                    // warning, never a failed publish.
                    logging::log(
                        Level::Warn,
                        format!("the sent draft could not be removed: {error}"),
                    );
                }
                Outcome::ReviewPosted(Box::new(posted))
            }
            Err(error) => Outcome::Failed(error.to_string()),
        }
    }

    /// Posts a reply, through the service that validates it (FR-6.4).
    fn post_reply(&self, number: u64, comment_id: u64, body: &str, cancel: &Cancel) -> Outcome {
        let posts = crate::application::posts::Posts::new(self.forge.as_ref(), self.repo.clone());
        match posts.reply(number, comment_id, body, cancel) {
            Ok(posted) => Outcome::CommentPosted(Box::new(posted)),
            Err(error) => Outcome::Failed(error.to_string()),
        }
    }

    /// Posts a comment on the pull request's conversation (FR-6.4).
    fn post_conversation(&self, number: u64, body: &str, cancel: &Cancel) -> Outcome {
        let posts = crate::application::posts::Posts::new(self.forge.as_ref(), self.repo.clone());
        match posts.comment(number, body, cancel) {
            Ok(posted) => Outcome::CommentPosted(Box::new(posted)),
            Err(error) => Outcome::Failed(error.to_string()),
        }
    }

    /// Resolves or unresolves a thread (FR-6.4).
    fn resolve_thread(&self, thread_id: &str, resolved: bool, cancel: &Cancel) -> Outcome {
        let posts = crate::application::posts::Posts::new(self.forge.as_ref(), self.repo.clone());
        match posts.resolve(thread_id, resolved, cancel) {
            Ok(()) => Outcome::ThreadResolved {
                thread_id: thread_id.to_owned(),
                resolved,
            },
            Err(error) => Outcome::Failed(error.to_string()),
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
        options: DiffOptions,
        cancel: &Cancel,
    ) -> Outcome {
        // Cache first, with the flags in the key: re-opening a file with the
        // same toggles must not re-run git (FR-3.2).
        match prs.cached_patch_with(number, head_sha, options, DiffSource::Worktree) {
            Ok(Some(cached)) if !cached.stale => {
                return Outcome::Patch {
                    outcome: Box::new(FetchOutcome::Fresh(cached.value)),
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
                    outcome: Box::new(FetchOutcome::Fresh(patch)),
                    source: DiffSource::Worktree,
                    head_sha: head_sha.to_owned(),
                }
            }
            // The worktree is gone: the caller falls back to the forge, and
            // says so rather than showing an empty diff.
            Err(error) => {
                match prs.cached_patch_with(number, head_sha, options, DiffSource::Worktree) {
                    Ok(Some(cached)) => Outcome::Patch {
                        outcome: Box::new(FetchOutcome::Offline {
                            value: cached.value,
                            reason: error.to_string(),
                        }),
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
    /// Progress from jobs that stream.
    progress_sender: Sender<Progress>,
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
        request: DetectRequest,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let (progress_sender, progress_receiver) = mpsc::channel();
        Self {
            executor: None,
            workspace,
            probe,
            catalog,
            llm,
            request,
            queue: VecDeque::new(),
            running: Vec::new(),
            // Job ids start at one so that the `Option`-free "no job yet" sentinel of
            // zero in the app can never collide with a real id.
            next_id: 1,
            sender,
            receiver,
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

        self.cancel_slot(slot);
        self.queue.retain(|(_, _, queued)| queued.slot() != slot);
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

    /// Whether a slot has a job running or waiting.
    #[must_use]
    pub fn is_busy_in(&self, slot: Slot) -> bool {
        self.running.iter().any(|running| running.slot == slot)
            || self.queue.iter().any(|(_, _, job)| job.slot() == slot)
    }

    /// Cancels every job in a slot, for `Esc` on the screen that owns it.
    pub fn cancel(&mut self, slot: Slot) {
        self.cancel_slot(slot);
        self.queue.retain(|(_, _, queued)| queued.slot() != slot);
    }

    /// Starts queued jobs while there is room, and collects finished ones.
    ///
    /// Returns the completions that arrived, in arrival order. It never blocks.
    pub fn poll(&mut self) -> Vec<Completion> {
        let mut completions = Vec::new();
        while let Ok(completion) = self.receiver.try_recv() {
            self.running.retain(|running| running.id != completion.job);
            completions.push(completion);
        }

        // Every job sends exactly one completion, including one that panicked, so
        // the running list is exactly "jobs that have not answered yet".
        self.pump();
        completions
    }

    /// Everything the running jobs have said since the last call.
    ///
    /// Bounded: a provider that streams faster than the interface draws must not be
    /// able to grow the queue without limit, and the text on screen is a preview of
    /// the answer rather than the answer.
    pub fn poll_progress(&mut self) -> Vec<Progress> {
        let mut progress = Vec::new();
        while let Ok(update) = self.progress_receiver.try_recv() {
            if progress.len() < MAX_PROGRESS_MESSAGES {
                progress.push(update);
            }
        }
        progress
    }

    /// Starts as many queued jobs as there is room for.
    fn pump(&mut self) {
        while self.running.len() < MAX_IN_FLIGHT {
            let Some((id, owner, job)) = self.queue.pop_front() else {
                return;
            };
            self.start(id, owner, job);
        }
    }

    /// Runs one job on a worker thread.
    fn start(&mut self, id: u64, owner: JobOwner, job: Job) {
        let slot = job.slot();
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
        let request = self.request.clone();
        let worker_owner = owner.clone();

        let spawned = std::thread::Builder::new()
            .name(format!("smart-review-job-{id}"))
            .spawn(move || {
                // A job that unwinds would never send a completion, and its slot
                // would be occupied for the rest of the session: four such workers
                // and nothing is ever fetched again, with nothing on screen to say
                // why. Catching it here means every job sends exactly one answer.
                let ports = JobPorts {
                    workspace: workspace.as_ref(),
                    probe: probe.as_ref(),
                    catalog: catalog.as_ref(),
                    llm: llm.as_ref(),
                    executor: executor.as_deref(),
                    request: &request,
                };
                let sink = ProgressSink {
                    sender: &progress_sender,
                    job: id,
                    owner: worker_owner.clone(),
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

                let _ = sender.send(Completion {
                    job: id,
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
                    owner,
                    outcome: Outcome::Failed(format!("could not start a worker thread: {error}")),
                });
            }
        }
    }
}

/// Where a streaming job reports what it is doing, and which job it is (FR-4.4).
struct ProgressSink<'a> {
    sender: &'a Sender<Progress>,
    job: u64,
    owner: JobOwner,
}

impl ProgressSink<'_> {
    /// Sends an update, dropping it when nobody is listening.
    ///
    /// A dropped preview costs nothing: the answer arrives whole in the completion.
    fn send(&self, update: ProgressUpdate) {
        let _ = self.sender.send(Progress {
            job: self.job,
            owner: self.owner.clone(),
            update,
        });
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
    executor: Option<&'a Executor>,
    request: &'a DetectRequest,
}

/// Runs a job, whatever kind it is.
///
/// Separate from the thread that runs it, so the thread plumbing (`start`) and the
/// work (`run_job`) can be read and tested apart.
fn run_job(job: &Job, ports: &JobPorts<'_>, cancel: &Cancel, sink: &ProgressSink<'_>) -> Outcome {
    match job {
        Job::Detect => match detect(ports.workspace, ports.probe, ports.request, cancel) {
            Ok(environment) => Outcome::Environment(Box::new(environment)),
            Err(error) => Outcome::EnvironmentFailed(Box::new(error)),
        },
        Job::Report { context } => Outcome::Checks(doctor::collect(context)),
        Job::Catalog { policy } => match ports.catalog.load(*policy) {
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
        match workspace.remove(&entry.repo, entry.number, cancel) {
            Ok(()) => removed += 1,
            Err(error) => failed.push(format!("pr-{}: {error}", entry.number)),
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
            thread_id,
            resolved,
        } => Some(Job::ResolveThread {
            thread_id: thread_id.clone(),
            resolved: *resolved,
        }),
        Effect::RunDoctor => Some(Job::Report {
            context: Box::new(context),
        }),
        Effect::LoadCatalog(policy) => Some(Job::Catalog { policy: *policy }),
        // A diff reload needs the head SHA, which the caller knows and this does not.
        Effect::ReloadDiff
        | Effect::EnsureWorkspace(_)
        | Effect::CheckModel
        | Effect::ClearKey(_)
        | Effect::SaveKey { .. }
        | Effect::SaveSelection(_)
        | Effect::CleanWorkspaces(_)
        | Effect::LoadAnalysis
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
        | Effect::EditComposer(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn runner_with(delay: Duration) -> (JobRunner, Arc<SlowForge>) {
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
            forge: Arc::clone(&forge) as Arc<dyn ForgePort>,
            cache: Arc::new(InMemoryCache::default()),
            clock: Arc::new(FakeClock),
            workspace: Arc::new(fake_workspace()),
            analysis: Arc::new(crate::test_support::InMemoryAnalysis::default()),
            llm: Arc::new(crate::test_support::NoLlm),
            repo: RepoId::parse("acme/service").unwrap(),
            policy: crate::application::prs::CachePolicy::default(),
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
            forge: Arc::new(PanicForge) as Arc<dyn ForgePort>,
            cache: Arc::new(InMemoryCache::default()),
            clock: Arc::new(FakeClock),
            workspace: Arc::new(fake_workspace()),
            analysis: Arc::new(crate::test_support::InMemoryAnalysis::default()),
            llm: Arc::new(crate::test_support::NoLlm),
            repo: RepoId::parse("acme/service").unwrap(),
            policy: crate::application::prs::CachePolicy::default(),
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
