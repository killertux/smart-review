//! Chat sessions (FR-5.1–FR-5.4).
//!
//! A chat is a conversation *about one pull request*, grounded in the same context
//! bundle an analysis gets (FR-5.3) — the model sees what `:context` reports and
//! nothing more, because there is no tool loop in v1 (DEC-2). Everything in this
//! module follows from two of the requirements:
//!
//! - **history is append-only** (FR-5.1). Nothing here edits or deletes a message:
//!   a retry adds a turn, a cancellation keeps the text that arrived marked as
//!   partial. The file on disk grows and is pruned, never rewritten in place.
//! - **what was sent is part of the turn**. A message records the size of the
//!   context that went with it and what the provider charged for it (FR-5.4), so a
//!   cost figure on screen is the sum of what actually happened rather than an
//!   estimate reconstructed afterwards.
//!
//! The conversation is *not* a cache. Losing it costs the user words rather than
//! money, but losing it to a crash is still a failure (NFR-4.1), which is why a
//! session is one JSON document written atomically rather than a log appended to.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::domain::analysis::PathIndex;
use crate::domain::model::{Cost, Thinking};
use crate::ports::llm::TokenUsage;

/// The prompt version for chat, bumping whenever the wording or the rules change
/// (FR-5.3, §7.3).
///
/// Separate from the analysis prompt's version: the two prompts change for different
/// reasons, and one bumping the other would invalidate a cache for no reason.
pub const CHAT_PROMPT_VERSION: u32 = 1;

/// The document version, for migrations (FR-5.1).
pub const SESSION_VERSION: u32 = 1;

/// How many sessions are kept per pull request (DEC-9).
pub const MAX_SESSIONS_PER_PR: usize = 50;

/// How large one session may grow before it is refused (DEC-9).
///
/// Refused rather than pruned: a conversation is the user's own words, and the one
/// thing the app must not do is quietly drop the middle of one.
pub const MAX_SESSION_BYTES: usize = 2 * 1024 * 1024;

/// The most a single question may be.
///
/// A question pasted from a review comment can be long, but a question is not a file:
/// past this the right tool is `:context add <path>` (FR-5.3), and saying so is
/// better than sending a novel.
pub const MAX_QUESTION_BYTES: usize = 8 * 1024;

/// Who wrote a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The user.
    User,
    /// The model.
    Assistant,
}

impl Role {
    /// The name in a transcript.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::User => "you",
            Self::Assistant => "model",
        }
    }
}

/// A path the answer mentions, made jumpable (FR-5.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    /// The path, spelled the way the diff spells it when the diff knows it.
    pub path: String,
    /// The line the answer named, when it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

impl Reference {
    /// How the reference is shown in a transcript.
    #[must_use]
    pub fn label(&self) -> String {
        match self.line {
            Some(line) => format!("{}:{line}", self.path),
            None => self.path.clone(),
        }
    }
}

/// One message in a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Who wrote it.
    pub role: Role,
    /// The text, exactly as written or received.
    pub text: String,
    /// When it was written, in seconds since the Unix epoch.
    pub at: u64,
    /// What the provider charged for this turn, on an answer (FR-5.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    /// The paths this answer mentions (FR-5.1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Reference>,
    /// How much context went with this turn, in bytes (FR-5.4, FR-4.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_bytes: Option<usize>,
    /// The answer was stopped before it finished (FR-5.2).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
    /// The turn failed: the text is the error, and the answer never came.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
}

impl Message {
    /// A user message.
    #[must_use]
    pub fn user(text: impl Into<String>, at: u64) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
            at,
            usage: None,
            references: Vec::new(),
            context_bytes: None,
            partial: false,
            failed: false,
        }
    }

    /// The same question, with the size of the context that was sent with it.
    #[must_use]
    pub fn with_context(mut self, bytes: usize) -> Self {
        self.context_bytes = Some(bytes);
        self
    }

    /// An answer.
    #[must_use]
    pub fn assistant(
        text: impl Into<String>,
        at: u64,
        usage: Option<TokenUsage>,
        references: Vec<Reference>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
            at,
            usage,
            references,
            context_bytes: None,
            partial: false,
            failed: false,
        }
    }

    /// Whether the *text* of this message is worth sending back to the provider.
    ///
    /// A partial or failed answer is kept on screen — the user watched it arrive, and
    /// FR-5.2 says the text stays visible — but replaying half an answer as if the
    /// model had said it would teach the model that incomplete sentences are answers.
    /// The question that produced it still goes back: see [`replayable_history`].
    #[must_use]
    pub fn is_replayable(&self) -> bool {
        !self.partial && !self.failed && !self.text.trim().is_empty()
    }

    /// Whether this is what the user asked, including the context that went with it.
    #[must_use]
    pub fn is_question(&self) -> bool {
        self.role == Role::User
    }
}

/// One conversation about one pull request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Document version (FR-5.1).
    pub version: u32,
    /// Stable id, also the file name.
    pub id: String,
    /// The repository, as `host/owner/name`.
    pub repo: String,
    /// The pull request.
    pub pr: u64,
    /// The head commit the conversation started at.
    ///
    /// Not a cache key and not a reason to refuse: a chat is a conversation, and a
    /// force-push mid-conversation is a thing to *tell* the user about, because the
    /// context they approved is no longer the code on screen.
    pub head_sha: String,
    /// The model the conversation started with, for provenance.
    pub model: String,
    /// The thinking setting it started with, for the same reason (FR-4.8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    /// When it was created.
    pub created_at: u64,
    /// When it last changed.
    pub updated_at: u64,
    /// The conversation, oldest first, append-only (FR-5.1).
    pub messages: Vec<Message>,
}

impl Session {
    /// A session with no messages yet.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        repo: impl Into<String>,
        pr: u64,
        head_sha: impl Into<String>,
        model: impl Into<String>,
        thinking: Option<Thinking>,
        at: u64,
    ) -> Self {
        Self {
            version: SESSION_VERSION,
            id: id.into(),
            repo: repo.into(),
            pr,
            head_sha: head_sha.into(),
            model: model.into(),
            thinking,
            created_at: at,
            updated_at: at,
            messages: Vec::new(),
        }
    }

    /// How many turns the user has asked.
    #[must_use]
    pub fn turns(&self) -> usize {
        self.messages.iter().filter(|m| m.is_question()).count()
    }

    /// The questions and answers, in order.
    #[must_use]
    pub fn conversation(&self) -> Vec<&Message> {
        self.messages.iter().collect()
    }

    /// What every answer in this session cost, as far as the provider reported it
    /// (FR-5.4).
    #[must_use]
    pub fn totals(&self) -> Totals {
        let mut totals = Totals::default();
        for message in &self.messages {
            if let Some(usage) = message.usage {
                totals.usage.add(usage);
            }
        }
        totals.turns = self.turns();
        totals
    }

    /// The first question in the session, which is the one that carried the context.
    #[must_use]
    pub fn first_question(&self) -> Option<&Message> {
        self.messages.iter().find(|message| message.is_question())
    }

    /// The last question, which `:chat retry` re-asks (FR-5.2).
    #[must_use]
    pub fn last_question(&self) -> Option<&Message> {
        self.messages
            .iter()
            .rev()
            .find(|message| message.is_question())
    }

    /// The serialised size, for the retention cap (DEC-9).
    #[must_use]
    pub fn bytes(&self) -> usize {
        serde_json::to_string(self).map_or(0, |json| json.len())
    }

    /// Whether the pull request has moved since the conversation began.
    #[must_use]
    pub fn is_stale_against(&self, head_sha: &str) -> bool {
        !self.head_sha.is_empty() && self.head_sha != head_sha
    }
}

/// What a session's answers add up to (FR-5.4).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Totals {
    /// Tokens, summed over the answers that reported them.
    pub usage: TokenUsage,
    /// How many turns the user asked.
    pub turns: usize,
    /// What it cost, when the catalog prices the model (FR-5.4).
    pub cost: Option<f64>,
}

impl Totals {
    /// A one-line summary for the status line: `3 turns · ~4.2k tokens · ~$0.0041`.
    #[must_use]
    pub fn label(&self) -> String {
        let tokens = self.usage.total;
        let mut parts = vec![format!(
            "{} turn{}",
            self.turns,
            if self.turns == 1 { "" } else { "s" }
        )];
        if tokens > 0 {
            parts.push(format!("~{} tokens", thousands(tokens)));
        }
        if let Some(cost) = self.cost {
            parts.push(format!("~{}", format_cost(cost)));
        }
        parts.join(" · ")
    }

    /// Adds cost, when the catalog prices the model.
    #[must_use]
    pub fn with_cost(mut self, cost: Option<f64>) -> Self {
        self.cost = cost;
        self
    }
}

/// A row in `:chat list` (FR-5.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// The id.
    pub id: String,
    /// When it was created.
    pub created_at: u64,
    /// When it last changed, which is what the list sorts by.
    pub updated_at: u64,
    /// How many questions it holds.
    pub turns: usize,
    /// How many messages it holds.
    pub messages: usize,
    /// The model it started with.
    pub model: String,
    /// The commit it started at.
    pub head_sha: String,
    /// Its size on disk.
    pub bytes: usize,
    /// Total tokens the provider reported (FR-5.4).
    #[serde(default)]
    pub tokens: u32,
}

impl SessionMeta {
    /// The row derived from a session.
    #[must_use]
    pub fn of(session: &Session) -> Self {
        let totals = session.totals();
        Self {
            id: session.id.clone(),
            created_at: session.created_at,
            updated_at: session.updated_at,
            turns: session.turns(),
            messages: session.messages.len(),
            model: session.model.clone(),
            head_sha: session.head_sha.clone(),
            bytes: session.bytes(),
            tokens: totals.usage.total,
        }
    }

    /// The first question, for the list, when the session has been read.
    #[must_use]
    pub fn preview<'a>(&self, session: &'a Session) -> Option<&'a str> {
        session
            .first_question()
            .map(|message| message.text.trim())
            .filter(|text| !text.is_empty())
    }
}

/// What pruning did, so it can be announced (DEC-9, FR-8.5).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pruned {
    /// The sessions that were removed, oldest first.
    pub removed: Vec<String>,
    /// Whether a removal happened because the size cap was reached.
    pub over_bytes: bool,
}

impl Pruned {
    /// Whether anything was removed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty()
    }

    /// The notice text, which names what went and how to keep it (DEC-9).
    #[must_use]
    pub fn notice(&self) -> String {
        format!(
            "{} old chat session{} removed to stay within the per-pull-request cap \
             ({} sessions, {} each); `:chat export` keeps a transcript",
            self.removed.len(),
            if self.removed.len() == 1 { "" } else { "s" },
            MAX_SESSIONS_PER_PR,
            crate::domain::context::human_bytes(MAX_SESSION_BYTES as u64),
        )
    }
}

/// Which sessions to keep, oldest removed first (DEC-9).
///
/// Pure, and separate from the store, because the rule is a requirement rather than a
/// filesystem behaviour: `list` ordering is the list's business, but "the newest 50
/// survive" is DEC-9's.
#[must_use]
pub fn sessions_to_prune(mut metas: Vec<SessionMeta>, max_sessions: usize) -> Vec<String> {
    // Oldest first, so the ones that go are the ones the user has moved on from.
    metas.sort_by_key(|meta| (meta.updated_at, meta.id.clone()));
    let keep_from = metas.len().saturating_sub(max_sessions);
    metas
        .into_iter()
        .take(keep_from)
        .map(|meta| meta.id)
        .collect()
}

/// The conversation as the provider should see it, with the oldest turns dropped
/// (FR-4.6: "per-file elision → diff context reduction → oldest chat turns dropped").
///
/// Two decisions are worth stating, because neither is forced by the format:
///
/// - **the context is not in the history.** The bundle goes in the system prompt, so
///   trimming is free to drop the oldest turns without ever dropping the subject of
///   the conversation, and no message has to be special-cased as "the important one".
/// - **a turn whose answer was stopped or failed keeps its question.** Dropping such a
///   question would leave the next question ("finish the thought") referring to
///   something the model was never told. Providers merge consecutive messages from the
///   same role into one turn, so the resulting history is legal; what it must not be is
///   *starting* with an answer, which is the one shape that reads as the model talking
///   to itself.
///
/// `min_messages` is the floor a small budget cannot go below: a question that arrives
/// with nothing before it is worse than a larger request. The newest question is not in
/// the history at all — it is the request.
#[must_use]
pub fn replayable_history(
    session: &Session,
    budget_bytes: usize,
    min_messages: usize,
) -> (Vec<Message>, usize) {
    // Every question, and only the answers whose text is worth replaying: a stopped
    // answer leaves its question in place (see the note above) but not its half-
    // sentence.
    let messages: Vec<&Message> = session
        .messages
        .iter()
        .filter(|message| message.is_replayable() || message.is_question())
        .collect();
    if messages.is_empty() {
        return (Vec::new(), 0);
    }

    let mut kept: Vec<usize> = Vec::new();
    let mut bytes = 0;
    for index in (0..messages.len()).rev() {
        let cost = messages[index].text.len();
        if kept.len() >= min_messages && bytes + cost > budget_bytes {
            break;
        }
        kept.push(index);
        bytes += cost;
    }
    kept.reverse();

    // A history that opens with an answer is a model talking to itself: the question
    // that produced it was dropped.
    let mut history: Vec<Message> = kept.into_iter().map(|i| messages[i].clone()).collect();
    if history
        .first()
        .is_some_and(|message| !message.is_question())
    {
        history.remove(0);
    }

    // Counted against the session rather than against the filtered list: what the
    // caller has to explain is everything the user wrote that the provider did not
    // receive, whether it was trimmed for the budget or left out for its own sake.
    let dropped = session.messages.len().saturating_sub(history.len());
    (history, dropped)
}

/// The paths an answer mentions, resolved against the change (FR-5.1).
///
/// Resolution is deliberately narrow: a token that the diff does not know is not a
/// reference, so a sentence mentioning `main.rs` in a change with three of them is
/// left alone rather than linking to the wrong one.
#[must_use]
pub fn references_in(text: &str, index: &PathIndex) -> Vec<Reference> {
    let mut found: Vec<Reference> = Vec::new();
    for token in text.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let Some((raw, line)) = split_line_suffix(token) else {
            continue;
        };
        let Some(path) = index.resolve(raw) else {
            continue;
        };
        let reference = Reference { path, line };
        if !found.contains(&reference) {
            found.push(reference);
        }
    }
    found
}

/// Splits `path:12` into the path and the line, tolerating the punctuation a sentence
/// wraps a path in.
///
/// The two kinds of decoration nest in either order: a backtick-quoted path at the end
/// of a sentence has the full stop *after* the closing backtick, while a parenthesised
/// one has it before the bracket. They are therefore stripped in a loop until nothing
/// changes rather than in one pass — a single pass leaves the backtick attached to the
/// line number, the number stops parsing, and a jumpable reference silently becomes no
/// reference at all.
fn split_line_suffix(token: &str) -> Option<(&str, Option<u32>)> {
    /// Quote-like characters a sentence wraps a path in.
    const WRAPPING: &[char] = &[
        '`', '"', '\'', '(', ')', '[', ']', '{', '}', '<', '>', '*', ',',
    ];
    /// Trailing punctuation that ends the *sentence* rather than the path.
    const TRAILING: &[char] = &['.', ';', ':', '!', '?'];

    let mut trimmed = token;
    loop {
        let next = trimmed.trim_matches(WRAPPING).trim_end_matches(TRAILING);
        if next.len() == trimmed.len() {
            break;
        }
        trimmed = next;
    }
    if trimmed.is_empty() {
        return None;
    }
    // `:12` or `:12-18`: the line the answer points at, if it points at one.
    if let Some((path, rest)) = trimmed.rsplit_once(':') {
        let first = rest.split('-').next().unwrap_or(rest);
        if let Ok(line) = first.parse::<u32>() {
            let path = path.trim_end_matches(':');
            if !path.is_empty() {
                return Some((path, Some(line)));
            }
        }
    }
    Some((trimmed, None))
}

/// What one answer cost, from the catalog's published prices (FR-5.4).
///
/// An estimate, and always labelled as one: the catalog is somebody else's data
/// (FR-4.7), cached input is not distinguished from fresh input, and providers round.
/// `None` means the catalog does not price this model, which is most of them.
#[must_use]
pub fn cost_of(usage: TokenUsage, cost: &Cost) -> Option<f64> {
    let input = cost.input?;
    let output = cost.output.unwrap_or(input);
    // Reasoning tokens are usually a *subset* of the completion tokens; a model whose
    // catalog entry prices them separately publishes completion counts that exclude
    // them, so billing them at the reasoning rate is the closer reading of both.
    let reasoning = cost.reasoning.map_or(0.0, |rate| {
        let tokens = f64::from(usage.reasoning.unwrap_or(0));
        tokens * rate / 1_000_000.0
    });
    let prompt = f64::from(usage.prompt) * input / 1_000_000.0;
    let completion_tokens = f64::from(
        usage
            .completion
            .saturating_sub(usage.reasoning.unwrap_or(0).min(usage.completion)),
    );
    let completion = completion_tokens * output / 1_000_000.0;
    let total = prompt + completion + reasoning;
    total.is_finite().then_some(total)
}

/// Renders a dollar amount so a small one is still readable (FR-5.4).
#[must_use]
pub fn format_cost(usd: f64) -> String {
    if !usd.is_finite() {
        return "unknown".to_owned();
    }
    if usd == 0.0 {
        return "$0".to_owned();
    }
    if usd < 0.000_1 {
        return "<$0.0001".to_owned();
    }
    if usd < 1.0 {
        return format!("${usd:.4}");
    }
    format!("${usd:.2}")
}

/// Groups thousands so a token count is readable at a glance.
#[must_use]
pub fn thousands(count: u32) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (position, character) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position).is_multiple_of(3) {
            out.push(',');
        }
        out.push(character);
    }
    out
}

/// A transcript, for `:chat export` (FR-5.1).
///
/// Markdown by default because a transcript is for reading or pasting into an issue;
/// JSON because the same content is what a script wants.
#[must_use]
pub fn to_markdown(session: &Session) -> String {
    let totals = session.totals();
    let thinking = match &session.thinking {
        Some(thinking) => format!(
            " ({})",
            Thinking::label(&Some(thinking.clone())).unwrap_or_default()
        ),
        None => String::new(),
    };
    let mut out = format!(
        "# {}#{}\n\n\n- started {} at `{}`\n- model `{}`{}\n- {}\n\n",
        session.repo,
        session.pr,
        session.created_at,
        short_sha(&session.head_sha),
        session.model,
        thinking,
        totals.label()
    );
    for message in &session.messages {
        let _ = write!(out, "## {}\n\n", message.role.label());
        out.push_str(message.text.trim_end());
        out.push_str("\n\n");
        if !message.references.is_empty() {
            out.push_str("referenced: ");
            out.push_str(
                &message
                    .references
                    .iter()
                    .map(Reference::label)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            out.push_str("\n\n");
        }
        if message.partial {
            out.push_str("_(the answer was stopped before it finished)_\n\n");
        }
    }
    out
}

/// The same transcript as JSON, for a script (FR-5.1).
///
/// # Errors
///
/// Returns the serializer's message when the session cannot be serialised, which for a
/// document of strings and numbers cannot happen; it is returned rather than panicked
/// on because a failed export must say so.
pub fn to_json(session: &Session) -> Result<String, String> {
    serde_json::to_string_pretty(session).map_err(|error| error.to_string())
}

/// The short form of a commit, for a label.
#[must_use]
pub fn short_sha(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

/// A session id: time, then a counter, so the ids sort and two in the same second
/// cannot collide.
///
/// No randomness and no new dependency: two sessions created in the same second by the
/// same process are exactly what the counter is for, and the interface creates at most
/// one at a time.
#[must_use]
pub fn session_id(at: u64, sequence: u64) -> String {
    format!("{at:x}-{sequence:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with(messages: Vec<Message>) -> Session {
        let mut session = Session::new(
            "abc",
            "github.com/acme/service",
            141,
            "ba6c89f0a1b2c3d4",
            "deepseek/deepseek-v4-pro",
            None,
            100,
        );
        session.messages = messages;
        session
    }

    fn index() -> PathIndex {
        PathIndex::from_paths(vec![
            "src/domain/money.rs".to_owned(),
            "src/infra/pg.rs".to_owned(),
            "docs/readme.md".to_owned(),
        ])
    }

    #[test]
    fn a_reference_needs_a_path_the_change_knows() {
        let found = references_in(
            "The rounding lives in `src/domain/money.rs`, and `src/infra/pg.rs:88` \
             calls it. See src/nowhere.rs and money.rs for the rest.",
            &index(),
        );
        // The bare name is unambiguous here (one `money.rs`), so it resolves, and the
        // line is kept. The unknown path does not resolve, and a path mentioned twice
        // is one reference: the list is for jumping, not for counting.
        assert_eq!(
            found,
            vec![
                Reference {
                    path: "src/domain/money.rs".to_owned(),
                    line: None
                },
                Reference {
                    path: "src/infra/pg.rs".to_owned(),
                    line: Some(88)
                },
            ]
        );
    }

    #[test]
    fn a_question_keeps_its_capitalisation_and_its_punctuation() {
        // The trap this guards: `references_in` splits on punctuation, and a caller
        // that reused that split for the question text would send `does this break`
        // without its question mark.
        let question = "Does this break the webhook retry contract?";
        assert_eq!(
            references_in(question, &index()),
            Vec::<Reference>::new(),
            "a question with no path mentions nothing"
        );
        assert_eq!(Message::user(question, 1).text, question);
    }

    #[test]
    fn a_path_wrapped_in_a_code_span_and_ending_a_sentence_still_resolves() {
        // The shape a markdown-ish answer produces most often, and the one that made a
        // reference disappear: the full stop is outside the closing backtick.
        let found = references_in("Look at `src/infra/pg.rs:88`.", &index());
        assert_eq!(
            found,
            vec![Reference {
                path: "src/infra/pg.rs".to_owned(),
                line: Some(88)
            }]
        );
        // And the other order.
        let found = references_in("(see src/infra/pg.rs.)", &index());
        assert_eq!(
            found,
            vec![Reference {
                path: "src/infra/pg.rs".to_owned(),
                line: None
            }]
        );
    }

    #[test]
    fn a_path_is_not_invented_from_punctuation() {
        let found = references_in("a:b:c, (src/infra/pg.rs), docs/readme.md.", &index());
        assert_eq!(
            found,
            vec![
                Reference {
                    path: "src/infra/pg.rs".to_owned(),
                    line: None
                },
                Reference {
                    path: "docs/readme.md".to_owned(),
                    line: None
                },
            ]
        );
    }

    #[test]
    fn the_cap_removes_the_oldest_sessions_first() {
        let metas: Vec<SessionMeta> = (0..5u64)
            .map(|n| SessionMeta {
                id: format!("s{n}"),
                created_at: n,
                updated_at: n,
                turns: 1,
                messages: 2,
                model: "m".to_owned(),
                head_sha: "abc".to_owned(),
                bytes: 10,
                tokens: 0,
            })
            .collect();
        assert_eq!(
            sessions_to_prune(metas.clone(), 3),
            vec!["s0".to_owned(), "s1".to_owned()]
        );
        // And the newest sessions are what survives.
        assert!(
            sessions_to_prune(metas.clone(), 3)
                .iter()
                .all(|id| id != "s4")
        );
        // Nothing to do when the cap is not reached.
        assert!(sessions_to_prune(metas, 50).is_empty());
    }

    /// A session of question/answer turns, the way the app builds one.
    fn conversation(turns: u64) -> Session {
        let mut messages = Vec::new();
        for turn in 0..turns {
            messages.push(Message::user(format!("question {turn}"), turn));
            messages.push(Message::assistant(
                format!("answer {turn}"),
                turn,
                None,
                Vec::new(),
            ));
        }
        session_with(messages)
    }

    #[test]
    fn the_history_keeps_the_newest_turns_and_never_opens_with_an_answer() {
        let session = conversation(5);
        let (history, dropped) = replayable_history(&session, 40, 2);
        assert!(dropped > 0, "something had to go");
        // The newest turn is the one that must be there: a question whose answer was
        // trimmed away is the failure mode this guards.
        assert_eq!(
            history.last().map(|m| m.text.as_str()),
            Some("answer 4"),
            "{history:?}"
        );
        assert!(
            history[0].is_question(),
            "starts with a question: {history:?}"
        );
        // What survives is a suffix of the conversation, in order, with nothing
        // invented and nothing reordered.
        let texts: Vec<&str> = history.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["question 3", "answer 3", "question 4", "answer 4"]
        );
    }

    #[test]
    fn the_most_recent_turns_are_kept_whatever_the_budget() {
        // `min_messages` is what stops a small budget from producing a question with
        // no answer in front of it ("what about that case?").
        let session = conversation(6);
        let (history, _) = replayable_history(&session, 1, 2);
        assert_eq!(history.len(), 2, "the last exchange: {history:?}");
        assert_eq!(history[0].text, "question 5");
        assert_eq!(history[1].text, "answer 5");
    }

    #[test]
    fn a_context_that_is_not_in_the_history_cannot_be_trimmed_away() {
        // The bundle lives in the system prompt (FR-5.3), so a long conversation can
        // drop every old turn without ever dropping what the conversation is about.
        let session = conversation(20);
        let (history, dropped) = replayable_history(&session, 4, 2);
        assert!(dropped >= 36, "nearly everything went: {dropped}");
        assert!(
            history
                .iter()
                .all(|message| !message.text.contains("CONTEXT"))
        );
    }

    #[test]
    fn a_partial_answer_is_shown_but_never_replayed() {
        let mut messages = vec![Message::user("question".to_owned(), 2)];
        let mut partial = Message::assistant("half an answ", 3, None, Vec::new());
        partial.partial = true;
        messages.push(partial);
        messages.push(Message::user("finish the thought".to_owned(), 4));
        messages.push(Message::assistant(
            "the whole answer".to_owned(),
            5,
            None,
            Vec::new(),
        ));
        let session = session_with(messages);

        let (history, dropped) = replayable_history(&session, 1000, 4);
        assert_eq!(dropped, 1, "the partial text is not in the history");
        assert_eq!(
            history.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            vec!["question", "finish the thought", "the whole answer"],
            "the question whose answer was stopped stays, so `finish the thought` has              something to refer to"
        );
        // But it is still on screen: the session keeps every message.
        assert_eq!(session.messages.len(), 4);
        assert!(session.messages[1].partial);
    }

    #[test]
    fn a_failed_turn_is_shown_but_never_replayed() {
        let mut failed = Message::assistant("the provider said no", 2, None, Vec::new());
        failed.failed = true;
        let session = session_with(vec![
            Message::user("question".to_owned(), 2),
            failed,
            Message::user("again".to_owned(), 3),
            Message::assistant("an answer at last".to_owned(), 4, None, Vec::new()),
        ]);
        let (history, dropped) = replayable_history(&session, 1000, 3);
        assert_eq!(dropped, 1);
        assert!(history.iter().all(|message| !message.failed));
        assert_eq!(
            history.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            vec!["question", "again", "an answer at last"]
        );
    }

    #[test]
    fn totals_add_up_and_the_label_says_what_it_counts() {
        let session = session_with(vec![
            Message::user("q1".to_owned(), 1),
            Message::assistant(
                "a1".to_owned(),
                2,
                Some(TokenUsage {
                    prompt: 1000,
                    completion: 500,
                    total: 1500,
                    reasoning: None,
                }),
                Vec::new(),
            ),
            Message::user("q2".to_owned(), 3),
            Message::assistant(
                "a2".to_owned(),
                4,
                Some(TokenUsage {
                    prompt: 1200,
                    completion: 600,
                    total: 1800,
                    reasoning: Some(200),
                }),
                Vec::new(),
            ),
        ]);
        let totals = session.totals();
        assert_eq!(totals.usage.prompt, 2200);
        assert_eq!(totals.usage.total, 3300);
        assert_eq!(totals.turns, 2);
        let label = totals.with_cost(Some(0.0042)).label();
        assert_eq!(label, "2 turns · ~3,300 tokens · ~$0.0042");
        // A session with no answers yet says so without inventing a cost.
        assert_eq!(Totals::default().label(), "0 turns");
    }

    #[test]
    fn cost_uses_the_catalogs_published_prices() {
        let cost = Cost {
            input: Some(0.28),
            output: Some(0.42),
            reasoning: None,
            cache_read: Some(0.028),
        };
        let usage = TokenUsage {
            prompt: 1_000_000,
            completion: 1_000_000,
            total: 2_000_000,
            reasoning: None,
        };
        let total = cost_of(usage, &cost).expect("priced");
        assert!((total - 0.70).abs() < 1e-9, "{total}");
        // A model the catalog does not price has no cost, rather than a zero that
        // would read as "free".
        assert!(cost_of(usage, &Cost::default()).is_none());
    }

    #[test]
    fn reasoning_tokens_are_billed_at_their_own_rate_when_one_is_published() {
        let cost = Cost {
            input: Some(0.0),
            output: Some(1.0),
            reasoning: Some(2.0),
            cache_read: None,
        };
        let usage = TokenUsage {
            prompt: 0,
            completion: 1000,
            total: 1000,
            reasoning: Some(400),
        };
        // 600 output tokens at $1/M plus 400 reasoning tokens at $2/M.
        let total = cost_of(usage, &cost).expect("priced");
        assert!((total - 0.0014).abs() < 1e-12, "{total}");
    }

    #[test]
    fn a_cost_is_never_printed_as_zero_when_it_is_merely_small() {
        assert_eq!(format_cost(0.0), "$0");
        assert_eq!(format_cost(0.000_01), "<$0.0001");
        assert_eq!(format_cost(0.004_2), "$0.0042");
        assert_eq!(format_cost(0.512_3), "$0.5123");
        assert_eq!(format_cost(12.345), "$12.35");
    }

    #[test]
    fn token_counts_are_grouped() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn a_transcript_carries_the_provenance_the_turns_and_the_references() {
        let mut session = session_with(vec![
            Message::user("does this break retries?".to_owned(), 1),
            Message::assistant(
                "It changes `src/infra/pg.rs:88`.".to_owned(),
                2,
                Some(TokenUsage {
                    prompt: 10,
                    completion: 5,
                    total: 15,
                    reasoning: None,
                }),
                vec![Reference {
                    path: "src/infra/pg.rs".to_owned(),
                    line: Some(88),
                }],
            ),
        ]);
        session.thinking = Some(Thinking::Toggle { value: true });
        let markdown = to_markdown(&session);
        assert!(
            markdown.contains("github.com/acme/service#141"),
            "{markdown}"
        );
        assert!(markdown.contains("`ba6c89f0`"), "{markdown}");
        assert!(markdown.contains("## you"), "{markdown}");
        assert!(markdown.contains("## model"), "{markdown}");
        assert!(
            markdown.contains("referenced: src/infra/pg.rs:88"),
            "{markdown}"
        );
        assert!(markdown.contains("1 turn · ~15 tokens"), "{markdown}");

        let json = to_json(&session).expect("serialises");
        let parsed: Session = serde_json::from_str(&json).expect("round-trips");
        assert_eq!(parsed, session);
    }

    #[test]
    fn a_session_survives_a_round_trip_through_its_own_format() {
        let session = session_with(vec![
            Message::user("hello".to_owned(), 1),
            Message::assistant("hi".to_owned(), 2, None, Vec::new()),
        ]);
        let json = serde_json::to_string(&session).expect("serialises");
        assert_eq!(serde_json::from_str::<Session>(&json).unwrap(), session);
        // The optional fields are omitted rather than written as nulls, so a file the
        // user opens to read is not mostly punctuation.
        assert!(!json.contains("usage"), "{json}");
        assert!(!json.contains("partial"), "{json}");
    }

    #[test]
    fn a_session_knows_when_the_pull_request_has_moved_under_it() {
        let session = session_with(Vec::new());
        assert!(!session.is_stale_against("ba6c89f0a1b2c3d4"));
        assert!(session.is_stale_against("f00dfeed00000000"));
        // An empty head sha is "unknown", not "different": a session opened before the
        // commit was known must not claim the code changed.
        let mut unknown = session.clone();
        unknown.head_sha = String::new();
        assert!(!unknown.is_stale_against("f00dfeed00000000"));
    }

    #[test]
    fn a_question_is_what_the_user_asked_and_an_answer_carries_the_cost() {
        let message = Message::user("why?", 7);
        assert!(message.is_question());
        assert!(!message.is_replayable() || !message.text.is_empty());
        let answer = Message::assistant("because", 8, None, Vec::new());
        assert!(!answer.is_question());
        assert!(answer.is_replayable());
        // An empty answer is not worth replaying: a provider that returned nothing
        // teaches the model nothing.
        assert!(!Message::assistant("   ", 8, None, Vec::new()).is_replayable());
    }

    #[test]
    fn the_pruning_notice_names_what_went_and_the_way_out() {
        let pruned = Pruned {
            removed: vec!["a".to_owned(), "b".to_owned()],
            over_bytes: false,
        };
        let notice = pruned.notice();
        assert!(notice.contains("2 old chat sessions"), "{notice}");
        assert!(notice.contains("50"), "{notice}");
        assert!(notice.contains("2.0 MB"), "{notice}");
        assert!(notice.contains(":chat export"), "{notice}");
        assert!(!pruned.is_empty());
        assert!(Pruned::default().is_empty());
    }

    #[test]
    fn ids_sort_by_time_and_do_not_collide_within_a_second() {
        assert_eq!(session_id(0x64, 0), "64-0");
        let early = session_id(1000, 0);
        let later = session_id(1000, 1);
        assert!(early < later, "{early} {later}");
        assert!(session_id(2000, 0) > later);
    }
}
