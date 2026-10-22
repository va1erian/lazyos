"""Data shared by the site generator's modules."""

from __future__ import annotations

import tomllib
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent


@dataclass
class Page:
    """One Markdown source and the page built from it."""

    src: str  # repository-relative, posix: "docs/architecture/boot.md"
    out: str  # site-relative, posix: "architecture/boot.html"
    section: str = ""
    title: str = ""
    body: str = ""
    summary: str = ""
    text: str = ""
    toc: list[tuple[int, str, str]] = field(default_factory=list)  # level, id, text
    ids: set[str] = field(default_factory=set)


@dataclass
class Config:
    title: str
    tagline: str
    repo: str
    ref: str
    section_order: list[str]
    overview: list[str]
    directories: dict[str, str]
    plan_suffixes: list[str]
    extras: dict[str, str]
    extra_sections: dict[str, str]
    generated: set[str]

    @property
    def repo_url(self) -> str:
        return f"https://github.com/{self.repo}"


def load_config(path: Path = HERE / "site.toml") -> Config:
    raw = tomllib.loads(path.read_text(encoding="utf-8"))
    site, nav = raw["site"], raw["nav"]
    return Config(
        title=site["title"],
        tagline=site["tagline"],
        repo=site["repo"],
        ref=site["ref"],
        section_order=list(nav["order"]),
        overview=list(nav["overview"]),
        directories=dict(nav["directories"]),
        plan_suffixes=list(nav["plans"]["suffixes"]),
        extras={k: v for k, v in raw["extras"].items() if k != "sections"},
        extra_sections=dict(raw["extras"].get("sections", {})),
        generated=set(raw["generated"]["paths"]),
    )
