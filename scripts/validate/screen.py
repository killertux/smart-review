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
import unicodedata


class Screen:
    def __init__(self, cols: int, rows: int) -> None:
        self.cols = cols
        self.rows = rows
        self.grid = [[" "] * cols for _ in range(rows)]
        self.changed = [[0] * cols for _ in range(rows)]
        self.row = 0
        self.col = 0
        self.saved_row = 0
        self.saved_col = 0
        self.revision = 0

    def clear(self) -> None:
        self.grid = [[" "] * self.cols for _ in range(self.rows)]
        self.revision += 1
        self.changed = [[self.revision] * self.cols for _ in range(self.rows)]
        self.row = 0
        self.col = 0

    def resize(self, cols: int, rows: int) -> None:
        """Resizes the terminal while preserving the visible intersection."""
        grid = [[" "] * cols for _ in range(rows)]
        changed = [[0] * cols for _ in range(rows)]
        for row in range(min(self.rows, rows)):
            for col in range(min(self.cols, cols)):
                grid[row][col] = self.grid[row][col]
                changed[row][col] = self.changed[row][col]
        self.cols = cols
        self.rows = rows
        self.grid = grid
        self.changed = changed
        self.row = min(self.row, rows - 1)
        self.col = min(self.col, cols - 1)

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
        if unicodedata.combining(character):
            if self.col > 0:
                self.grid[self.row][self.col - 1] += character
                self.revision += 1
                self.changed[self.row][self.col - 1] = self.revision
            return
        if self.col >= self.cols:
            # Autowrap: the next line, as a terminal with DECAWM on would do.
            self.col = 0
            self.row = min(self.row + 1, self.rows - 1)
        self.grid[self.row][self.col] = character
        self.revision += 1
        self.changed[self.row][self.col] = self.revision
        width = 2 if unicodedata.east_asian_width(character) in ("W", "F") else 1
        if width == 2 and self.col + 1 < self.cols:
            self.grid[self.row][self.col + 1] = ""
            self.changed[self.row][self.col + 1] = self.revision
        self.col += width

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
                self.revision += 1
                for column in range(self.col, self.cols):
                    self.grid[self.row][column] = " "
                    self.changed[self.row][column] = self.revision
                for row in range(self.row + 1, self.rows):
                    self.grid[row] = [" "] * self.cols
                    self.changed[row] = [self.revision] * self.cols
        elif final == "K":
            if first == 0:
                for column in range(self.col, self.cols):
                    self.grid[self.row][column] = " "
            elif first == 1:
                for column in range(0, self.col + 1):
                    self.grid[self.row][column] = " "
            else:
                self.grid[self.row] = [" "] * self.cols
            self.revision += 1
            if first == 0:
                columns = range(self.col, self.cols)
            elif first == 1:
                columns = range(0, self.col + 1)
            else:
                columns = range(self.cols)
            for column in columns:
                self.changed[self.row][column] = self.revision
        elif final == "s":
            self.saved_row = self.row
            self.saved_col = self.col
        elif final == "u":
            self.row = self.saved_row
            self.col = self.saved_col

    def text(self) -> str:
        return "\n".join("".join(row).rstrip() for row in self.grid)

    def pattern_visible_since(self, matcher: re.Pattern[str], revision: int) -> bool:
        """Whether a visible match contains a cell redrawn after `revision`."""
        characters: list[str] = []
        revisions: list[int] = []
        for row in range(self.rows):
            end = self.cols
            while end and self.grid[row][end - 1] == " ":
                end -= 1
            for col in range(end):
                cell = self.grid[row][col]
                characters.extend(cell)
                revisions.extend([self.changed[row][col]] * len(cell))
            if row + 1 < self.rows:
                characters.append("\n")
                revisions.append(0)
        text = "".join(characters)
        return any(
            max(revisions[match.start() : match.end()], default=0) > revision
            for match in matcher.finditer(text)
        )


class Replay:
    """Incrementally applies terminal output to one screen.

    PTY reads may split UTF-8 and escape sequences anywhere. The replay keeps a small
    parser state rather than copying and reparsing the captured suffix or whole stream.
    """

    def __init__(self, cols: int, rows: int) -> None:
        self.screen = Screen(cols, rows)
        self.state = "text"
        self.csi_body = ""

    def feed(self, data: str, *, final: bool = False) -> None:
        for character in data:
            if self.state == "text":
                if character == "\x1b":
                    self.state = "escape"
                else:
                    self.screen.put(character)
            elif self.state == "escape":
                if character == "[":
                    self.state = "csi"
                    self.csi_body = ""
                elif character == "]":
                    self.state = "osc"
                elif character == "P":
                    self.state = "dcs"
                elif character == "7":
                    self.screen.saved_row = self.screen.row
                    self.screen.saved_col = self.screen.col
                    self.state = "text"
                elif character == "8":
                    self.screen.row = self.screen.saved_row
                    self.screen.col = self.screen.saved_col
                    self.state = "text"
                elif character == "c":
                    self.screen.clear()
                    self.state = "text"
                elif character in "()":
                    self.state = "charset"
                else:
                    self.state = "text"
            elif self.state == "csi":
                if "@" <= character <= "~":
                    params = self.csi_body.split(" ", maxsplit=1)[0]
                    self.screen.csi(params, character)
                    self.csi_body = ""
                    self.state = "text"
                else:
                    self.csi_body += character
            elif self.state == "osc":
                if character == "\x07":
                    self.state = "text"
                elif character == "\x1b":
                    self.state = "osc_escape"
            elif self.state == "osc_escape":
                self.state = "text" if character == "\\" else "osc"
            elif self.state == "dcs":
                if character == "\x1b":
                    self.state = "dcs_escape"
            elif self.state == "dcs_escape":
                self.state = "text" if character == "\\" else "dcs"
            elif self.state == "charset":
                self.state = "text"
        if final:
            self.state = "text"
            self.csi_body = ""

    def finish(self) -> None:
        self.feed("", final=True)

    def resize(self, cols: int, rows: int) -> None:
        self.screen.resize(cols, rows)

    def text(self) -> str:
        return self.screen.text()


def replay(data: str, cols: int, rows: int) -> str:
    replayed = Replay(cols, rows)
    replayed.feed(data, final=True)
    replayed.finish()
    return replayed.text()


def replay_until(raw: str, cols: int, rows: int, pattern: str) -> str | None:
    """The screen as it was the first time `pattern` was on it.

    A popup can be drawn and closed inside one keystroke's worth of frames, so the
    incremental parser is fed every character. The screen is rendered only when the
    last character of a possible match has just been written, which keeps replay
    linear in the size of the capture.
    """
    matcher = re.compile(pattern)
    replayed = Replay(cols, rows)
    tail = pattern[-1]
    for character in raw:
        replayed.feed(character)
        if character == tail:
            rendered = replayed.text()
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
