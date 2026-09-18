#!/usr/bin/env python3
"""A scripted OpenAI-compatible provider for analysis and chat validation.

The point of analysis validation is the path from a diff to an ordered review, and
the only way to validate that path without paying a provider is to be the provider. This
serves just enough of `/chat/completions` — streaming and not — for the `llm` crate's
OpenAI backend to talk to it, and answers from a script:

    usage: fake_llm.py <port> <mode-file> <request-log>

The mode file is read on every request, so the checker can change what the provider
says between one run and the next. Modes:

    good             the analysis the prompt asked for
    prose            prose, which cannot be used (twice, for the repair path)
    prose-then-good  prose on the first attempt, and the analysis when the repair
                     prompt arrives — detected by its own wording, so the fake needs no
                     state to know which attempt it is
    empty            a JSON object that says nothing
    chat             a chat answer: prose, one path reference, one general-knowledge
                     line (chat)
    chat-slow        the same, dribbled out over a second, so the checker can stop it
                     halfway (chat)

Every request body is appended to the log file, one JSON object per line, which is how
the checker asserts what was actually sent: the diff, the file contents, the repository
conventions, and above all what was *not* sent.
"""

import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# The analysis the "good" mode returns. Three properties matter to the checker:
# it names files that are in the diff, it names one that is not (so the warning path
# is exercised), and it puts the two real files in a deliberate order.
GOOD = {
    "summary": "Money now rounds half up, and the invoice total uses it.",
    "intent": "Finance reported a rounding drift on large invoices.",
    "risk_areas": [
        {
            "title": "Rounding in Money arithmetic",
            "severity": "high",
            "files": ["src/domain/money.rs"],
            "why": "the sign of the rounding decides who pays the extra cent",
        },
        {
            "title": "A file that is not in this change",
            "severity": "low",
            "files": ["src/domain/ghost.rs"],
            "why": "the model should not be able to place this",
        },
    ],
    "review_plan": [
        {
            "order": 1,
            "group": "domain",
            "rationale": "the arithmetic everything else depends on",
            "files": ["src/domain/money.rs"],
        },
        {
            "order": 2,
            "group": "tests",
            "rationale": "the tests that pin the new rounding down",
            "files": ["tests/money.rs"],
        },
    ],
    "per_file_notes": [
        {
            "path": "src/domain/money.rs",
            "change": "rounds half up instead of half even",
            "notes": "check the negative case: -0.5 must not round to zero",
            "review_focus": ["negative amounts", "half-cent cases"],
        }
    ],
    "suggested_questions": ["Is the rounding rule documented for finance?"],
}

# A chat answer, with the two things the chat checker looks for: a reference to a file
# that is in the diff (so the pane can say it is jumpable) and a sentence the model
# marked as general knowledge rather than as coming from the context (FR-5.3).
CHAT = "\n\n".join(
    [
        "The rounding changed in `src/domain/money.rs`: it now adds five cents before "
        "dividing, which rounds half up for positive amounts.",
        "The negative case is the one to check: `src/domain/money.rs` has no sign handling.",
        "[general] Rust's integer division truncates toward zero, so the sign follows the "
        "numerator.",
        "If you want the invoice total checked, add `src/domain/invoice.rs` to the context: "
        "it is not in this change.",
    ]
)

PROSE = (
    "I looked at the diff. Money now rounds half up, and the invoice total follows "
    "it. The main risk is the sign of the rounding. Let me know if you want more."
)


def scripted(mode: str, prompt: str) -> str:
    """What the provider says for this request."""
    if mode in ("chat", "chat-slow"):
        return chat_answer(prompt)
    if mode == "prose":
        return PROSE
    if mode == "prose-then-good":
        # The repair prompt quotes the failure, so its own wording is the signal: no
        # state, and no way for the two attempts to be confused.
        if "could not be used" in prompt:
            return json.dumps(GOOD)
        return PROSE
    if mode == "empty":
        return "{}"
    return json.dumps(GOOD)


def chat_answer(prompt: str) -> str:
    """A chat answer that quotes the question it is answering.

    The quote is not decoration: the checker has to tell one answer from another, and a
    scripted provider that says the same thing every time makes "the answer to *this*
    question arrived" unobservable — the previous answer's text is still on screen.
    """
    asked = ""
    for line in reversed(prompt.splitlines()):
        if line.startswith("user:"):
            asked = line.removeprefix("user:").strip()[:40]
            break
    return f'You asked: "{asked}". ' + CHAT


def message_text(content) -> str:
    """The text of one message.

    OpenAI-compatible providers accept either a string or a list of content parts,
    and the crate sends the list form, so both have to be readable: a checker that
    only understood one shape would answer nothing and the app would wait for a reply
    that never came.
    """
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for part in content:
            if isinstance(part, dict):
                parts.append(part.get("text") or "")
            elif isinstance(part, str):
                parts.append(part)
        return " ".join(parts)
    return ""


def answer_for(body: dict, mode: str) -> str:
    """The text of the answer, for the messages in this request.

    The transcript is labelled with roles rather than joined blindly: a chat answer that
    quotes the question has to be able to find it, and "the last thing the user said" is
    not recoverable from a bag of words.
    """
    prompt = "\n".join(
        f"{message.get('role', '?')}: {message_text(message.get('content'))}"
        for message in body.get("messages", [])
    )
    return scripted(mode, prompt)


class Handler(BaseHTTPRequestHandler):
    """Serves `/chat/completions` and records everything it is sent."""

    protocol_version = "HTTP/1.1"
    mode_file = ""
    log_file = ""
    lock = threading.Lock()

    def log_message(self, *_args) -> None:  # noqa: ANN002
        """Silence the default access log: it is noise on the checker's output."""

    @classmethod
    def mode(cls) -> str:
        """What the checker has asked for right now.

        Deliberately lock-free: every caller already holds `cls.lock`, and taking it
        twice here would deadlock the server on the first request — with the app waiting
        for an answer that can never come, which looks exactly like a product bug.
        """
        return open(cls.mode_file, encoding="utf-8").read().strip() or "good"

    def do_POST(self) -> None:  # noqa: N802 - the base class names it
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length).decode("utf-8", "replace")
        try:
            body = json.loads(raw)
        except json.JSONDecodeError:
            body = {"unparseable": raw}

        with self.lock:
            with open(self.log_file, "a", encoding="utf-8") as handle:
                # The mode is recorded with the request: "what did the provider say"
                # and "what did it say it because of" are one question when a check
                # fails, and reconstructing it from the order of the steps is guesswork.
                handle.write(
                    json.dumps(
                        {"path": self.path, "mode": self.mode(), "body": body},
                        sort_keys=True,
                    )
                    + "\n"
                )
            mode = self.mode()

        answer = answer_for(body, mode)
        if body.get("stream"):
            self.stream(answer, slow=mode == "chat-slow")
        else:
            self.json(answer)

    def stream(self, answer: str, slow: bool = False) -> None:
        """Server-sent events, in several chunks, as a real provider does.

        `slow` dribbles the answer out over about a second, which is what gives the
        chat checker a window in which to press `Esc`: a stream that finishes in one
        millisecond cannot be cancelled, and a cancellation nobody can test is a
        cancellation nobody knows works.
        """
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        # Deliberately split mid-token: the interface must join the pieces rather than
        # assume one chunk is a whole answer.
        step = max(1, len(answer) // 7)
        for start in range(0, len(answer), step):
            chunk = answer[start : start + step]
            payload = {
                "id": "chatcmpl-fake",
                "object": "chat.completion.chunk",
                "model": "fake-analysis-1",
                "choices": [{"index": 0, "delta": {"content": chunk}}],
            }
            self.wfile.write(f"data: {json.dumps(payload)}\n\n".encode())
            self.wfile.flush()
            if slow:
                time.sleep(0.25)
        usage = {
            "id": "chatcmpl-fake",
            "object": "chat.completion.chunk",
            "model": "fake-analysis-1",
            "choices": [],
            "usage": {
                "prompt_tokens": 1234,
                "completion_tokens": 567,
                "total_tokens": 1801,
                "completion_tokens_details": {"reasoning_tokens": 89},
            },
        }
        self.wfile.write(f"data: {json.dumps(usage)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def json(self, answer: str) -> None:
        """A plain completion, which is what the connection check asks for."""
        payload = {
            "id": "chatcmpl-fake",
            "object": "chat.completion",
            "model": "fake-analysis-1",
            "choices": [
                {
                    "index": 0,
                    "message": {"role": "assistant", "content": answer},
                    "finish_reason": "stop",
                }
            ],
            "usage": {"prompt_tokens": 9, "completion_tokens": 3, "total_tokens": 12},
        }
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__)
        return 2
    port, Handler.mode_file, Handler.log_file = sys.argv[1], sys.argv[2], sys.argv[3]
    server = ThreadingHTTPServer(("127.0.0.1", int(port)), Handler)
    server.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
