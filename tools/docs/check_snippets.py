#!/usr/bin/env python3
"""Check that the Rhai code blocks of the documentation are valid Rhai.

Every fenced block tagged `rhai` in `docs/**/*.md` is compiled (never run)
with the same Rhai the `rhai` command and LazyRAD use, through
`libs/rhai-lazy/examples/check_syntax.rs`. A block that is deliberately not a
whole script (an ellipsis, a fragment) is tagged `rhai,ignore`... or, better,
completed. Also checks that every relative `.md` link in the docs points at a
file that exists, since the Docs app follows them on the guest.

    python tools/docs/check_snippets.py            # snippets and links
    python tools/docs/check_snippets.py docs/lazyrad/cookbook.md
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FENCE = re.compile(r"^```(?P<info>[^\n]*)\n(?P<body>.*?)^```[ \t]*$", re.S | re.M)
# `[text](target)`; image links and absolute URLs are not followed by Docs.
LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)\s]+)\)")


def show(path: Path) -> str:
    try:
        return str(path.relative_to(ROOT))
    except ValueError:
        return str(path)


def docs_files(args: list[str]) -> list[Path]:
    if args:
        return [Path(a).resolve() for a in args]
    return sorted((ROOT / "docs").rglob("*.md"))


def blocks(text: str):
    """(1-based line, info string, body) of every fenced block."""
    for match in FENCE.finditer(text):
        yield text.count("\n", 0, match.start()) + 1, match["info"].strip(), match["body"]


def extract(paths: list[Path], out: Path) -> dict[Path, tuple[Path, int]]:
    """Writes each `rhai` block to `out`; returns file -> (doc, line)."""
    origin: dict[Path, tuple[Path, int]] = {}
    for doc in paths:
        text = doc.read_text(encoding="utf-8")
        for index, (line, info, body) in enumerate(blocks(text)):
            tags = [t for t in re.split(r"[\s,]+", info) if t]
            if "rhai" not in tags or "ignore" in tags:
                continue
            name = out / f"{doc.stem}_{line}_{index}.rhai"
            name.write_text(body, encoding="utf-8")
            origin[name] = (doc, line)
    return origin


def embedded_root_doc(doc: Path, file: str) -> bool:
    """`docs/README.md` and `docs/THIRD_PARTY.md` are the repository's root files,
    which the image build embeds beside the docs tree (`docs_embed.rs`)."""
    target = (doc.parent / file).resolve()
    return target.parent == (ROOT / "docs") and (ROOT / target.name).is_file()


def slug(title: str) -> str:
    """A heading's anchor as the Docs app spells it (`xui-app/docs/src/lib.rs`)."""
    title = re.sub(r"[`*_]", lambda m: "_" if m.group() == "_" else "", title.strip())
    return "".join(
        "-" if c == " " else c.lower() if c.isascii() and c.isalnum() else c if c in "-_" else ""
        for c in title
    )


def anchors(path: Path) -> set[str]:
    """The ids the Docs app gives the headings of `path` (repeats are numbered)."""
    seen: dict[str, int] = {}
    result = set()
    text = FENCE.sub("", path.read_text(encoding="utf-8"))
    for match in re.finditer(r"^#{1,6}[ 	]+(.+?)[ 	#]*$", text, re.M):
        base = slug(re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", match[1]))
        count = seen.get(base, 0)
        result.add(base if count == 0 else f"{base}-{count}")
        seen[base] = count + 1
    return result


def check_links(paths: list[Path]) -> list[str]:
    problems = []
    for doc in paths:
        text = FENCE.sub("", doc.read_text(encoding="utf-8"))
        for target in LINK.findall(text):
            if re.match(r"^[a-z][a-z0-9+.-]*:", target):
                continue
            file, _, fragment = target.partition("#")
            linked = (doc.parent / file).resolve() if file else doc.resolve()
            if not linked.exists():
                if not embedded_root_doc(doc, file):
                    problems.append(f"{show(doc)}: broken link {target}")
            elif fragment and linked.suffix == ".md" and fragment not in anchors(linked):
                problems.append(f"{show(doc)}: no heading #{fragment} in {show(linked)}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("files", nargs="*")
    parser.add_argument("--no-links", action="store_true")
    args = parser.parse_args()
    paths = docs_files(args.files)

    problems = [] if args.no_links else check_links(paths)
    with tempfile.TemporaryDirectory() as tmp:
        origin = extract(paths, Path(tmp))
        if origin:
            listing = Path(tmp) / "files.list"
            listing.write_text(chr(10).join(map(str, origin)), encoding="utf-8")
            result = subprocess.run(
                ["cargo", "run", "-q", "--manifest-path", str(ROOT / "libs/rhai-lazy/Cargo.toml"),
                 "--example", "check_syntax", "--", f"@{listing}"],
                capture_output=True, text=True,
            )
            if result.returncode not in (0, 1):
                print(result.stderr, file=sys.stderr)
                return 2
            for line in result.stderr.splitlines():
                path, _, message = line.partition(": ")
                doc, start = origin.get(Path(path), (None, 0))
                problems.append(f"{show(doc)}:{start}: {message}" if doc else line)
    for problem in problems:
        print(problem)
    print(f"docs: {len(origin)} rhai block(s), {len(paths)} file(s), {len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
