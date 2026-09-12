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


def drain(master: int) -> bytes:
    """Reads whatever the pty has to say right now, without blocking."""
    chunks = []
    while True:
        ready, _, _ = select.select([master], [], [], 0)
        if not ready:
            break
        try:
            data = os.read(master, 65536)
        except OSError:
            break
        if not data:
            break
        chunks.append(data)
    return b"".join(chunks)


def wait_for(
    master: int,
    capture: bytearray,
    pattern: str,
    deadline: float,
    cols: int,
    rows: int,
) -> bool:
    """Polls the pty until `pattern` matches the replayed screen, or the deadline."""
    matcher = re.compile(pattern)
    while time.monotonic() < deadline:
        capture.extend(drain(master))
        if matcher.search(
            screen.replay(capture.decode("utf-8", errors="replace"), cols, rows)
        ):
            return True
        time.sleep(0.03)
    capture.extend(drain(master))
    return False


def settle(master: int, capture: bytearray, seconds: float) -> None:
    """Waits for a short quiet period, absorbing anything the pty writes meanwhile."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        capture.extend(drain(master))
        time.sleep(min(0.03, max(0.0, deadline - time.monotonic())))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cols", type=int, default=160)
    parser.add_argument("--rows", type=int, default=40)
    parser.add_argument("--log", required=True, help="where the raw capture is written")
    parser.add_argument("--ready", default="", help="regex to wait for before the first key")
    parser.add_argument("--timeout", type=float, default=60.0, help="overall cap in seconds")
    parser.add_argument("--step-timeout", type=float, default=15.0, help="per-step wait cap")
    parser.add_argument("--settle", type=float, default=0.25, help="quiet period for empty waits")
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
    while len(wait_groups) < len(key_groups):
        wait_groups.append("")

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

    capture = bytearray()
    overall_deadline = time.monotonic() + arguments.timeout
    step_deadline = lambda: min(  # noqa: E731
        overall_deadline, time.monotonic() + arguments.step_timeout
    )

    try:
        if arguments.ready:
            if not wait_for(
                master,
                capture,
                arguments.ready,
                step_deadline(),
                arguments.cols,
                arguments.rows,
            ):
                print(
                    f"drive.py: the ready state {arguments.ready!r} never appeared",
                    file=sys.stderr,
                )

        for keys, pattern in zip(key_groups, wait_groups):
            os.write(master, interpret_keys(keys))
            if pattern:
                if not wait_for(
                    master,
                    capture,
                    pattern,
                    step_deadline(),
                    arguments.cols,
                    arguments.rows,
                ):
                    print(
                        f"drive.py: step pattern {pattern!r} never appeared",
                        file=sys.stderr,
                    )
            else:
                settle(master, capture, arguments.settle)

        # Capture the last frame and the terminal restore, then stop the app if it is
        # still running (a run whose keys do not quit).
        settle(master, capture, arguments.settle)
        if process.poll() is None:
            try:
                process.send_signal(signal.SIGTERM)
            except ProcessLookupError:
                pass  # it exited between the poll and the signal
            try:
                process.wait(timeout=2.0)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        settle(master, capture, 0.1)
    finally:
        if process.poll() is None:
            try:
                process.kill()
            except ProcessLookupError:
                pass
            process.wait()
        try:
            os.close(master)
        except OSError:
            pass

    raw = bytes(capture)
    with open(arguments.log, "wb") as handle:
        handle.write(raw)
    print(screen.replay(raw.decode("utf-8", errors="replace"), arguments.cols, arguments.rows))
    return 0


if __name__ == "__main__":
    sys.exit(main())
