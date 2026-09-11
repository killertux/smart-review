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

use crate::application::environment::{DetectRequest, detect};
use crate::application::prs::{FetchOutcome, Prs};
use crate::doctor::{self, Check, Context};
use crate::domain::diff::Patch;
use crate::domain::environment::Environment;
use crate::domain::pr::PullRequestDetail;
use crate::domain::query::PrQuery;
use crate::domain::repo::RepoId;
use crate::logging::{self, Level};
use crate::ports::cache::CacheStore;
use crate::ports::forge::{ForgePort, ForgeProbe, PullRequestPage};
use crate::ports::workspace::WorkspacePort;
use crate::ports::{Cancel, Clock};
use crate::tui::app::Effect;
use crate::tui::list_view::PrListState;

/// How many jobs may run at once. Process jobs are the expensive ones, and four is
/// what ARCH-5 allows.
pub const MAX_IN_FLIGHT: usize = 4;

/// Which kind of work a job is, one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// Detection, which happens once.
    Environment,
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
}

/// What a job was asked to do.
#[derive(Debug, Clone)]
pub enum Job {
    /// Work out where we are running (FR-1.1).
    Detect,
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
    /// Collect the environment report (FR-9.3).
    Report {
        /// What to check. Boxed because the context is much larger than any other
        /// variant's payload, and every job crosses a channel.
        context: Box<Context>,
    },
}

impl Job {
    /// Which slot the job occupies.
    #[must_use]
    pub fn slot(&self) -> Slot {
        match self {
            Self::Detect => Slot::Environment,
            Self::List { .. } => Slot::List,
            Self::Count { .. } => Slot::Count,
            Self::Detail { .. } => Slot::Detail,
            Self::Patch { .. } => Slot::Patch,
            Self::Report { .. } => Slot::Doctor,
        }
    }

    /// Whether this job runs a process and can therefore be cancelled usefully.
    #[must_use]
    pub fn is_cancellable(&self) -> bool {
        !matches!(self, Self::Report { .. })
    }
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
    /// A count arrived.
    Count(u32),
    /// A detail arrived, or the cached one did.
    Detail(Box<FetchOutcome<PullRequestDetail>>),
    /// A diff arrived, or the cached one did.
    Patch(Box<FetchOutcome<Patch>>),
    /// The environment report.
    Checks(Vec<Check>),
    /// The job failed, with the message to show.
    Failed(String),
    /// The job was replaced or abandoned before it finished.
    Abandoned,
}

/// A finished job, on its way back to the event loop.
#[derive(Debug, Clone)]
pub struct Completion {
    /// Which job this is the answer to.
    pub job: u64,
    /// What it produced.
    pub outcome: Outcome,
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
    repo: RepoId,
    policy: crate::application::prs::CachePolicy,
}

impl Executor {
    /// Binds the executor to a repository and the ports it reads through.
    #[must_use]
    pub fn new(
        forge: Arc<dyn ForgePort>,
        cache: Arc<dyn CacheStore>,
        clock: Arc<dyn Clock>,
        repo: RepoId,
        policy: crate::application::prs::CachePolicy,
    ) -> Self {
        Self {
            forge,
            cache,
            clock,
            repo,
            policy,
        }
    }

    /// Runs one job to completion.
    fn run(&self, job: &Job, cancel: &Cancel) -> Outcome {
        let prs = Prs::new(
            self.forge.as_ref(),
            self.cache.as_ref(),
            self.clock.as_ref(),
            &self.repo,
        )
        .with_policy(self.policy);

        match job {
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
                Ok(outcome) => Outcome::Patch(Box::new(outcome)),
                Err(error) => Outcome::Failed(error.to_string()),
            },
            // Detection and the report do not need a repository, and are handled by
            // `JobRunner` directly.
            Job::Detect | Job::Report { .. } => Outcome::Abandoned,
        }
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
    /// What the command line asked for.
    request: DetectRequest,
    /// Jobs waiting for a free slot.
    queue: VecDeque<(u64, Job)>,
    /// Jobs that are running.
    running: Vec<Running>,
    /// The next job id.
    next_id: u64,
    sender: Sender<Completion>,
    receiver: Receiver<Completion>,
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
        request: DetectRequest,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            executor: None,
            workspace,
            probe,
            request,
            queue: VecDeque::new(),
            running: Vec::new(),
            next_id: 0,
            sender,
            receiver,
        }
    }

    /// Supplies the ports a repository-scoped job needs.
    pub fn set_executor(&mut self, executor: Arc<Executor>) {
        self.executor = Some(executor);
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
        let id = self.next_id;
        self.next_id += 1;
        let slot = job.slot();

        self.cancel_slot(slot);
        self.queue.retain(|(_, queued)| queued.slot() != slot);
        self.queue.push_back((id, job));
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

    /// Cancels every job in a slot, for `Esc` on the screen that owns it.
    pub fn cancel(&mut self, slot: Slot) {
        self.cancel_slot(slot);
        self.queue.retain(|(_, queued)| queued.slot() != slot);
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

        // A job whose thread died without sending would otherwise hold its slot
        // forever, so the running list is reconciled against what actually came back.
        self.pump();
        completions
    }

    /// Starts as many queued jobs as there is room for.
    fn pump(&mut self) {
        while self.running.len() < MAX_IN_FLIGHT {
            let Some((id, job)) = self.queue.pop_front() else {
                return;
            };
            self.start(id, job);
        }
    }

    /// Runs one job on a worker thread.
    fn start(&mut self, id: u64, job: Job) {
        let slot = job.slot();
        // Every job gets a flag; a report simply never blocks long enough for it to
        // matter, and giving one class of job no handle would mean a special case in
        // the code that cancels.
        let cancel = Cancel::new();
        let worker_cancel = cancel.clone();
        let sender = self.sender.clone();
        let executor = self.executor.clone();
        let workspace = Arc::clone(&self.workspace);
        let probe = Arc::clone(&self.probe);
        let request = self.request.clone();

        let spawned = std::thread::Builder::new()
            .name(format!("smart-review-job-{id}"))
            .spawn(move || {
                let outcome = match &job {
                    Job::Detect => {
                        match detect(workspace.as_ref(), probe.as_ref(), &request, &worker_cancel) {
                            Ok(environment) => Outcome::Environment(Box::new(environment)),
                            Err(error) => Outcome::EnvironmentFailed(Box::new(error)),
                        }
                    }
                    Job::Report { context } => Outcome::Checks(doctor::collect(context)),
                    other => match executor.as_ref() {
                        Some(executor) => executor.run(other, &worker_cancel),
                        None => Outcome::Failed(
                            "the repository is not known yet; run :doctor to see why".to_owned(),
                        ),
                    },
                };

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

                let _ = sender.send(Completion { job: id, outcome });
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
                    outcome: Outcome::Failed(format!("could not start a worker thread: {error}")),
                });
            }
        }
    }
}

/// Turns an effect into the job it asks for, if it asks for one.
///
/// Keeping this mapping here rather than in the reducer is what lets the reducer stay
/// a pure function of state: it says *what* it wants, and this decides *how*.
#[must_use]
pub fn job_for(effect: &Effect, list: &PrListState, context: Context) -> Option<Job> {
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
        Effect::RunDoctor => Some(Job::Report {
            context: Box::new(context),
        }),
        // A diff reload needs the head SHA, which the caller knows and this does not.
        Effect::ReloadDiff
        | Effect::None
        | Effect::KeepPending
        | Effect::SaveState
        | Effect::CopyPath(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pr::{CheckRun, CheckSummary, PrState, PullRequestSummary};
    use crate::ports::forge::ForgeCapabilities;
    use crate::ports::workspace::{RepoInfo, WorkspaceError};
    use crate::test_support::InMemoryCache;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    #[derive(Debug, Default)]
    struct FakeWorkspace;

    impl WorkspacePort for FakeWorkspace {
        fn detect(&self) -> Result<RepoInfo, WorkspaceError> {
            Ok(RepoInfo {
                root: Some(std::path::PathBuf::from("/src/service")),
                remotes: vec![crate::ports::workspace::Remote {
                    name: "origin".to_owned(),
                    url: "git@github.com:acme/service.git".to_owned(),
                }],
                default_branch: Some("main".to_owned()),
                git_version: "2.43.0".to_owned(),
            })
        }
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
        let mut runner = JobRunner::new(
            Arc::new(FakeWorkspace),
            Arc::new(FakeProbe {
                delay: Duration::ZERO,
            }),
            DetectRequest::default(),
        );
        let forge = Arc::new(SlowForge::new(delay));
        runner.set_executor(Arc::new(Executor::new(
            Arc::clone(&forge) as Arc<dyn ForgePort>,
            Arc::new(InMemoryCache::default()),
            Arc::new(FakeClock),
            RepoId::parse("acme/service").unwrap(),
            crate::application::prs::CachePolicy::default(),
        )));
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
        let mut runner = JobRunner::new(
            Arc::new(FakeWorkspace),
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

    #[test]
    fn a_report_job_runs_without_a_repository() {
        let (mut runner, _forge) = runner_with(Duration::ZERO);
        runner.executor = None;
        runner.submit(Job::Report {
            context: Box::new(context()),
        });
        let completions = wait_for_completion(&mut runner);
        match &completions[0].outcome {
            Outcome::Checks(checks) => assert!(
                checks.iter().any(|check| check.name == "home"),
                "the report should have run: {checks:?}"
            ),
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
            job_for(&Effect::DetectEnvironment, &list, context()),
            Some(Job::Detect)
        ));
        assert!(matches!(
            job_for(&Effect::LoadPullRequests, &list, context()),
            Some(Job::List { .. })
        ));
        assert!(matches!(
            job_for(&Effect::OpenPullRequest(141), &list, context()),
            Some(Job::Detail { number: 141 })
        ));
        assert!(matches!(
            job_for(&Effect::RunDoctor, &list, context()),
            Some(Job::Report { .. })
        ));
        assert!(job_for(&Effect::None, &list, context()).is_none());
        assert!(job_for(&Effect::CopyPath("a".to_owned()), &list, context()).is_none());

        // `:load-more` asks for a bigger page, not the same one again.
        let mut grown = PrListState::new(50, 120);
        grown.limit = 50;
        match job_for(&Effect::LoadMore, &grown, context()) {
            Some(Job::List { query }) => assert_eq!(query.limit, 100),
            other => panic!("expected a list job, got {other:?}"),
        }
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
        };
        crate::Startup::load(&cli).unwrap().doctor_context()
    }
}
