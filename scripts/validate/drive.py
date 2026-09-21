#!/usr/bin/env python3
"""Drive `smart-review` in a pty until expected text appears, then print the screen.

The feature validators used to send key groups separated by fixed sleeps: enough
pause that a background job (opening a pull request, fetching the catalog, streaming
an analysis) had surely finished. That is slow — every step costs its sleep even when
the interface answered in a few hundred milliseconds — and it made the non-quitting
runs sit until `timeout` killed them, because nothing else did.

This driver replaces both problems. A run is a sequence of steps; each step sends a
group of keys and then waits — polling the replayed screen — until the step's regular
expression appears, up to a per-step deadline. The next key is therefore sent as soon
as the interface actually reached the state it was supposed to reach, and the run
ends the moment the last state is visible, quitting or killing the app as needed.

The full raw capture is still written to `--log`, so `screen.py --when` can keep
replaying to the moment a transient popup was on screen: checks that assert on a panel
that `:q` closes are unchanged.

Usage:
  drive.py --cols 160 --rows 40 --log capture.log \
           [--ready REGEX] [--timeout SECS] [--settle SECS] \
           [--resize-steps '80x24=REGEX~160x40=REGEX'] \
           --keys 'g1~g2~g3' --waits 'p1~p2~p3' \
           -- cmd args...

`--keys` and `--waits` are `~`-separated lists of the same length. A step whose wait
is empty just settles briefly. Key groups use the same escapes `printf '%b'` did:
`\\r`, `\\n`, `\\t`, `\\e`/`\\033`, and `\\` for a literal backslash.
"""

import argparse
import codecs
import fcntl
import os
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

# Importing `screen` must not write a `__pycache__` into the checkout: pull-requests.sh asserts
# that the validation run leaves the repository untouched.
sys.dont_write_bytecode = True

import screen  # noqa: E402  (same directory)


def interpret_keys(text: str) -> bytes:
    """Turns a key group into the bytes to write, like `printf '%b'` did."""
    output = bytearray()
    index = 0
    while index < len(text):
        character = text[index]
        if character != "\\":
            output += character.encode()
            index += 1
            continue
        index += 1
        if index >= len(text):
            output += b"\\"
            break
        escape = text[index]
        index += 1
        if escape == "r":
            output += b"\r"
        elif escape == "n":
            output += b"\n"
        elif escape == "t":
            output += b"\t"
        elif escape == "b":
            output += b"\b"
        elif escape == "e":
            output += b"\x1b"
        elif escape == "\\":
            output += b"\\"
        elif escape in "01234567":
            digits = escape
            while index < len(text) and len(digits) < 3 and text[index] in "01234567":
                digits += text[index]
                index += 1
            output.append(int(digits, 8) & 0xFF)
        else:
            # Unknown escape: keep it literally rather than guessing.
            output += b"\\" + escape.encode()
    return bytes(output)


class Capture:
    """The raw capture and its incrementally reconstructed terminal screen."""

    def __init__(self, cols: int, rows: int) -> None:
        self.raw = bytearray()
        self.decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        self.replay = screen.Replay(cols, rows)

    def add(self, data: bytes) -> None:
        self.raw.extend(data)
        self.replay.feed(self.decoder.decode(data))

    def finish(self) -> None:
        self.replay.feed(self.decoder.decode(b"", final=True), final=True)
        self.replay.finish()

    def text(self) -> str:
        return self.replay.text()

    @property
    def revision(self) -> int:
        return self.replay.screen.revision

    def matches_since(self, matcher: re.Pattern[str], revision: int | None) -> bool:
        if revision is None:
            return matcher.search(self.text()) is not None
        return self.replay.screen.pattern_visible_since(matcher, revision)


def drain(master: int, capture: Capture) -> tuple[bool, bool]:
    """Reads available PTY output. Returns `(read_data, reached_eof)`."""
    read_data = False
    # A noisy child must not keep this helper inside one unbounded drain while the
    # global/step deadline and keyboard dispatch wait outside it.
    for _ in range(64):
        ready, _, _ = select.select([master], [], [], 0)
        if not ready:
            break
        try:
            data = os.read(master, 65536)
        except OSError:
            return read_data, True
        if not data:
            return read_data, True
        capture.add(data)
        read_data = True
    return read_data, False


def wait_for(
    master: int,
    capture: Capture,
    pattern: str,
    deadline: float,
    *,
    since_revision: int | None = None,
) -> tuple[bool, bool]:
    """Waits until `pattern` matches the live screen, or the deadline."""
    matcher = re.compile(pattern)
    if capture.matches_since(matcher, since_revision):
        return True, False
    while time.monotonic() < deadline:
        remaining = max(0.0, deadline - time.monotonic())
        ready, _, _ = select.select([master], [], [], min(0.1, remaining))
        if not ready:
            continue
        read_data, reached_eof = drain(master, capture)
        if read_data and capture.matches_since(matcher, since_revision):
            return True, reached_eof
        if reached_eof:
            return False, True
    drain(master, capture)
    return capture.matches_since(matcher, since_revision), False


def settle(master: int, capture: Capture, quiet_seconds: float, deadline: float) -> tuple[bool, bool]:
    """Returns `(became_quiet, reached_eof)` under `deadline`."""
    quiet_deadline = min(deadline, time.monotonic() + quiet_seconds)
    while time.monotonic() < deadline:
        remaining = max(0.0, quiet_deadline - time.monotonic())
        if remaining == 0:
            return quiet_deadline < deadline, False
        ready, _, _ = select.select([master], [], [], remaining)
        if not ready:
            return quiet_deadline < deadline, False
        read_data, reached_eof = drain(master, capture)
        if reached_eof:
            return True, True
        if read_data:
            quiet_deadline = min(deadline, time.monotonic() + quiet_seconds)
    return False, False


def wait_for_exit(
    master: int,
    capture: Capture,
    process: subprocess.Popen[bytes],
    deadline: float,
) -> bool:
    """Waits for the owned child while draining its final terminal output."""
    reached_eof = False
    while time.monotonic() < deadline:
        if process.poll() is not None and reached_eof:
            return True
        remaining = max(0.0, deadline - time.monotonic())
        ready, _, _ = select.select([master], [], [], min(0.05, remaining))
        if ready:
            _, reached_eof = drain(master, capture)
        elif process.poll() is not None:
            # Some PTYs report EOF only after one final non-blocking read.
            _, reached_eof = drain(master, capture)
            if reached_eof:
                return True
    drain(master, capture)
    return process.poll() is not None


def wait_for_exit_or_quiet(
    master: int,
    capture: Capture,
    process: subprocess.Popen[bytes],
    quiet_seconds: float,
    deadline: float,
) -> tuple[bool, bool]:
    """Returns `(exited, became_quiet)` while continuously draining output."""
    quiet_deadline = min(deadline, time.monotonic() + quiet_seconds)
    while time.monotonic() < deadline:
        if process.poll() is not None:
            return wait_for_exit(master, capture, process, deadline), False
        remaining = max(0.0, min(quiet_deadline, deadline) - time.monotonic())
        if remaining == 0:
            return False, quiet_deadline < deadline
        ready, _, _ = select.select([master], [], [], remaining)
        if not ready:
            return False, quiet_deadline < deadline
        read_data, reached_eof = drain(master, capture)
        if reached_eof and process.poll() is not None:
            return True, False
        if read_data:
            quiet_deadline = min(deadline, time.monotonic() + quiet_seconds)
    return process.poll() is not None, False


def resize_steps(text: str) -> list[tuple[int, int, str]]:
    """Parses `COLSxROWS=REGEX` groups used to exercise SIGWINCH wiring."""
    if not text:
        return []
    steps = []
    for group in text.split("~"):
        size, separator, pattern = group.partition("=")
        match = re.fullmatch(r"([1-9][0-9]*)x([1-9][0-9]*)", size)
        if not separator or match is None:
            raise ValueError(
                "--resize-steps entries must be COLSxROWS=REGEX, separated by `~`"
            )
        steps.append((int(match.group(1)), int(match.group(2)), pattern))
    return steps


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cols", type=int, default=160)
    parser.add_argument("--rows", type=int, default=40)
    parser.add_argument("--log", required=True, help="where the raw capture is written")
    parser.add_argument("--ready", default="", help="regex to wait for before the first key")
    parser.add_argument("--timeout", type=float, default=60.0, help="overall cap in seconds")
    parser.add_argument("--step-timeout", type=float, default=15.0, help="per-step wait cap")
    parser.add_argument(
        "--settle",
        type=float,
        default=0.1,
        help="required PTY idle period for empty waits (not a fixed sleep)",
    )
    parser.add_argument("--keys", default="", help="`~`-separated key groups")
    parser.add_argument("--waits", default="", help="`~`-separated regexes, one per key group")
    parser.add_argument(
        "--resize-steps",
        default="",
        help="`~`-separated COLSxROWS=REGEX resizes performed after readiness",
    )
    parser.add_argument(
        "--step-notify",
        help="write step-N.sent files here after each key group (fixture coordination)",
    )
    parser.add_argument("command", nargs=argparse.REMAINDER)
    arguments = parser.parse_args()

    # `REMAINDER` keeps the `--` separator, so drop it before treating the rest as
    # the command.
    if arguments.command and arguments.command[0] == "--":
        arguments.command = arguments.command[1:]
    if not arguments.command:
        print("drive.py: no command given", file=sys.stderr)
        return 2

    key_groups = arguments.keys.split("~") if arguments.keys else []
    wait_groups = arguments.waits.split("~") if arguments.waits else []
    if key_groups and not wait_groups:
        wait_groups = [""] * len(key_groups)
    if len(wait_groups) != len(key_groups):
        print(
            "drive.py: --keys and --waits must contain the same number of groups "
            f"({len(key_groups)} keys, {len(wait_groups)} waits)",
            file=sys.stderr,
        )
        try:
            with open(arguments.log, "wb"):
                pass
        except OSError:
            pass
        return 2
    try:
        resizes = resize_steps(arguments.resize_steps)
    except ValueError as error:
        print(f"drive.py: {error}", file=sys.stderr)
        try:
            with open(arguments.log, "wb"):
                pass
        except OSError:
            pass
        return 2

    if arguments.step_notify:
        os.makedirs(arguments.step_notify, exist_ok=True)

    master, slave = os.openpty()
    fcntl.ioctl(
        slave,
        termios.TIOCSWINSZ,
        struct.pack("HHHH", arguments.rows, arguments.cols, 0, 0),
    )

    process = subprocess.Popen(
        arguments.command,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        close_fds=True,
        env=os.environ,
        start_new_session=True,
    )
    os.close(slave)

    capture = Capture(arguments.cols, arguments.rows)
    overall_deadline = time.monotonic() + arguments.timeout
    step_deadline = lambda: min(  # noqa: E731
        overall_deadline, time.monotonic() + arguments.step_timeout
    )

    failure = ""
    try:
        if arguments.ready:
            matched, _ = wait_for(
                master,
                capture,
                arguments.ready,
                step_deadline(),
            )
            if not matched:
                failure = f"the ready state {arguments.ready!r} never appeared"

        for index, (cols, rows, pattern) in enumerate(resizes, start=1):
            if failure:
                break
            before_revision = capture.revision
            fcntl.ioctl(
                master,
                termios.TIOCSWINSZ,
                struct.pack("HHHH", rows, cols, 0, 0),
            )
            capture.replay.resize(cols, rows)
            matched, reached_eof = wait_for(
                master,
                capture,
                pattern,
                step_deadline(),
                since_revision=before_revision,
            )
            if not matched:
                failure = (
                    f"resize step {index} ({cols}x{rows}) pattern {pattern!r} never appeared"
                )
            if reached_eof and not failure:
                failure = f"the child exited during resize step {index}"
            if not failure:
                # A visible frame proves the resize was handled, but its final writes
                # may still be queued. Serialise acknowledgements so a following
                # TIOCSWINSZ cannot be coalesced with work from this one.
                quiet, reached_eof = settle(
                    master, capture, arguments.settle, step_deadline()
                )
                if reached_eof:
                    failure = f"the child exited after resize step {index}"
                elif not quiet:
                    failure = (
                        f"resize step {index} ({cols}x{rows}) did not become quiet "
                        "before its deadline"
                    )

        for index, (keys, pattern) in enumerate(zip(key_groups, wait_groups), start=1):
            if failure:
                break
            before_revision = capture.revision
            try:
                os.write(master, interpret_keys(keys))
            except OSError as error:
                failure = f"step {index} could not send its keys: {error}"
                break
            if arguments.step_notify:
                marker = os.path.join(arguments.step_notify, f"step-{index}.sent")
                with open(marker, "w", encoding="utf-8") as handle:
                    handle.write(f"{time.monotonic()}\n")
            if pattern:
                matched, reached_eof = wait_for(
                    master,
                    capture,
                    pattern,
                    step_deadline(),
                    since_revision=before_revision,
                )
                if not matched:
                    failure = f"step {index} pattern {pattern!r} never appeared"
            else:
                quiet, reached_eof = settle(
                    master, capture, arguments.settle, step_deadline()
                )
                if not quiet and time.monotonic() >= overall_deadline:
                    failure = "the global timeout expired while waiting for terminal output to settle"

            code = process.poll()
            if reached_eof and index < len(key_groups) and not failure:
                failure = f"the child exited with status {code} before step {index + 1}"
            elif code not in (None, 0) and not failure:
                failure = f"the child exited with status {code}"
            if time.monotonic() >= overall_deadline and not failure:
                failure = "the global timeout expired"

        # Give a quitting child a bounded opportunity to restore the terminal and
        # drain its final frame. Runs intentionally ending on an open modal are then
        # stopped through their owned process group rather than a broad process match.
        exited, quiet = wait_for_exit_or_quiet(
            master,
            capture,
            process,
            arguments.settle,
            step_deadline(),
        )
        if not exited and not quiet and time.monotonic() >= overall_deadline and not failure:
            failure = "the global timeout expired while collecting the final frame"
        code = process.poll()
        if code not in (None, 0) and not failure:
            failure = f"the child exited with status {code}"
        if process.poll() is None:
            try:
                # The child owns a fresh session. Signal that process group so a
                # cancelled validator cannot leave a fake `gh` or editor behind.
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass  # it exited between the poll and the signal
            try:
                # IR-14 gives durable state and draft writes up to two seconds to
                # finish during orderly shutdown. Allow that contract to complete
                # before treating the application as stuck and force-killing it.
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
        wait_for_exit(master, capture, process, time.monotonic() + 0.25)
    finally:
        if process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
        try:
            os.close(master)
        except OSError:
            pass

    capture.finish()
    raw = bytes(capture.raw)
    with open(arguments.log, "wb") as handle:
        handle.write(raw)
    print(capture.text())
    if failure:
        print(f"drive.py: {failure}; capture: {arguments.log}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
