#!/usr/bin/env python3
"""Regression tests for the incremental terminal replay and PTY driver (IR-15, IR-18)."""

from __future__ import annotations

import codecs
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.dont_write_bytecode = True

import screen  # noqa: E402
import drive  # noqa: E402


REPRESENTATIVE = (
    b"\x1b[?1049h\x1b[2J\x1b[H"
    b"smart-review \xe2\x9c\x93\r\n"
    b"\x1b[31mred\x1b[0m\x1b]0;ignored title\x07"
    b"\x1b[3;4Hwide: \xe6\xbc\xa2\x1b7moved\x1b8!"
    b"\x1bPignored payload\x1b\\\x1b[?1049l"
)


def replay_bytes(parts: list[bytes]) -> str:
    decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
    replay = screen.Replay(40, 8)
    for part in parts:
        replay.feed(decoder.decode(part))
    replay.feed(decoder.decode(b"", final=True), final=True)
    replay.finish()
    return replay.text()


class ReplayTests(unittest.TestCase):
    def test_ir_15_every_two_chunk_split_matches_whole_replay(self) -> None:
        expected = replay_bytes([REPRESENTATIVE])
        for split in range(len(REPRESENTATIVE) + 1):
            with self.subTest(split=split):
                self.assertEqual(
                    replay_bytes([REPRESENTATIVE[:split], REPRESENTATIVE[split:]]),
                    expected,
                )

    def test_ir_15_byte_at_a_time_preserves_utf8_and_escape_state(self) -> None:
        self.assertEqual(
            replay_bytes([REPRESENTATIVE[index : index + 1] for index in range(len(REPRESENTATIVE))]),
            replay_bytes([REPRESENTATIVE]),
        )

    def test_ir_15_resize_preserves_the_visible_intersection(self) -> None:
        replay = screen.Replay(8, 2)
        replay.feed("first\r\nsecond")
        replay.resize(5, 3)
        self.assertEqual(replay.text(), "first\nsecon\n")

    def test_ir_15_replay_until_uses_the_incremental_parser(self) -> None:
        raw = REPRESENTATIVE.decode("utf-8")
        found = screen.replay_until(raw, 40, 8, "smart-review ✓")
        self.assertIsNotNone(found)
        self.assertIn("smart-review ✓", found or "")

    def test_ir_15_reset_inside_alternate_screen_starts_a_clean_frame(self) -> None:
        replay = screen.Replay(20, 3)
        replay.feed("\x1b[?1049hold\x1bcnew\x1b[?1049l", final=True)
        self.assertEqual(replay.text().splitlines()[0], "new")


class DriverTests(unittest.TestCase):
    def test_ir_18_resize_steps_are_explicit_and_validated(self) -> None:
        self.assertEqual(
            drive.resize_steps("79x23=terminal too small~160x40=smart-review"),
            [(79, 23, "terminal too small"), (160, 40, "smart-review")],
        )
        with self.assertRaises(ValueError):
            drive.resize_steps("79-by-23")

    def test_ir_18_consecutive_resizes_are_acknowledged_before_the_next(self) -> None:
        child = r'''
import fcntl, os, signal, struct, sys, termios, time, tty
tty.setraw(0)

def size():
    rows, cols, _, _ = struct.unpack("HHHH", fcntl.ioctl(0, termios.TIOCGWINSZ, b"\0" * 8))
    return cols, rows

seen = 0
last = size()

def resized(_signum, _frame):
    global seen, last
    current = size()
    if current == last:
        return
    last = current
    seen += 1
    os.write(1, f"\x1b[2J\x1b[HSIZE {last[0]}x{last[1]}".encode())

signal.signal(signal.SIGWINCH, resized)
os.write(1, b"\x1b[2J\x1b[HREADY")
deadline = time.monotonic() + 2
while seen < 2 and time.monotonic() < deadline:
    time.sleep(0.005)
if seen != 2:
    raise SystemExit(3)
open(sys.argv[1], "w", encoding="utf-8").write(f"{last[0]}x{last[1]}")
os.read(0, 1)
'''
        result, capture, received = self.run_driver(
            child,
            "--ready",
            "READY",
            "--step-timeout",
            "1",
            "--settle",
            "0.03",
            "--resize-steps",
            "20x6=SIZE 20x6~30x7=SIZE 30x7",
            "--keys",
            "x",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(received.read_text(encoding="utf-8"), "30x7")
        raw = capture.read_text(encoding="utf-8", errors="replace")
        self.assertIn("SIZE 20x6", raw)
        self.assertIn("SIZE 30x7", raw)

    def run_driver(
        self,
        child: str,
        *arguments: str,
        timeout: float = 5,
    ) -> tuple[subprocess.CompletedProcess[str], Path, Path]:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        capture = root / "capture.log"
        received = root / "received.txt"
        command = [
            sys.executable,
            str(HERE / "drive.py"),
            "--cols",
            "40",
            "--rows",
            "8",
            "--log",
            str(capture),
            *arguments,
            "--",
            sys.executable,
            "-c",
            child,
            str(received),
        ]
        result = subprocess.run(command, text=True, capture_output=True, timeout=timeout)
        return result, capture, received

    def test_ir_15_unmet_wait_fails_and_stops_later_keys(self) -> None:
        child = r'''
import os, sys, termios, tty
tty.setraw(0)
os.write(1, b"\x1b[2J\x1b[HREADY generic success")
first = os.read(0, 1)
open(sys.argv[1], "wb").write(first)
os.write(1, b"\r\nstill generic success")
try:
    second = os.read(0, 1)
    open(sys.argv[1], "ab").write(second)
except OSError:
    pass
'''
        result, capture, received = self.run_driver(
            child,
            "--ready",
            "READY",
            "--step-timeout",
            "0.2",
            "--timeout",
            "1",
            "--keys",
            "x~y",
            "--waits",
            "impossible~generic success",
        )
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(received.read_bytes(), b"x")
        self.assertTrue(capture.exists())
        self.assertIn("step 1", result.stderr)

    def test_ir_15_unmet_ready_wait_fails(self) -> None:
        child = "import os, time; os.write(1, b'NOT-READY'); time.sleep(2)"
        result, capture, received = self.run_driver(
            child,
            "--ready",
            "EXPECTED",
            "--step-timeout",
            "0.2",
            "--timeout",
            "1",
        )
        self.assertEqual(result.returncode, 1)
        self.assertTrue(capture.exists())
        self.assertIn("ready state", result.stderr)

    def test_ir_15_stale_text_does_not_satisfy_a_later_wait(self) -> None:
        child = r'''
import os, sys, tty
tty.setraw(0)
os.write(1, b"\x1b[2J\x1b[HREADY STALE")
os.read(0, 1)
os.write(1, b"\x1b[2;1Hchanged elsewhere")
os.read(0, 1)
'''
        result, _, _ = self.run_driver(
            child,
            "--ready",
            "READY",
            "--step-timeout",
            "0.2",
            "--timeout",
            "1",
            "--keys",
            "x~y",
            "--waits",
            "STALE~changed elsewhere",
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("step 1", result.stderr)

    def test_ir_15_unexpected_child_exit_is_a_failure(self) -> None:
        child = "import os; os.write(1, b'READY'); raise SystemExit(7)"
        result, capture, _ = self.run_driver(
            child,
            "--ready",
            "READY",
            "--timeout",
            "1",
        )
        self.assertEqual(result.returncode, 1)
        self.assertTrue(capture.exists())
        self.assertIn("status 7", result.stderr)

    def test_ir_15_graceful_exit_drains_terminal_restore(self) -> None:
        child = r'''
import os, time, tty
tty.setraw(0)
os.write(1, b"\x1b[?1049hREADY")
os.read(0, 1)
os.write(1, b"closing")
time.sleep(0.03)
os.write(1, b"\x1b[?1049lRESTORED")
'''
        result, capture, _ = self.run_driver(
            child,
            "--ready",
            "READY",
            "--settle",
            "0.1",
            "--keys",
            "x",
            "--waits",
            "closing",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(b"\x1b[?1049lRESTORED", capture.read_bytes())

    def test_ir_15_mismatched_groups_fail_before_launch(self) -> None:
        child = "raise SystemExit(0)"
        result, capture, received = self.run_driver(
            child,
            "--keys",
            "x~y",
            "--waits",
            "one",
        )
        self.assertEqual(result.returncode, 2)
        self.assertTrue(capture.exists())
        self.assertFalse(received.exists(), "the child must not launch for an invalid scenario")
        self.assertIn("same number", result.stderr)

    def test_ir_15_global_timeout_fails_a_noisy_child(self) -> None:
        child = r'''
import os, time
os.write(1, b"READY")
while True:
    os.write(1, b".")
    time.sleep(0.01)
'''
        result, _, _ = self.run_driver(
            child,
            "--ready",
            "READY",
            "--timeout",
            "0.2",
            "--settle",
            "0.05",
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("global timeout", result.stderr)

    def test_ir_15_concurrent_drivers_keep_their_captures_separate(self) -> None:
        child = r'''
import os, sys, time
os.write(1, ("READY " + sys.argv[1]).encode())
time.sleep(0.05)
'''
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            processes = []
            for index in range(2):
                capture = root / f"capture-{index}.log"
                marker = root / f"marker-{index}"
                processes.append(
                    subprocess.Popen(
                        [
                            sys.executable,
                            str(HERE / "drive.py"),
                            "--log",
                            str(capture),
                            "--ready",
                            "READY",
                            "--",
                            sys.executable,
                            "-c",
                            child,
                            str(marker),
                        ],
                        stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                        text=True,
                    )
                )
            results = [process.communicate(timeout=5) for process in processes]
            self.assertEqual([process.returncode for process in processes], [0, 0], results)
            self.assertIn("marker-0", (root / "capture-0.log").read_text(errors="replace"))
            self.assertNotIn("marker-1", (root / "capture-0.log").read_text(errors="replace"))
            self.assertIn("marker-1", (root / "capture-1.log").read_text(errors="replace"))


class ValidatorModeTests(unittest.TestCase):
    def test_ir_15_validators_are_named_for_features_not_milestones(self) -> None:
        expected = {
            "shell.sh",
            "pull-requests.sh",
            "workspace-models.sh",
            "analysis.sh",
            "chat.sh",
            "review-publishing.sh",
            "review-collaboration.sh",
        }
        self.assertTrue(expected.issubset({path.name for path in HERE.glob("*.sh")}))
        self.assertEqual(list(HERE.glob("m[0-9]*.sh")), [])
        aggregate = (HERE / "all.sh").read_text(encoding="utf-8")
        for filename in expected:
            self.assertIn(filename.removesuffix(".sh"), aggregate)

    def test_ir_15_scenarios_only_mode_marks_cargo_gates_prebuilt(self) -> None:
        result = subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; validation_mode --scenarios-only; printf %s "$SMART_REVIEW_SKIP_CARGO"',
                "bash",
                str(HERE / "common.sh"),
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "1")

    def test_ir_18_smoke_only_mode_selects_the_small_pty_contracts(self) -> None:
        result = subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; validation_mode --smoke-only; printf "%s:%s" '
                '"$SMART_REVIEW_SKIP_CARGO" "$SMART_REVIEW_SMOKE_ONLY"',
                "bash",
                str(HERE / "common.sh"),
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "1:1")

    def test_ir_18_default_aggregate_runs_smoke_not_historical_repetition(self) -> None:
        aggregate = (HERE / "all.sh").read_text(encoding="utf-8")
        self.assertIn('VALIDATOR_MODE="--smoke-only"', aggregate)
        self.assertIn("SMART_REVIEW_FULL_VALIDATION", aggregate)
        self.assertNotIn('bash "$ROOT/scripts/validate/$feature.sh" --scenarios-only', aggregate)

    def test_ir_15_unknown_validator_mode_fails(self) -> None:
        result = subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; validation_mode --unknown',
                "bash",
                str(HERE / "common.sh"),
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)


if __name__ == "__main__":
    unittest.main()
