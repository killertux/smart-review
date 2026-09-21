#!/usr/bin/env python3
"""Validate local Markdown links without network access."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote


ROOT = Path(__file__).resolve().parents[2]
LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")
HEADING = re.compile(r"^#{1,6}\s+(.+?)\s*#*\s*$")


def github_slug(text: str) -> str:
    """Return the GitHub-style fragment used by headings in this repository."""
    text = re.sub(r"<[^>]+>", "", text)
    text = re.sub(r"[`*_~]", "", text).strip().lower()
    text = re.sub(r"[^\w\- ]", "", text, flags=re.UNICODE)
    return re.sub(r" +", "-", text)


def fragments(path: Path) -> set[str]:
    """Collect heading fragments, including GitHub's duplicate suffixes."""
    found: set[str] = set()
    counts: dict[str, int] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        match = HEADING.match(line)
        if match is None:
            continue
        base = github_slug(match.group(1))
        count = counts.get(base, 0)
        found.add(base if count == 0 else f"{base}-{count}")
        counts[base] = count + 1
    return found


def markdown_files() -> list[Path]:
    """Return active root documents plus every document below docs/."""
    roots = [ROOT / name for name in ("README.md", "AGENTS.md", "ARCHITECTURE.md")]
    return roots + sorted((ROOT / "docs").rglob("*.md"))


def validate() -> list[str]:
    """Return every broken local link or fragment."""
    errors: list[str] = []
    fragment_cache: dict[Path, set[str]] = {}
    for source in markdown_files():
        text = source.read_text(encoding="utf-8")
        for raw_target in LINK.findall(text):
            target = raw_target.split(maxsplit=1)[0].strip("<>")
            if target.startswith(("http://", "https://", "mailto:")):
                continue
            path_text, separator, fragment = target.partition("#")
            destination = source if not path_text else source.parent / unquote(path_text)
            destination = destination.resolve()
            try:
                destination.relative_to(ROOT)
            except ValueError:
                errors.append(f"{source.relative_to(ROOT)}: link escapes the repository: {target}")
                continue
            if not destination.exists():
                errors.append(f"{source.relative_to(ROOT)}: missing target: {target}")
                continue
            if separator and fragment and destination.suffix.lower() == ".md":
                available = fragment_cache.setdefault(destination, fragments(destination))
                if unquote(fragment).lower() not in available:
                    errors.append(f"{source.relative_to(ROOT)}: missing fragment: {target}")
    return errors


def main() -> int:
    """Print actionable failures and return a shell-friendly status."""
    errors = validate()
    if errors:
        print("documentation validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print("documentation links: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
