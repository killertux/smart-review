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

CSI = re.compile(r"\x1b\[([0-9;?]*)([a-zA-Z])")
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


def replay(data: str, cols: int, rows: int) -> str:
    screen = Screen(cols, rows)
    index = 0
    while index < len(data):
        character = data[index]
        if character == "\x1b":
            remainder = data[index:]
            match = CSI.match(remainder)
            if match:
                screen.csi(match.group(1), match.group(2))
                index += match.end()
                continue
            osc = OSC.match(remainder)
            if osc:
                # An operating-system command such as a window title or the OSC 52
                # clipboard write: nothing is drawn.
                index += osc.end()
                continue
            # A two-character escape (for example the alternate screen switch), which
            # this replay can ignore: the grid keeps being drawn into.
            index += 2
            continue
        screen.put(character)
        index += 1
    return screen.text()


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
