#!/usr/bin/env python3
"""Replays a captured terminal stream into a grid and prints the final screen.

A pty capture is every frame concatenated, and `ratatui` only writes the cells that
changed, so grepping the stream text matches fragments that were never on screen at
the same time. Replaying the escape sequences reconstructs what the user actually
saw at the end, which is what the validation checks want to assert on.

Two modes:

- the default prints the **final** screen, which is what a check about the tree, the
  status line or the tabs wants;
- `--when PATTERN` prints the screen **at the moment the pattern was on it**, which is
  what a check about a popup wants: a popup is usually closed by the keystrokes that
  quit the app, and its text cannot be grepped out of the raw stream because ratatui
  writes only the cells that changed — the spaces between words are often never
  written at all.

Usage: screen.py [--cols N] [--rows N] [--when PATTERN] < captured.log
"""

import argparse
import re
import sys

# ECMA-48 final bytes span `@` through `~`; most terminal controls use a letter,
# while keys and a few controls end in `~`. Unrecognised finals are harmlessly ignored
# by `Screen.csi`, but they still have to be consumed as one sequence.
CSI = re.compile(r"\x1b\[([0-9;?]*)([@-~])")
OSC = re.compile(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")


class Screen:
    def __init__(self, cols: int, rows: int) -> None:
        self.cols = cols
        self.rows = rows
        self.grid = [[" "] * cols for _ in range(rows)]
        self.row = 0
        self.col = 0

    def clear(self) -> None:
        self.grid = [[" "] * self.cols for _ in range(self.rows)]
        self.row = 0
        self.col = 0

    def put(self, character: str) -> None:
        if character == "\n":
            self.row = min(self.row + 1, self.rows - 1)
            return
        if character == "\r":
            self.col = 0
            return
        if character == "\b":
            self.col = max(self.col - 1, 0)
            return
        if character == "\t":
            self.col = min((self.col // 8 + 1) * 8, self.cols - 1)
            return
        if character < " ":
            return
        if self.col >= self.cols:
            # Autowrap: the next line, as a terminal with DECAWM on would do.
            self.col = 0
            self.row = min(self.row + 1, self.rows - 1)
        self.grid[self.row][self.col] = character
        self.col += 1

    def csi(self, params: str, final: str) -> None:
        numbers = [int(part) for part in params.replace("?", "").split(";") if part.isdigit()]
        first = numbers[0] if numbers else 0
        if final in "Hf":
            self.row = max(0, min((numbers[0] if numbers else 1) - 1, self.rows - 1))
            self.col = max(0, min((numbers[1] if len(numbers) > 1 else 1) - 1, self.cols - 1))
        elif final == "A":
            self.row = max(self.row - max(first, 1), 0)
        elif final == "B":
            self.row = min(self.row + max(first, 1), self.rows - 1)
        elif final == "C":
            self.col = min(self.col + max(first, 1), self.cols - 1)
        elif final == "D":
            self.col = max(self.col - max(first, 1), 0)
        elif final == "G":
            self.col = max(0, min((first or 1) - 1, self.cols - 1))
        elif final == "J":
            if first == 2 or first == 3:
                self.clear()
            elif first == 0:
                for column in range(self.col, self.cols):
                    self.grid[self.row][column] = " "
                for row in range(self.row + 1, self.rows):
                    self.grid[row] = [" "] * self.cols
        elif final == "K":
            if first == 0:
                for column in range(self.col, self.cols):
                    self.grid[self.row][column] = " "
            elif first == 1:
                for column in range(0, self.col + 1):
                    self.grid[self.row][column] = " "
            else:
                self.grid[self.row] = [" "] * self.cols

    def text(self) -> str:
        return "\n".join("".join(row).rstrip() for row in self.grid)


class Replay:
    """Incrementally applies terminal output to one screen.

    PTY reads may split an escape sequence between two chunks. `pending` keeps that
    incomplete suffix until the next read, so the live driver does not need to replay
    its whole capture after every poll.
    """

    def __init__(self, cols: int, rows: int) -> None:
        self.screen = Screen(cols, rows)
        self.pending = ""

    def feed(self, data: str, *, final: bool = False) -> None:
        data = self.pending + data
        self.pending = ""
        index = 0
        while index < len(data):
            character = data[index]
            if character != "\x1b":
                self.screen.put(character)
                index += 1
                continue

            remainder = data[index:]
            match = CSI.match(remainder)
            if match:
                self.screen.csi(match.group(1), match.group(2))
                index += match.end()
                continue
            osc = OSC.match(remainder)
            if osc:
                self.pending = ""
                index += osc.end()
                continue

            # A CSI or OSC can be split anywhere by a PTY read. Keep it for the next
            # chunk unless this is the final feed; an unknown complete escape remains
            # the same harmless two-character sequence the old replay ignored.
            if not final and (
                remainder == "\x1b"
                or remainder.startswith("\x1b[")
                or remainder.startswith("\x1b]")
            ):
                self.pending = remainder
                return
            index += min(2, len(remainder))

    def finish(self) -> None:
        if self.pending:
            pending = self.pending
            self.pending = ""
            self.feed(pending, final=True)

    def text(self) -> str:
        return self.screen.text()


def replay(data: str, cols: int, rows: int) -> str:
    replayed = Replay(cols, rows)
    replayed.feed(data, final=True)
    replayed.finish()
    return replayed.text()


def replay_until(raw: str, cols: int, rows: int, pattern: str) -> str | None:
    """The screen as it was the first time `pattern` was on it.

    The pattern is checked after every character, because a popup can be drawn and
    closed inside one keystroke's worth of frames. Only the check is repeated; the
    screen is rendered when the last character of a possible match has just been
    written, which keeps this linear in the size of the capture.
    """
    matcher = re.compile(pattern)
    screen = Screen(cols, rows)
    index = 0
    tail = pattern[-1]
    while index < len(raw):
        character = raw[index]
        if character == "\x1b":
            remainder = raw[index:]
            match = CSI.match(remainder)
            if match:
                screen.csi(match.group(1), match.group(2))
                index += match.end()
                continue
            osc = OSC.match(remainder)
            if osc:
                index += osc.end()
                continue
            index += 2
            continue
        screen.put(character)
        index += 1
        if character == tail:
            rendered = screen.text()
            if matcher.search(rendered):
                return rendered
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cols", type=int, default=160)
    parser.add_argument("--rows", type=int, default=40)
    parser.add_argument("--path", help="read from this file instead of stdin")
    parser.add_argument(
        "--when",
        help="print the screen as it was when this pattern was visible (text or regex)",
    )
    arguments = parser.parse_args()

    if arguments.path:
        with open(arguments.path, "rb") as handle:
            raw = handle.read()
    else:
        raw = sys.stdin.buffer.read()

    text = raw.decode("utf-8", errors="replace")
    if arguments.when:
        found = replay_until(text, arguments.cols, arguments.rows, arguments.when)
        if found is None:
            # Nothing printed: the pattern was never on screen. A checker compares the
            # output against what it expected, so silence is the negative answer.
            return 1
        print(found)
        return 0

    print(replay(text, arguments.cols, arguments.rows))
    return 0


if __name__ == "__main__":
    sys.exit(main())
