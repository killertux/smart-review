//! Publishing a review: one call, and what it says when it refuses (FR-6.3, FR-6.5).
//!
//! GitHub's "create a review" endpoint takes the decision, the body and every inline
//! comment in **one** request, so a review either exists or it does not. That is the
//! whole reason this file exists instead of a loop over `gh api` per comment: N calls
//! means N notifications, N chances to fail halfway, and N things to delete by hand.
//!
//! The alternative — `gh pr review` — cannot carry inline comments at all, so it is
//! used for exactly one case: a review with a verdict and nothing anchored to a line.
//!
//! Two things here are deliberately not clever:
//!
//! * the payload is built by `serde_json`, never by pasting user text into a query
//!   string. A comment body is arbitrary prose (quotes, newlines, `\`), and the only
//!   safe place to escape it is in a serializer;
//! * GitHub's refusals are translated by *phrase*, in one function, with the phrases
//!   kept together so they can be re-checked against the API in one place. A refusal
//!   the translation does not know is passed through unchanged rather than replaced
//!   with something vaguer (FR-9.1).

use std::path::PathBuf;

use crate::adapters::gh::GhCliForge;
use crate::adapters::process::CommandSpec;
use crate::domain::draft::{Draft, DraftComment};
use crate::ports::{Cancel, ReviewPosted};

/// The payload `POST /repos/{owner}/{repo}/pulls/{number}/reviews` takes.
///
/// `commit_id` is the commit the comments were written against, when the draft knows
/// it: anchoring to the current head after a force-push would put the comment on
/// whatever now happens to be on that line.
#[must_use]
pub(super) fn review_payload(draft: &Draft) -> serde_json::Value {
    let mut payload = serde_json::json!({ "event": draft.effective_decision().event() });
    if let Some(body) = draft.body.as_deref().filter(|body| !body.trim().is_empty()) {
        payload["body"] = serde_json::json!(body);
    }
    if let Some(head) = &draft.head_sha {
        payload["commit_id"] = serde_json::json!(head);
    }
    if !draft.comments.is_empty() {
        payload["comments"] = serde_json::Value::Array(
            draft
                .comments
                .iter()
                .map(comment_payload)
                .collect::<Vec<_>>(),
        );
    }
    payload
}

/// One inline comment, in the spelling this endpoint takes.
///
/// The mapping lives here rather than in the draft: the draft is the user's document
/// and does not know what an API calls a line. `start_line` and `start_side` are only
/// sent together, because a start without a side is read from the other end.
fn comment_payload(comment: &DraftComment) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "path": comment.path,
        "line": comment.line,
        "side": comment.side.api(),
        "body": comment.body,
    });
    if let Some(start) = comment.start_line {
        payload["start_line"] = serde_json::json!(start);
        payload["start_side"] = serde_json::json!(comment.side.api());
    }
    payload
}

/// GitHub's own sentence for a refusal, translated into what the user should do.
///
/// The phrases are matched case-insensitively against everything `gh` printed. An
/// unrecognized refusal is returned as it arrived, trimmed: a strange sentence from
/// the API is more useful than a confident one from here (FR-9.1).
#[must_use]
pub(super) fn translate_refusal(text: &str, number: u64) -> String {
    let lowered = text.to_ascii_lowercase();
    // A verdict on your own pull request. GitHub's message names the *event*, so all
    // three spellings are here — and the fix is the same for all of them: say it as a
    // comment instead, which the draft supports by leaving the decision alone.
    if lowered.contains("can not approve your own pull request")
        || lowered.contains("cannot approve your own pull request")
    {
        return format!(
            "#{number} is yours, so GitHub will not let you approve it — post it as a \
             comment instead (no decision, or `:draft decision comment`)"
        );
    }
    if lowered.contains("can not request changes on your own pull request")
        || lowered.contains("cannot request changes on your own pull request")
    {
        return format!(
            "#{number} is yours, so GitHub will not let you request changes on it — post \
             it as a comment instead (no decision, or `:draft decision comment`)"
        );
    }
    if lowered.contains("no commits between") {
        return format!(
            "#{number} has no commits between its base and its head, so there is nothing \
             to review yet"
        );
    }
    // An anchor GitHub will not accept: the line is not in the diff, or the side is
    // wrong for it. The message names the position, which is the part to keep.
    if lowered.contains("pull_request_review_thread.line")
        || lowered.contains("line must be part of the diff")
    {
        return format!(
            "GitHub would not anchor a comment: that line is not part of #{number}'s diff. \
             The pull request may have moved since the comment was written — `:draft` lists \
             what is staged, and re-anchoring means writing it again on the current diff"
        );
    }
    if lowered.contains("validation failed") {
        return format!(
            "GitHub rejected the review of #{number}: {}",
            first_line(text)
        );
    }
    if lowered.contains("not permitted")
        || lowered.contains("forbidden")
        || lowered.contains("resource not accessible")
    {
        return format!(
            "your token is not allowed to review #{number} — `gh auth status` says what it \
             is logged in as, and a repository you can push to is usually one you can review"
        );
    }
    if lowered.contains("bad credentials") || lowered.contains("http 401") {
        return "GitHub rejected the token in `gh` — `gh auth login` to renew it".to_owned();
    }
    if lowered.contains("rate limit") {
        return "GitHub is rate-limiting this token; the review was not sent — try again in \
                a few minutes"
            .to_owned();
    }
    first_line(text)
}

fn first_line(text: &str) -> String {
    let trimmed = text.trim();
    let first = trimmed
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("GitHub refused the review and said nothing");
    if first.chars().count() > 400 {
        let shortened: String = first.chars().take(400).collect();
        return format!("{shortened}…");
    }
    first.to_owned()
}

/// Where the JSON payload is written for `--input`.
///
/// In the temporary directory rather than the state directory: it exists for one
/// command, and a body that mentions a security problem is not something to leave in
/// `~/.smart-review` (§7.4's reasoning about secrets, applied to prose).
///
/// The name carries a counter because the pid alone is not enough: two reviews
/// published in one session (or two tests publishing in parallel) would share a name,
/// and the atomic-write dance around it — write `.tmp`, rename — would have one of
/// them rename the other's file away. That is not hypothetical; it is how this
/// function first failed.
fn payload_path(number: u64) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "smart-review-review-{}-{number}-{unique}.json",
        std::process::id()
    ))
}

impl GhCliForge {
    /// Posts the draft as one review (FR-6.3).
    ///
    /// # Errors
    ///
    /// Returns an error when the draft is not publishable, when the payload cannot be
    /// written, or when `gh` refuses — with GitHub's refusal translated.
    pub(super) fn submit_review(
        &self,
        number: u64,
        draft: &Draft,
        cancel: &Cancel,
    ) -> crate::Result<ReviewPosted> {
        draft
            .publishable()
            .map_err(|error| crate::Error::forge(format!("review #{number}"), error.to_string()))?;

        if draft.comments.is_empty() {
            return self.submit_plain(number, draft, cancel);
        }
        self.submit_batched(number, draft, cancel)
    }

    /// `gh pr review` — the verdict and the body, with no inline comments (FR-6.3).
    fn submit_plain(
        &self,
        number: u64,
        draft: &Draft,
        cancel: &Cancel,
    ) -> crate::Result<ReviewPosted> {
        let decision = draft.effective_decision();
        let number_arg = number.to_string();
        let flag = format!("--{}", decision.gh_flag());
        let mut args: Vec<&str> = vec!["pr", "review", &number_arg, &flag];
        if let Some(body) = draft.body.as_deref().filter(|body| !body.trim().is_empty()) {
            args.push("--body");
            args.push(body);
        }
        let spec = self.spec(&args).mutating();

        let Some(output) = self.mutate(&spec, cancel)? else {
            return Ok(dry_run());
        };
        if !output.success() {
            return Err(crate::Error::forge(
                spec.render(),
                translate_refusal(&output.stderr, number),
            ));
        }
        Ok(ReviewPosted::default())
    }

    /// `gh api -X POST …/reviews --input <payload>` — the batched review (FR-6.3).
    ///
    /// One request carrying the decision, the body and every comment, which is what
    /// makes "one review" true rather than hopeful.
    fn submit_batched(
        &self,
        number: u64,
        draft: &Draft,
        cancel: &Cancel,
    ) -> crate::Result<ReviewPosted> {
        let payload = review_payload(draft);
        let text = serde_json::to_string_pretty(&payload)
            .map_err(|error| crate::Error::forge("review", error.to_string()))?;
        let path = payload_path(number);
        // Written even during a dry run, and deliberately: the recorded command says
        // `--input <path>`, and a command a person cannot run by hand is not the
        // command that would have run (FR-6.5).
        crate::adapters::fs::write_atomic_with_mode(&path, &text, Some(0o600))
            .map_err(|error| crate::Error::io("write the review payload", path.clone(), error))?;

        let spec = CommandSpec::new(self.program.clone())
            .args(["api", "-X", "POST"])
            .arg(format!(
                "repos/{}/{}/pulls/{number}/reviews",
                self.repo.owner(),
                self.repo.name()
            ))
            .arg("--input")
            .arg(&path)
            .mutating();

        let Some(output) = self.mutate(&spec, cancel)? else {
            return Ok(dry_run());
        };
        let _ = std::fs::remove_file(&path);
        if !output.success() {
            return Err(crate::Error::forge(
                spec.render(),
                translate_refusal(&output.stderr, number),
            ));
        }
        let response: serde_json::Value =
            serde_json::from_str(&output.stdout).unwrap_or(serde_json::Value::Null);
        Ok(ReviewPosted {
            id: response.get("id").and_then(serde_json::Value::as_u64),
            url: response
                .get("html_url")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            dry_run: false,
        })
    }
}

/// What a dry run reports: nothing was sent (FR-6.5).
fn dry_run() -> ReviewPosted {
    ReviewPosted {
        id: None,
        url: None,
        dry_run: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::draft::{Decision, DraftComment, Side};
    use crate::domain::time::from_unix_secs;

    fn draft() -> Draft {
        let mut draft = Draft::new(141, from_unix_secs(1_700_000_000));
        draft.set_decision(
            Some(Decision::RequestChanges),
            from_unix_secs(1_700_000_000),
        );
        draft.set_body("One thing to fix.", from_unix_secs(1_700_000_000));
        draft.head_sha = Some("abc123".to_owned());
        draft
    }

    #[test]
    fn the_payload_carries_the_decision_the_body_and_the_comments() {
        let mut draft = draft();
        draft.add(
            DraftComment::new("src/a.rs", Side::New, 31, Some(28), "this rounds up")
                .expect("valid"),
            from_unix_secs(1_700_000_000),
        );
        draft.add(
            DraftComment::new("src/b.rs", Side::Old, 7, None, "and this is unused").expect("valid"),
            from_unix_secs(1_700_000_000),
        );

        let payload = review_payload(&draft);
        assert_eq!(payload["event"], "REQUEST_CHANGES");
        assert_eq!(payload["body"], "One thing to fix.");
        assert_eq!(payload["commit_id"], "abc123");
        let comments = payload["comments"].as_array().expect("an array");
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0]["path"], "src/a.rs");
        assert_eq!(comments[0]["line"], 31);
        assert_eq!(comments[0]["start_line"], 28);
        assert_eq!(comments[0]["side"], "RIGHT");
        assert_eq!(comments[0]["start_side"], "RIGHT");
        assert_eq!(comments[1]["side"], "LEFT");
        assert!(comments[1].get("start_line").is_none());
    }

    #[test]
    fn a_body_is_escaped_by_the_serializer_not_by_hand() {
        // The reason this payload is JSON: comment bodies are arbitrary prose. This
        // one contains everything that breaks naive string building.
        let mut draft = draft();
        let hostile = "he said \"no\"\nand then:\\ \t\u{1F600} \u{2028} <!-- --> </script>";
        draft.set_body(hostile, from_secs());
        draft.add(
            DraftComment::new("src/a.rs", Side::New, 1, None, hostile).expect("valid"),
            from_secs(),
        );
        let payload = review_payload(&draft);

        // Round-trips exactly, and the serialized form is valid JSON.
        let text = serde_json::to_string(&payload).expect("serializable");
        let read: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(read["body"], hostile);
        assert_eq!(read["comments"][0]["body"], hostile);
    }

    fn from_secs() -> crate::domain::time::Timestamp {
        from_unix_secs(1_700_000_000)
    }

    #[test]
    fn a_decision_only_approval_sends_no_body_and_no_comments() {
        let mut draft = Draft::new(141, from_secs());
        draft.set_decision(Some(Decision::Approve), from_secs());
        let payload = review_payload(&draft);
        assert_eq!(payload["event"], "APPROVE");
        assert!(payload.get("body").is_none(), "an empty body is not sent");
        assert!(payload.get("comments").is_none());
        assert!(payload.get("commit_id").is_none());
    }

    #[test]
    fn a_blank_body_is_not_sent_as_text() {
        let mut draft = Draft::new(141, from_secs());
        draft.set_body("   \n\t ", from_secs());
        let payload = review_payload(&draft);
        assert!(payload.get("body").is_none());
        assert_eq!(payload["event"], "COMMENT", "and it is still a comment");
    }

    #[test]
    fn your_own_pull_request_is_explained_instead_of_quoted() {
        for message in [
            "gh: Can not approve your own pull request (HTTP 422)",
            "GraphQL: Cannot approve your own pull request",
        ] {
            let translated = translate_refusal(message, 141);
            assert!(translated.contains("is yours"), "explained: {translated}");
            assert!(translated.contains("comment instead"), "{translated}");
        }
        let translated = translate_refusal("Can not request changes on your own pull request", 141);
        assert!(translated.contains("request changes on it"), "{translated}");
    }

    #[test]
    fn an_anchor_github_will_not_take_explains_itself() {
        let translated = translate_refusal(
            "gh: Validation Failed (HTTP 422)\nPull request review thread line must be part of \
             the diff",
            141,
        );
        assert!(
            translated.contains("not part of #141's diff"),
            "{translated}"
        );
        assert!(
            translated.contains("re-anchoring"),
            "what to do about it: {translated}"
        );
    }

    #[test]
    fn validation_failure_without_a_known_phrase_keeps_githubs_words() {
        let translated = translate_refusal("gh: Validation Failed (HTTP 422)", 141);
        assert!(
            translated.starts_with("GitHub rejected the review of #141"),
            "{translated}"
        );
        assert!(translated.contains("Validation Failed"), "{translated}");
    }

    #[test]
    fn the_other_refusals_have_sentences_too() {
        assert!(translate_refusal("HTTP 401: Bad credentials", 7).contains("gh auth login"));
        assert!(translate_refusal("API rate limit exceeded", 7).contains("rate-limiting"));
        assert!(translate_refusal("Resource not accessible by integration", 7).contains("token"));
        assert!(translate_refusal("no commits between main and topic", 7).contains("nothing"));
    }

    #[test]
    fn an_unknown_refusal_is_passed_through_trimmed() {
        // A strange sentence from the API beats a confident one invented here: it is
        // the difference between "it failed" and knowing what to change.
        let translated = translate_refusal("\n  gh: something new happened \n", 141);
        assert_eq!(translated, "gh: something new happened");
    }

    #[test]
    fn an_empty_refusal_still_says_something() {
        assert_eq!(
            translate_refusal("   \n", 141),
            "GitHub refused the review and said nothing"
        );
    }

    #[test]
    fn a_long_refusal_is_shortened_to_one_readable_line() {
        let long = format!("gh: {}", "x".repeat(1000));
        let translated = translate_refusal(&long, 141);
        assert!(translated.chars().count() <= 401, "{translated}");
        assert!(translated.ends_with('…'));
    }

    #[test]
    fn the_payload_path_is_per_pull_request() {
        let one = payload_path(1);
        let two = payload_path(2);
        assert_ne!(one, two);
        assert!(one.to_string_lossy().contains("-1-"), "{}", one.display());
        assert!(
            one.starts_with(std::env::temp_dir()),
            "written outside the state directory"
        );
    }
}
