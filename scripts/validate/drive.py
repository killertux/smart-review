#!/usr/bin/env python3
"""Drive `smart-review` in a pty until expected text appears, then print the screen.

The milestone validators used to send key groups separated by fixed sleeps: enough
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

# Importing `screen` must not write a `__pycache__` into the checkout: m1.sh asserts
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


def drain(master: int, capture: Capture) -> tuple[bool, bool]:
    """Reads available PTY output. Returns `(read_data, reached_eof)`."""
    read_data = False
    while True:
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
) -> bool:
    """Waits until `pattern` matches the live screen, or the deadline."""
    matcher = re.compile(pattern)
    if matcher.search(capture.text()):
        return True
    while time.monotonic() < deadline:
        remaining = max(0.0, deadline - time.monotonic())
        ready, _, _ = select.select([master], [], [], min(0.1, remaining))
        if not ready:
            continue
        read_data, reached_eof = drain(master, capture)
        if read_data and matcher.search(capture.text()):
            return True
        if reached_eof:
            return False
    drain(master, capture)
    return False


def settle(master: int, capture: Capture, quiet_seconds: float, deadline: float) -> None:
    """Returns after the PTY has stayed quiet, bounded by `deadline`."""
    quiet_deadline = min(deadline, time.monotonic() + quiet_seconds)
    while time.monotonic() < deadline:
        remaining = max(0.0, quiet_deadline - time.monotonic())
        if remaining == 0:
            return
        ready, _, _ = select.select([master], [], [], remaining)
        if not ready:
            return
        read_data, reached_eof = drain(master, capture)
        if reached_eof:
            return
        if read_data:
            quiet_deadline = min(deadline, time.monotonic() + quiet_seconds)


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
        return 2

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
            if not wait_for(
                master,
                capture,
                arguments.ready,
                step_deadline(),
            ):
                failure = f"the ready state {arguments.ready!r} never appeared"

        for index, (keys, pattern) in enumerate(zip(key_groups, wait_groups), start=1):
            if failure:
                break
            try:
                os.write(master, interpret_keys(keys))
            except OSError as error:
                failure = f"step {index} could not send its keys: {error}"
                break
            if pattern:
                if not wait_for(
                    master,
                    capture,
                    pattern,
                    step_deadline(),
                ):
                    failure = f"step {index} pattern {pattern!r} never appeared"
            else:
                settle(master, capture, arguments.settle, step_deadline())

        # Capture the last frame and the terminal restore, then stop the app if it is
        # still running (a run whose keys do not quit).
        settle(master, capture, arguments.settle, step_deadline())
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
        settle(master, capture, 0.05, time.monotonic() + 0.25)
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
