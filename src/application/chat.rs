//! The chat use case (FR-5.1–FR-5.4): ground, ask, stream, record.
//!
//! Chat differs from analysis in three ways that shape this module:
//!
//! - **the context is the system prompt, not a message** (FR-5.3). That is what makes
//!   history trimming safe: the oldest turns can be dropped without ever dropping the
//!   subject of the conversation, and no message has to be flagged as the important
//!   one. It also means the bundle is re-sent every turn, which is what "the model sees
//!   exactly what `:context` reports and nothing more" costs.
//! - **the answer is prose, not a document.** Nothing is normalized, so a failure is a
//!   provider error with a message — there is no repair pass, because there is nothing
//!   to repair.
//! - **nothing is cached.** An answer depends on the question, and a conversation is
//!   the record; a "cache" here would be a way to replay a stale answer to a new
//!   question.
//!
//! What it *shares* with analysis is the bundle ([`crate::application::context`]), the
//! cancellation flag and the progress channel, because those are the parts that must
//! not diverge (FR-5.3).

use crate::application::context::{self, ContextSource, ContextSpec};
use crate::domain::chat::{
    Message, Reference, Role, Session, cost_of, references_in, replayable_history,
};
use crate::domain::context::{Bundle, BundlePolicy, estimate_tokens};
use crate::domain::diff::Patch;
use crate::domain::model::Cost;
use crate::domain::pr::PullRequestDetail;
use crate::domain::repo::RepoId;
use crate::ports::llm::{ChatRequest, LlmError, LlmPort, TokenUsage};
use crate::ports::workspace::WorkspacePort;
use crate::ports::{Cancel, Clock};

/// The share of the context budget the conversation may take (FR-4.6).
///
/// A share rather than the whole thing: the bundle is the subject and the history is
/// the discussion of it, and a conversation that crowded out the code it is about
/// would answer worse the longer it went on. Expressed as a divisor because the budget
/// is bytes and integer arithmetic here cannot round the wrong way.
pub const HISTORY_BUDGET_DIVISOR: usize = 4;

/// The fewest messages a trimmed history keeps, whatever the budget.
///
/// Two is one exchange: a question and the answer it refers to.
pub const MIN_HISTORY_MESSAGES: usize = 2;

/// What happened to the history on the way to the provider (FR-4.6).
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryPlan {
    /// The messages that are sent.
    pub messages: Vec<Message>,
    /// How many of the session's messages are not sent.
    pub dropped: usize,
    /// The budget the history was trimmed to, in bytes.
    pub budget_bytes: usize,
}

impl HistoryPlan {
    /// Whether anything was dropped, which is what the UI has to admit to.
    #[must_use]
    pub fn trimmed(&self) -> bool {
        self.dropped > 0
    }

    /// The one-line explanation, or `None` when the whole conversation fits.
    #[must_use]
    pub fn note(&self) -> Option<String> {
        self.trimmed().then(|| {
            format!(
                "{} earlier message{} not sent: the conversation exceeded its share of \
                 the context budget",
                self.dropped,
                if self.dropped == 1 { "" } else { "s" }
            )
        })
    }
}

/// Progress reported while an answer streams (FR-5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// What the job is doing now.
    Stage(String),
    /// Text as it arrived.
    Delta(String),
}

/// Receives progress as it happens.
pub type ProgressHandler<'a> = dyn FnMut(Progress) + Send + 'a;

/// Everything one question needs that the caller already knows.
#[derive(Debug, Clone)]
pub struct ChatSpec {
    /// The repository.
    pub repo: RepoId,
    /// The pull request number.
    pub pr: u64,
    /// The commit the answer describes, for the record.
    pub head_sha: String,
    /// How to reach the provider, with the key and the thinking settings.
    pub chat: ChatRequest,
    /// The complete shared context specification (IR-12).
    pub context: ContextSpec,
    /// Full input allowance, separate from the source-bundle policy.
    pub input_budget_tokens: u32,
    /// What the catalog says this model costs, for the per-session estimate (FR-5.4).
    pub cost: Option<Cost>,
}

impl ContextSource for ChatSpec {
    fn changed_paths(&self) -> Vec<String> {
        self.context.changed_paths()
    }

    fn detail(&self) -> &PullRequestDetail {
        self.context.detail()
    }

    fn patch(&self) -> Option<&Patch> {
        self.context.patch()
    }

    fn checkout(&self) -> Option<&context::Checkout> {
        self.context.checkout()
    }

    fn policy(&self) -> &BundlePolicy {
        self.context.policy()
    }

    fn added(&self) -> &[String] {
        self.context.added()
    }
}

/// What one question produced.
#[derive(Debug, Clone)]
pub enum ChatRun {
    /// An answer.
    Answered(Box<Answered>),
    /// The user stopped it (FR-5.2). The text that arrived is kept, marked partial.
    Cancelled(Box<Message>),
}

/// A finished answer, with everything that has to be recorded about it.
#[derive(Debug, Clone)]
pub struct Answered {
    /// The message to append to the session.
    pub message: Message,
    /// What the provider charged.
    pub usage: Option<TokenUsage>,
    /// How large the context was, in bytes.
    pub context_bytes: usize,
}

/// The chat use case.
pub struct Chatter<'a> {
    workspace: &'a dyn WorkspacePort,
    llm: &'a dyn LlmPort,
    clock: &'a dyn Clock,
    repo: &'a RepoId,
}

impl std::fmt::Debug for Chatter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chatter")
            .field("repo", &self.repo.key())
            .finish_non_exhaustive()
    }
}

impl<'a> Chatter<'a> {
    /// Binds the use case to its ports.
    #[must_use]
    pub fn new(
        workspace: &'a dyn WorkspacePort,
        llm: &'a dyn LlmPort,
        clock: &'a dyn Clock,
        repo: &'a RepoId,
    ) -> Self {
        Self {
            workspace,
            llm,
            clock,
            repo,
        }
    }

    /// Gathers the bundle this conversation sends (FR-5.3).
    #[must_use]
    pub fn gather(&self, spec: &ChatSpec, cancel: &Cancel) -> Bundle {
        context::gather(self.workspace, spec, cancel).bundle
    }

    /// Asks one question (FR-5.2, FR-5.3).
    ///
    /// # Errors
    ///
    /// Returns the provider's own [`LlmError`]: a chat answer is prose, so there is
    /// nothing to normalize and no repair pass to hide a failure behind.
    pub fn ask(
        &self,
        spec: &ChatSpec,
        session: &Session,
        bundle: &Bundle,
        question: &str,
        cancel: &Cancel,
        progress: &mut ProgressHandler<'_>,
    ) -> Result<ChatRun, LlmError> {
        let system = system_prompt(&bundle.text);
        if crate::ports::llm::estimated_input_bytes(Some(&system), &[], question)
            > input_budget_bytes(spec)
        {
            return Err(LlmError::Request {
                provider: spec.chat.provider.clone(),
                reason: "the context and question exceed the configured input limit; reduce :context or shorten the question".to_owned(),
            });
        }
        let plan = history_plan_that_fits(session, spec, &system, question);
        let request = ChatRequest {
            system: Some(system),
            prompt: question.to_owned(),
            history: plan
                .messages
                .iter()
                .map(|message| (message.role, message.text.clone()))
                .collect(),
            ..spec.chat.clone()
        };

        progress(Progress::Stage(format!("asking {}", request.model)));
        let mut text = String::new();
        let mut on_delta = |delta: &str| {
            text.push_str(delta);
            progress(Progress::Delta(delta.to_owned()));
        };
        let outcome = self.llm.stream(&request, cancel, &mut on_delta);

        let now = self.clock.now_unix_secs();
        // A cancelled call keeps what arrived: FR-5.2 says the partial text stays
        // visible, and a stopped answer is a thing the user chose to stop rather than a
        // failure to report.
        if cancel.is_cancelled() {
            let mut message = Message::assistant(text, now, None, Vec::new());
            message.partial = true;
            return Ok(ChatRun::Cancelled(Box::new(message)));
        }
        let outcome = outcome?;

        let index = spec.context.patch.as_deref().map_or_else(
            crate::domain::analysis::PathIndex::default,
            crate::domain::analysis::PathIndex::from_patch,
        );
        let references = references_in(&outcome.text, &index);
        let mut message = Message::assistant(outcome.text, now, outcome.usage, references);
        message.context_bytes = Some(bundle.bytes());
        Ok(ChatRun::Answered(Box::new(Answered {
            usage: outcome.usage,
            context_bytes: bundle.bytes(),
            message,
        })))
    }

    /// The history this conversation would send, and what it cost to trim (FR-4.6).
    #[must_use]
    pub fn history_plan(&self, session: &Session, policy: &BundlePolicy) -> HistoryPlan {
        let budget_bytes = history_budget(policy);
        let (messages, dropped) = replayable_history(session, budget_bytes, MIN_HISTORY_MESSAGES);
        HistoryPlan {
            messages,
            dropped,
            budget_bytes,
        }
    }

    /// How big a request would be, before it is sent (FR-4.6).
    #[must_use]
    pub fn estimate(&self, spec: &ChatSpec, session: &Session, bundle: &Bundle) -> Estimate {
        let system = system_prompt(&bundle.text);
        let plan = history_plan_that_fits(session, spec, &system, "");
        let history_bytes: usize = plan.messages.iter().map(|message| message.text.len()).sum();
        let total = system.len() + history_bytes;
        Estimate {
            context_bytes: bundle.bytes(),
            history_bytes,
            system_bytes: system.len(),
            estimated_tokens: estimate_tokens(total),
            history: plan,
        }
    }
}

/// Trims history against the space left after the complete system prompt and question.
///
/// A source bundle's ceiling is not a license to append conversation afterwards: the
/// provider sees all of these blocks in one request (FR-4.6, IR-03).
fn history_plan_that_fits(
    session: &Session,
    spec: &ChatSpec,
    system: &str,
    question: &str,
) -> HistoryPlan {
    let full_budget = input_budget_bytes(spec);
    let base = crate::ports::llm::estimated_input_bytes(Some(system), &[], question);
    let budget_bytes = full_budget
        .saturating_sub(base)
        .min(full_budget / HISTORY_BUDGET_DIVISOR);
    let (mut messages, _) = replayable_history(session, budget_bytes, 0);
    while crate::ports::llm::estimated_input_bytes(
        Some(system),
        &messages
            .iter()
            .map(|message| (message.role, message.text.clone()))
            .collect::<Vec<_>>(),
        question,
    ) > full_budget
    {
        if messages.is_empty() {
            break;
        }
        messages.remove(0);
    }
    let dropped = session.messages.len().saturating_sub(messages.len());
    HistoryPlan {
        messages,
        dropped,
        budget_bytes,
    }
}

/// What a request is made of, for the confirmation and the inspector (FR-4.6, FR-5.4).
#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    /// The bundle's size.
    pub context_bytes: usize,
    /// The conversation's size.
    pub history_bytes: usize,
    /// The instructions' size.
    pub system_bytes: usize,
    /// The whole request, in tokens, as an estimate.
    pub estimated_tokens: usize,
    /// What the conversation contributes, and what was dropped from it.
    pub history: HistoryPlan,
}

impl Estimate {
    /// The size of the whole request, in bytes.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.history_bytes + self.system_bytes
    }

    /// One line for the status area.
    #[must_use]
    pub fn label(&self) -> String {
        format!(
            "~{} tokens ({} context, {} conversation)",
            crate::domain::chat::thousands(
                u32::try_from(self.estimated_tokens).unwrap_or(u32::MAX)
            ),
            crate::domain::context::human_bytes(self.context_bytes as u64),
            crate::domain::context::human_bytes(self.history_bytes as u64)
        )
    }
}

/// How big a request would be, from a bundle that has already been gathered (FR-4.6).
///
/// Pure, and free-standing because the interface needs it *after* a gather job has
/// returned a bundle and before anything is sent: the estimate is what the user is
/// shown to agree to, so it goes through the same prompt assembly the request will.
#[must_use]
pub fn estimate_of(
    spec: &ChatSpec,
    session: &Session,
    bundle: &Bundle,
    question: &str,
) -> Estimate {
    let system = system_prompt(&bundle.text);
    let plan = history_plan_that_fits(session, spec, &system, question);
    let history_bytes: usize = plan.messages.iter().map(|message| message.text.len()).sum();
    let total = system.len() + question.len() + history_bytes;
    Estimate {
        context_bytes: bundle.bytes(),
        history_bytes,
        system_bytes: system.len(),
        estimated_tokens: estimate_tokens(total),
        history: plan,
    }
}

fn input_budget_bytes(spec: &ChatSpec) -> usize {
    (spec.input_budget_tokens as usize).saturating_mul(crate::domain::context::BYTES_PER_TOKEN)
}

/// The byte budget for the conversation, from the bundle's token budget (FR-4.6).
#[must_use]
pub fn history_budget(policy: &BundlePolicy) -> usize {
    let bytes = policy.max_bytes();
    let share = bytes / HISTORY_BUDGET_DIVISOR;
    // Never below the floor: a conversation trimmed to nothing cannot be a
    // conversation, and `min_messages` would keep more than the share anyway.
    share.max(4_096)
}

/// The system prompt: who the model is, how to answer, and the context (FR-5.3, §7.3).
///
/// The bundle is *here* rather than in the first user message on purpose: it is
/// instruction plus material, it belongs to every turn equally, and keeping it out of
/// the message list is what lets the oldest turns be dropped without dropping the code
/// they are about.
#[must_use]
pub fn system_prompt(bundle: &str) -> String {
    format!(
        "You are a senior software engineer answering questions about one pull request. \
         You are precise, concrete and brief.\n\
         \n\
         Rules:\n\
         - Answer from the context below and nothing else. The context is everything you \
         have: there is no way for you to read files, so never claim to have looked at \
         something you were not given.\n\
         - Name the file when you assert something about code, spelled exactly as it \
         appears in the context. Quote a line rather than inventing a line number.\n\
         - When a question cannot be answered from the context, say what is missing and \
         name the file that would answer it — the user can add it to the context and ask \
         again. Do not guess.\n\
         - When you state something that is *not* from the context — general knowledge \
         about a library, a language or a convention — begin that sentence with \
         `[general]`. The interface shows those differently, so the user can tell what is \
         grounded in their code.\n\
         - Say when you are unsure, and prefer a short answer to a padded one.\n\
         - Write in the language the question is written in.\n\
         \n\
         The pull request, as the user's own tooling reports it:\n\
         \n\
         <<<CONTEXT\n{bundle}\nCONTEXT>>>\n"
    )
}

/// The cost of one answer, from the catalog's prices (FR-5.4).
#[must_use]
pub fn answer_cost(usage: Option<TokenUsage>, cost: Option<&Cost>) -> Option<f64> {
    match (usage, cost) {
        (Some(usage), Some(cost)) => cost_of(usage, cost),
        _ => None,
    }
}

/// Whether a line of an answer is a general-knowledge claim (FR-5.3).
///
/// The marker is asked for in the system prompt and honoured on a best-effort basis:
/// a model that forgets it produces a line that reads as grounded, which is why the
/// interface *also* shows what the context contained. This is the visible half of the
/// requirement, not the whole of it.
#[must_use]
pub fn general_knowledge_marker(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for marker in ["[general]", "[GEN]", "(general)"] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(rest.trim_start_matches([' ', ':', '—', '-']).trim_start());
        }
    }
    None
}

/// The references an answer makes, for the jump-to-file behaviour (FR-5.1).
#[must_use]
pub fn references_of(message: &Message, patch: Option<&Patch>) -> Vec<Reference> {
    let index = patch.map_or_else(
        crate::domain::analysis::PathIndex::default,
        crate::domain::analysis::PathIndex::from_patch,
    );
    references_in(&message.text, &index)
}

/// A new session for a pull request (FR-5.1).
#[must_use]
pub fn new_session(
    id: String,
    repo: &RepoId,
    pr: u64,
    head_sha: &str,
    model: &str,
    thinking: Option<crate::domain::model::Thinking>,
    at: u64,
) -> Session {
    Session::new(id, repo.key(), pr, head_sha, model, thinking, at)
}

/// The role of the last message, for the UI's "who spoke last" question.
#[must_use]
pub fn last_role(session: &Session) -> Option<Role> {
    session.messages.last().map(|message| message.role)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::chat::Message;
    use crate::test_support::{FakeLlm, FakeWorkspace, temp_home};

    fn repo() -> RepoId {
        RepoId::parse("github.com/acme/service").expect("valid")
    }

    fn patch() -> Patch {
        crate::domain::diff::parse_patch(
            "diff --git a/src/money.rs b/src/money.rs\n\
             index 1111111..2222222 100644\n\
             --- a/src/money.rs\n\
             +++ b/src/money.rs\n\
             @@ -1,2 +1,3 @@\n fn money() {}\n+fn cents() {}\n",
        )
    }

    fn spec() -> ChatSpec {
        ChatSpec {
            repo: repo(),
            pr: 141,
            head_sha: "abc123".to_owned(),
            chat: ChatRequest::new(
                "deepseek",
                "deepseek-v4-pro",
                crate::domain::model::Route::Native(crate::domain::model::NativeBackend::DeepSeek),
                crate::ports::secret::ApiKey::new("sk-test", crate::ports::secret::KeySource::File),
                String::new(),
            ),
            context: ContextSpec {
                detail: Box::new(crate::test_support::sample_detail()),
                patch: Some(Box::new(patch())),
                checkout: Some(context::Checkout {
                    path: std::path::PathBuf::from("/tmp/ws"),
                    head_sha: "abc123".to_owned(),
                    base_sha: "base123".to_owned(),
                }),
                policy: BundlePolicy::default(),
                added: Vec::new(),
                identity: crate::application::context::ContextIdentity {
                    head_sha: "abc123".to_owned(),
                    base_sha: Some("base123".to_owned()),
                    local_content: true,
                    changed_paths: vec!["src/money.rs".to_owned()],
                    added: Vec::new(),
                    policy: BundlePolicy::default(),
                },
            },
            input_budget_tokens: 100_000,
            cost: None,
        }
    }

    fn workspace() -> FakeWorkspace {
        let mut workspace = FakeWorkspace::new(crate::ports::workspace::RepoInfo {
            root: Some(std::path::PathBuf::from("/tmp/ws")),
            remotes: Vec::new(),
            default_branch: Some("main".to_owned()),
            git_version: "2.43.0".to_owned(),
        });
        workspace
            .files
            .insert("abc123:src/money.rs".to_owned(), b"fn money() {}".to_vec());
        workspace
            .files
            .insert("base123:src/money.rs".to_owned(), b"fn money() {}".to_vec());
        workspace
            .files
            .insert("abc123:docs/design.md".to_owned(), b"# design".to_vec());
        workspace
    }

    fn session() -> Session {
        Session::new(
            "1-0",
            "github.com/acme/service",
            141,
            "abc123",
            "deepseek/deepseek-v4-pro",
            None,
            100,
        )
    }

    #[test]
    fn the_system_prompt_carries_the_context_and_the_grounding_rules() {
        let prompt = system_prompt("## diff\n+fn cents() {}");
        assert!(prompt.contains("<<<CONTEXT"), "{prompt}");
        assert!(prompt.contains("+fn cents() {}"), "{prompt}");
        assert!(
            prompt.contains("[general]"),
            "the marker is asked for: {prompt}"
        );
        assert!(prompt.contains("no way for you to read files"), "{prompt}");
    }

    #[test]
    fn a_general_knowledge_line_is_recognised_and_stripped() {
        assert_eq!(
            general_knowledge_marker("[general] Postgres takes a table lock here."),
            Some("Postgres takes a table lock here.")
        );
        assert_eq!(
            general_knowledge_marker("  [general]: tokens expire after an hour"),
            Some("tokens expire after an hour")
        );
        // A line that merely mentions the word is not a marker.
        assert_eq!(general_knowledge_marker("in general, this is fine"), None);
        assert_eq!(general_knowledge_marker("`[general]` is a marker"), None);
    }

    #[test]
    fn the_history_share_is_a_quarter_of_the_budget_and_never_zero() {
        let small = BundlePolicy {
            max_context_tokens: 1_000,
            ..BundlePolicy::default()
        };
        assert_eq!(history_budget(&small), 4_096, "the floor holds");
        let large = BundlePolicy {
            max_context_tokens: 100_000,
            ..BundlePolicy::default()
        };
        assert_eq!(history_budget(&large), 100_000);
    }

    #[test]
    fn a_question_is_asked_with_the_context_in_the_system_prompt_and_the_turns_as_history() {
        let workspace = workspace();
        let llm = FakeLlm::answering(&["It changes `src/money.rs`."]);
        let clock = crate::test_support::FakeClock::new(1_000);
        let repo = repo();
        let chatter = Chatter::new(&workspace, &llm, &clock, &repo);
        let cancel = Cancel::new();

        let mut session = session();
        session.messages = vec![
            Message::user("what does money() do?", 10),
            Message::assistant("It sums lines.", 11, None, Vec::new()),
        ];
        let spec = spec();
        let bundle = chatter.gather(&spec, &cancel);
        let mut progress = |_: Progress| {};
        let run = chatter
            .ask(
                &spec,
                &session,
                &bundle,
                "does it round?",
                &cancel,
                &mut progress,
            )
            .expect("answers");
        let ChatRun::Answered(answered) = run else {
            panic!("expected an answer");
        };

        // The context is in the system prompt, not in a message.
        let (system, prompt) = llm.prompts.lock().expect("lock")[0].clone();
        let system = system.expect("the chat path always sends a system prompt");
        assert!(system.contains("<<<CONTEXT"), "{}", &system[..200]);
        assert!(system.contains("src/money.rs"));
        assert_eq!(prompt, "does it round?");
        // …and the history is the previous exchange.
        let history = llm.histories.lock().expect("lock")[0].clone();
        assert_eq!(
            history
                .iter()
                .map(|(role, text)| (*role, text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (Role::User, "what does money() do?"),
                (Role::Assistant, "It sums lines."),
            ]
        );
        // The answer records what it cost and how big the context was.
        assert_eq!(answered.message.role, Role::Assistant);
        assert!(
            answered
                .message
                .context_bytes
                .is_some_and(|bytes| bytes > 0)
        );
        assert_eq!(
            answered.message.references,
            vec![Reference {
                path: "src/money.rs".to_owned(),
                line: None
            }]
        );
    }

    #[test]
    fn a_cancelled_answer_keeps_the_text_that_arrived_and_is_marked_partial() {
        let workspace = workspace();
        let llm = FakeLlm::answering(&["half of an answer"]);
        let clock = crate::test_support::FakeClock::new(1_000);
        let repo = repo();
        let chatter = Chatter::new(&workspace, &llm, &clock, &repo);
        let cancel = Cancel::new();
        cancel.cancel();

        let spec = spec();
        let bundle = chatter.gather(&spec, &cancel);
        let mut progress = |_: Progress| {};
        let run = chatter
            .ask(&spec, &session(), &bundle, "why?", &cancel, &mut progress)
            .expect("cancelling is not an error");
        let ChatRun::Cancelled(message) = run else {
            panic!("expected a cancelled answer");
        };
        assert!(message.partial);
        assert_eq!(message.text, "half of an answer");
        assert!(message.usage.is_none());
    }

    #[test]
    fn a_provider_failure_is_an_error_and_not_an_answer() {
        let workspace = workspace();
        let llm = FakeLlm::failing(LlmError::Auth {
            provider: "deepseek".to_owned(),
            reason: "401 Unauthorized".to_owned(),
        });
        let clock = crate::test_support::FakeClock::new(1_000);
        let repo = repo();
        let chatter = Chatter::new(&workspace, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let spec = spec();
        let bundle = chatter.gather(&spec, &cancel);
        let mut progress = |_: Progress| {};
        let error = chatter
            .ask(&spec, &session(), &bundle, "why?", &cancel, &mut progress)
            .expect_err("reports");
        assert!(error.to_string().contains("401"), "{error}");
    }

    #[test]
    fn a_file_the_user_added_is_in_the_bundle_and_says_so() {
        let workspace = workspace();
        let llm = FakeLlm::answering(&["ok"]);
        let clock = crate::test_support::FakeClock::new(1_000);
        let repo = repo();
        let chatter = Chatter::new(&workspace, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let mut spec = spec();
        spec.context.added = vec!["docs/design.md".to_owned()];

        let bundle = chatter.gather(&spec, &cancel);
        assert!(bundle.text.contains("# design"), "the file is sent");
        let segment = bundle
            .segments
            .iter()
            .find(|segment| segment.label == "docs/design.md")
            .expect("listed");
        assert_eq!(
            segment.kind,
            crate::domain::context::SegmentKind::UserFile,
            "and listed as the user's own addition"
        );
    }

    #[test]
    fn fr_4_6_the_final_request_reuses_a_rename_decision_for_a_user_added_file() {
        let mut workspace = workspace();
        workspace.files.insert(
            "base123:.env".to_owned(),
            b"RENAMED_USER_FILE_OLD_SECRET".to_vec(),
        );
        workspace.files.insert(
            "abc123:docs/design.md".to_owned(),
            b"RENAMED_USER_FILE_NEW_SECRET".to_vec(),
        );
        let llm = FakeLlm::answering(&["ok"]);
        let clock = crate::test_support::FakeClock::new(1_000);
        let repo = repo();
        let chatter = Chatter::new(&workspace, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let mut spec = spec();
        spec.context.patch = Some(Box::new(crate::domain::diff::parse_patch(
            "diff --git a/.env b/docs/design.md\n\
             similarity index 90%\n\
             rename from .env\n\
             rename to docs/design.md\n\
             --- a/.env\n\
             +++ b/docs/design.md\n\
             @@ -1 +1 @@\n\
             -RENAMED_USER_FILE_OLD_SECRET\n\
             +RENAMED_USER_FILE_NEW_SECRET\n",
        )));
        spec.context.added = vec!["docs/design.md".to_owned()];
        let bundle = chatter.gather(&spec, &cancel);
        let mut progress = |_: Progress| {};
        chatter
            .ask(
                &spec,
                &session(),
                &bundle,
                "is this safe?",
                &cancel,
                &mut progress,
            )
            .expect("the fake provider answers");

        let prompts = llm.prompts.lock().expect("lock");
        let sent = prompts[0]
            .0
            .as_deref()
            .expect("chat sends context in the system prompt");
        assert!(!sent.contains("RENAMED_USER_FILE_OLD_SECRET"), "{sent}");
        assert!(!sent.contains("RENAMED_USER_FILE_NEW_SECRET"), "{sent}");
        assert!(
            bundle
                .inspection()
                .iter()
                .any(|line| { line.contains("docs/design.md") && line.contains("credential") })
        );
    }

    #[test]
    fn fr_4_6_ignored_conventions_and_user_additions_are_not_sent() {
        let mut workspace = workspace();
        workspace.files.insert(
            "abc123:AGENTS.md".to_owned(),
            b"IGNORED_CONVENTION_SENTINEL".to_vec(),
        );
        workspace.ignored = vec!["AGENTS.md".to_owned(), "docs/design.md".to_owned()];
        let llm = FakeLlm::answering(&["ok"]);
        let clock = crate::test_support::FakeClock::new(1_000);
        let repo = repo();
        let chatter = Chatter::new(&workspace, &llm, &clock, &repo);
        let mut spec = spec();
        spec.context.added = vec!["docs/design.md".to_owned()];

        let bundle = chatter.gather(&spec, &Cancel::new());

        assert!(!bundle.text.contains("IGNORED_CONVENTION_SENTINEL"));
        assert!(!bundle.text.contains("# design"));
        for path in ["AGENTS.md", "docs/design.md"] {
            assert!(
                bundle
                    .inspection()
                    .iter()
                    .any(|line| { line.contains(path) && line.contains("ignore rules") }),
                "{path} should be listed as excluded: {:?}",
                bundle.segments
            );
        }
    }

    #[test]
    fn a_file_the_user_added_that_cannot_be_read_is_reported_not_hidden() {
        let workspace = workspace();
        let llm = FakeLlm::answering(&["ok"]);
        let clock = crate::test_support::FakeClock::new(1_000);
        let repo = repo();
        let chatter = Chatter::new(&workspace, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let mut spec = spec();
        spec.context.added = vec!["docs/missing.md".to_owned()];

        let bundle = chatter.gather(&spec, &cancel);
        assert!(
            bundle.segments.iter().any(|segment| segment
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("could not add docs/missing.md"))),
            "{:?}",
            bundle.segments
        );
    }

    #[test]
    fn the_estimate_says_what_the_request_is_made_of() {
        let workspace = workspace();
        let llm = FakeLlm::answering(&["ok"]);
        let clock = crate::test_support::FakeClock::new(1_000);
        let repo = repo();
        let chatter = Chatter::new(&workspace, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let spec = spec();
        let mut session = session();
        session.messages = vec![
            Message::user("question".to_owned(), 1),
            Message::assistant("answer".to_owned(), 2, None, Vec::new()),
        ];
        let bundle = chatter.gather(&spec, &cancel);
        let estimate = chatter.estimate(&spec, &session, &bundle);
        assert!(estimate.context_bytes > 0);
        assert!(estimate.history_bytes > 0);
        assert!(estimate.system_bytes > 0);
        assert_eq!(
            estimate.bytes(),
            estimate.history_bytes + estimate.system_bytes
        );
        assert_eq!(estimate.estimated_tokens, estimate.bytes().div_ceil(4));
        let label = estimate.label();
        assert!(label.starts_with('~'), "{label}");
        assert!(label.contains("context"), "{label}");
        assert!(!estimate.history.trimmed(), "two messages fit");
    }

    #[test]
    fn a_long_conversation_is_trimmed_and_says_by_how_much() {
        let workspace = workspace();
        let llm = FakeLlm::answering(&["ok"]);
        let clock = crate::test_support::FakeClock::new(1_000);
        let repo = repo();
        let chatter = Chatter::new(&workspace, &llm, &clock, &repo);
        let mut session = session();
        // Long enough that 200 turns cannot fit in a quarter of a 100k-token budget.
        let padding = "x".repeat(2_000);
        for turn in 0..200u64 {
            session
                .messages
                .push(Message::user(format!("question {turn} {padding}"), turn));
            session.messages.push(Message::assistant(
                format!("answer {turn} {padding}"),
                turn,
                None,
                Vec::new(),
            ));
        }
        let plan = chatter.history_plan(&session, &BundlePolicy::default());
        assert!(plan.trimmed(), "200 turns do not fit");
        assert!(plan.messages.len() >= MIN_HISTORY_MESSAGES);
        let note = plan.note().expect("admitted");
        assert!(note.contains("not sent"), "{note}");
        // And the newest turn is always there.
        assert!(
            plan.messages
                .last()
                .is_some_and(|message| message.text.starts_with("answer 199")),
            "{:?}",
            plan.messages.last().map(|message| &message.text[..30])
        );
    }

    #[test]
    fn a_model_without_published_prices_has_no_cost() {
        let usage = TokenUsage {
            prompt: 100,
            completion: 50,
            total: 150,
            reasoning: None,
        };
        assert!(answer_cost(Some(usage), None).is_none());
        assert!(answer_cost(None, Some(&Cost::default())).is_none());
        let cost = Cost {
            input: Some(1.0),
            output: Some(2.0),
            reasoning: None,
            cache_read: None,
        };
        let total = answer_cost(Some(usage), Some(&cost)).expect("priced");
        assert!((total - 0.0002).abs() < 1e-12, "{total}");
    }

    #[test]
    fn the_references_of_a_message_need_the_patch_to_be_jumpable() {
        let message =
            Message::assistant("Look at `src/money.rs:12`.".to_owned(), 1, None, Vec::new());
        let found = references_of(&message, Some(&patch()));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "src/money.rs");
        assert_eq!(found[0].line, Some(12));
        // Without the patch there is nothing to resolve against, so nothing is
        // promised: a "jump" that cannot jump is worse than a plain path.
        assert!(references_of(&message, None).is_empty());
    }

    #[test]
    fn a_new_session_records_where_it_started() {
        let repo = repo();
        let session = new_session(
            "1-0".to_owned(),
            &repo,
            141,
            "abc123",
            "deepseek/deepseek-v4-pro",
            Some(crate::domain::model::Thinking::Toggle { value: true }),
            1_000,
        );
        assert_eq!(session.repo, "github.com/acme/service");
        assert_eq!(session.pr, 141);
        assert_eq!(session.head_sha, "abc123");
        assert_eq!(session.messages.len(), 0);
        assert_eq!(last_role(&session), None);
        assert_eq!(session.version, crate::domain::chat::SESSION_VERSION);
        assert_eq!(
            crate::domain::chat::CHAT_PROMPT_VERSION,
            1,
            "bumping the prompt version is what invalidates a stored conversation's grounding"
        );
    }

    #[test]
    fn the_workspace_is_where_the_context_is_read_from() {
        // A pull request without a checkout still produces metadata and an explicit
        // note. Source content is fail-closed because ignore/size policy cannot be
        // verified without repository access.
        let workspace = workspace();
        let llm = FakeLlm::answering(&["ok"]);
        let clock = crate::test_support::FakeClock::new(1_000);
        let repo = repo();
        let chatter = Chatter::new(&workspace, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let mut spec = spec();
        spec.context.checkout = None;
        let bundle = chatter.gather(&spec, &cancel);
        assert!(
            !bundle.text.contains("fn cents"),
            "source content is not sent"
        );
        assert!(
            bundle.segments.iter().any(|segment| segment
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("no local workspace"))),
            "{:?}",
            bundle.segments
        );
        assert!(
            workspace
                .calls()
                .iter()
                .all(|call| !call.starts_with("read_file")),
            "nothing was read without a checkout: {:?}",
            workspace.calls()
        );
    }

    #[test]
    fn the_home_is_not_needed_to_ask_a_question() {
        // No file is written by asking: the session store is the caller's business
        // (FR-5.1), which is what keeps this use case testable without a disk.
        let home = temp_home();
        let workspace = workspace();
        let llm = FakeLlm::answering(&["ok"]);
        let clock = crate::test_support::FakeClock::new(1_000);
        let repo = repo();
        let chatter = Chatter::new(&workspace, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let spec = spec();
        let bundle = chatter.gather(&spec, &cancel);
        let mut progress = |_: Progress| {};
        chatter
            .ask(&spec, &session(), &bundle, "why?", &cancel, &mut progress)
            .expect("answers");
        let written: Vec<String> = walk(home.path());
        assert!(written.is_empty(), "{written:?}");
    }

    /// Every file under a directory, for the test above.
    fn walk(root: &std::path::Path) -> Vec<String> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(root) else {
            return found;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(walk(&path));
            } else {
                found.push(path.display().to_string());
            }
        }
        found
    }
}
