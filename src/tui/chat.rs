//! The chat pane's state (FR-5.1–FR-5.4).
//!
//! State, not behaviour: everything that decides *what happens* is in
//! [`crate::application::chat`], and everything that decides *what it looks like* is in
//! [`crate::tui::components::chat`]. What lives here is the small amount of bookkeeping
//! the interface needs between frames — which conversation is open, what is typed,
//! what has streamed in, what is in flight — plus the rules that are about the
//! *interface* rather than the requirement: when the pane is shown, which question a
//! retry repeats, and what the header says.

use crate::application::chat::{Estimate, HistoryPlan};
use crate::domain::chat::{Session, SessionMeta, Totals};
use crate::tui::app::StreamBuffer;
use crate::tui::input::TextInput;

/// What the pane is doing (FR-5.2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ChatStatus {
    /// Nothing in flight.
    #[default]
    Idle,
    /// Waiting for the provider.
    Sending {
        /// What the job is doing now.
        stage: String,
    },
    /// Text is arriving.
    Streaming {
        /// What the job is doing now.
        stage: String,
    },
    /// The last attempt failed, with the reason to show.
    Failed {
        /// The provider's own message.
        reason: String,
    },
    /// The answer was stopped before it finished (FR-5.2).
    Stopped,
}

impl ChatStatus {
    /// Whether work is in flight, which is what `Esc` cancels.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Sending { .. } | Self::Streaming { .. })
    }

    /// The one-line description for the pane's footer.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Idle => "ready".to_owned(),
            Self::Sending { stage } | Self::Streaming { stage } => stage.clone(),
            Self::Failed { reason } => format!("failed: {reason}"),
            Self::Stopped => "stopped; the answer is kept".to_owned(),
        }
    }
}

/// The chat pane.
#[derive(Debug, Default)]
pub struct ChatState {
    /// Whether the pane is on screen.
    ///
    /// Separate from having a session: a user who has never asked anything has no
    /// session, and the pane is still worth opening — it says what to do.
    pub open: bool,
    /// The conversations that exist for this pull request, newest first (FR-5.1).
    pub sessions: Vec<SessionMeta>,
    /// The conversation on screen.
    pub session: Option<Session>,
    /// What the pane is doing.
    pub status: ChatStatus,
    /// The compose box.
    pub input: TextInput,
    /// How far up the conversation is scrolled, in lines from the bottom.
    pub scroll: usize,
    /// The text that has streamed in for the answer in flight (FR-5.2).
    pub stream: StreamBuffer,
    /// The question in flight, kept so a retry can repeat it (FR-5.2).
    pub pending: Option<String>,
    /// The question waiting to be confirmed before the first send for this repository
    /// (FR-4.6).
    pub awaiting_confirmation: Option<String>,
    /// The question whose context has been gathered and is waiting to be sent.
    ///
    /// A gather is not the send: the effect that gathers returns, and the effect that
    /// sends comes back later, by which time the compose box has been cleared. Without
    /// this, the send would re-read an empty box and report that there is nothing to ask
    /// — which is exactly what the second question did.
    pub staged: Option<String>,
    /// What the next request would be made of, when it is known (FR-4.6, FR-5.4).
    pub estimate: Option<Estimate>,
    /// What the last request left out of the conversation (FR-4.6).
    pub dropped: usize,
    /// The files the user added to the context with `:context add` (FR-5.3).
    pub added: Vec<String>,
    /// Which row of them the user is on, for `:context remove` and the inspector.
    pub added_cursor: usize,
    /// Whether the session list is what the pane shows, for `:chat list`.
    pub listing: bool,
    /// The job id of the send in flight (FR-5.2).
    pub job: u64,
    /// The job id of the load in flight (FR-5.1).
    pub load_job: u64,
    /// The job id of the export in flight (FR-5.1).
    pub export_job: u64,
}

impl ChatState {
    /// Opens the pane.
    pub fn open(&mut self) {
        self.open = true;
        self.listing = false;
    }

    /// Closes the pane, keeping the conversation.
    pub fn close(&mut self) {
        self.open = false;
        self.listing = false;
    }

    /// The conversation's totals, for the header (FR-5.4).
    #[must_use]
    pub fn totals(&self) -> Totals {
        self.session
            .as_ref()
            .map_or_else(Totals::default, Session::totals)
    }

    /// Whether there is anything to send.
    #[must_use]
    pub fn can_send(&self) -> bool {
        !self.input.is_empty() && !self.status.is_running()
    }

    /// Whether the pane is waiting for the user to confirm a send (FR-4.6).
    #[must_use]
    pub fn is_confirming(&self) -> bool {
        self.awaiting_confirmation.is_some()
    }

    /// Takes the question a gather was made for, if there is one (FR-4.6).
    pub fn take_staged(&mut self) -> Option<String> {
        self.staged.take()
    }

    /// The question a retry would repeat: the one in flight, or the last one asked
    /// (FR-5.2).
    #[must_use]
    pub fn retry_question(&self) -> Option<String> {
        if let Some(pending) = &self.pending {
            return Some(pending.clone());
        }
        self.session
            .as_ref()
            .and_then(Session::last_question)
            .map(|message| message.text.clone())
    }

    /// The messages to draw, oldest first, with the answer in flight appended as its
    /// own partial message so the pane has one shape whether an answer is streaming or
    /// finished.
    #[must_use]
    pub fn messages(&self) -> Vec<ChatLine> {
        let mut lines: Vec<ChatLine> = Vec::new();
        if let Some(session) = &self.session {
            for index in 0..session.messages.len() {
                lines.push(ChatLine::Stored(index));
            }
        }
        if self.status.is_running() || self.status == ChatStatus::Stopped {
            lines.push(ChatLine::Streaming);
        }
        lines
    }

    /// How many of the conversation's messages the last request did not send (FR-4.6).
    pub fn set_dropped(&mut self, plan: &HistoryPlan) {
        self.dropped = plan.dropped;
    }

    /// Records an estimate for display (FR-4.6).
    pub fn set_estimate(&mut self, estimate: Estimate) {
        self.dropped = estimate.history.dropped;
        self.estimate = Some(estimate);
    }

    /// Forgets the answer in flight, keeping whatever arrived.
    pub fn stop(&mut self) {
        self.status = ChatStatus::Stopped;
        self.pending = None;
        self.staged = None;
    }

    /// Clears the pane back to "nothing asked yet" for a new conversation (FR-5.1).
    pub fn reset(&mut self) {
        self.session = None;
        self.status = ChatStatus::Idle;
        self.stream.clear();
        self.pending = None;
        self.awaiting_confirmation = None;
        self.staged = None;
        self.estimate = None;
        self.dropped = 0;
        self.scroll = 0;
        self.listing = false;
    }
}

/// One line of the conversation, as the renderer sees it.
///
/// The renderer needs the message's *identity* as well as its text, because a click, a
/// jump or a retry has to name a message rather than a row on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatLine {
    /// A message that is stored in the session, by index.
    Stored(usize),
    /// The answer arriving now (FR-5.2).
    Streaming,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::chat::Message;

    fn session() -> Session {
        let mut session = Session::new(
            "1-0",
            "github.com/acme/service",
            141,
            "abc123",
            "deepseek/deepseek-v4-pro",
            None,
            100,
        );
        session.messages.push(Message::user("does it round?", 101));
        session
            .messages
            .push(Message::assistant("It does.", 102, None, Vec::new()));
        session
    }

    #[test]
    fn a_stopped_answer_is_still_something_to_send_again() {
        let mut chat = ChatState {
            session: Some(session()),
            ..ChatState::default()
        };
        assert_eq!(chat.retry_question(), Some("does it round?".to_owned()));
        // While a question is in flight the retry repeats *that* one: the user asked
        // something, the answer failed, and `r` means "try that again".
        chat.pending = Some("and the discount?".to_owned());
        assert_eq!(chat.retry_question(), Some("and the discount?".to_owned()));
    }

    #[test]
    fn sending_needs_something_to_send_and_nothing_in_flight() {
        let mut chat = ChatState::default();
        assert!(!chat.can_send(), "an empty box has nothing to send");
        chat.input.set_text("why?");
        assert!(chat.can_send());
        chat.status = ChatStatus::Sending {
            stage: "asking".to_owned(),
        };
        assert!(!chat.can_send(), "one answer at a time");
    }

    #[test]
    fn the_answers_render_in_order_with_the_one_in_flight_last() {
        let mut chat = ChatState::default();
        assert_eq!(chat.messages(), Vec::new(), "nothing asked yet");
        chat.session = Some(session());
        assert_eq!(
            chat.messages(),
            vec![ChatLine::Stored(0), ChatLine::Stored(1)]
        );
        chat.status = ChatStatus::Streaming {
            stage: "asking".to_owned(),
        };
        assert_eq!(
            chat.messages(),
            vec![
                ChatLine::Stored(0),
                ChatLine::Stored(1),
                ChatLine::Streaming
            ]
        );
        // A stopped answer stays on screen (FR-5.2), which is the point of keeping it.
        chat.status = ChatStatus::Stopped;
        assert_eq!(chat.messages().len(), 3);
        chat.status = ChatStatus::Idle;
        assert_eq!(chat.messages().len(), 2, "once stored, it is the session's");
    }

    #[test]
    fn starting_a_new_conversation_clears_the_pane_but_not_the_list() {
        let mut chat = ChatState {
            session: Some(session()),
            sessions: vec![SessionMeta::of(&session())],
            ..ChatState::default()
        };
        chat.input.set_text("half-typed");
        chat.reset();
        assert!(chat.session.is_none());
        assert_eq!(chat.sessions.len(), 1, "the old one is still openable");
        // The half-typed question is kept: `:chat new` is not `:chat clear`, and losing
        // typing to a command is the kind of small betrayal that makes an interface
        // annoying.
        assert_eq!(chat.input.text(), "half-typed");
    }

    #[test]
    fn the_status_says_what_is_happening_including_why_it_failed() {
        assert_eq!(ChatStatus::Idle.label(), "ready");
        assert_eq!(
            ChatStatus::Sending {
                stage: "asking deepseek-chat".to_owned()
            }
            .label(),
            "asking deepseek-chat"
        );
        assert_eq!(
            ChatStatus::Failed {
                reason: "401 Unauthorized".to_owned()
            }
            .label(),
            "failed: 401 Unauthorized"
        );
        assert!(ChatStatus::Stopped.label().contains("kept"));
        assert!(!ChatStatus::Stopped.is_running());
    }

    #[test]
    fn a_failure_is_not_a_conversation_turn() {
        // The status is not a message: a provider error must not become something the
        // model is told it said.
        let chat = ChatState {
            session: Some(session()),
            status: ChatStatus::Failed {
                reason: "timeout".to_owned(),
            },
            ..ChatState::default()
        };
        assert_eq!(chat.messages().len(), 2);
        assert_eq!(chat.session.as_ref().expect("session").messages.len(), 2);
    }
}
