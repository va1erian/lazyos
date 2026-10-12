#!/usr/bin/env python3
"""List the sound and music lumps of SRB2Kart's data by format (docs/game-sound-plan.md S0).

Reads the WAD files ``tools/srb2kart/build.py`` extracts (default
``target/srb2kart/data``) and prints, per file, how many sound-effect
(``DS*``) and music (``O_*``, ``D_*``) lumps each format holds, their bytes,
their rates and channels, the loop tags of Ogg music, and what all of it
costs once decoded to 16-bit samples. Nothing is written: the data is Kart
Krew's and never leaves ``target/``.

    python tools/srb2kart/sound_census.py
    python tools/srb2kart/sound_census.py --data DIR --list    # one line per lump
"""

from __future__ import annotations

import argparse
import struct
import sys
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
DEFAULT_DATA = ROOT / "target" / "srb2kart" / "data"


@dataclass
class Lump:
    wad: str
    name: str
    kind: str  # "sfx" or "music"
    fmt: str
    size: int
    rate: int = 0
    channels: int = 0
    samples: int = 0  # per channel; 0 when unknown
    tags: dict = field(default_factory=dict)

    def decoded_bytes(self) -> int:
        return self.samples * max(self.channels, 1) * 2


def wad_lumps(path: Path):
    """`(name, bytes)` for every lump of a WAD (IWAD or PWAD)."""
    data = path.read_bytes()
    magic, count, offset = struct.unpack_from("<4sii", data, 0)
    if magic not in (b"IWAD", b"PWAD"):
        raise ValueError(f"{path.name}: not a WAD ({magic!r})")
    for index in range(count):
        pos, size, raw = struct.unpack_from("<ii8s", data, offset + 16 * index)
        name = raw.split(b"\0", 1)[0].decode("latin-1").upper()
        if pos < 0 or size < 0 or pos + size > len(data):
            raise ValueError(f"{path.name}: lump {name} out of bounds")
        yield name, data[pos:pos + size]


def kind_of(name: str) -> str | None:
    if name.startswith("DS"):
        return "sfx"
    if name.startswith("O_") or name.startswith("D_"):
        return "music"
    return None


def classify(wad: str, name: str, kind: str, body: bytes) -> Lump:
    lump = Lump(wad, name, kind, "other", len(body))
    if body[:4] == b"OggS":
        lump.fmt = "ogg"
        ogg_info(body, lump)
    elif len(body) >= 8 and struct.unpack_from("<H", body, 0)[0] == 3:
        lump.fmt = "dmx"
        lump.rate, count = struct.unpack_from("<HI", body, 2)
        lump.channels = 1
        # DMX pads 16 bytes before and after the samples it counts.
        lump.samples = max(min(count, len(body) - 8) - 32, 0)
    elif body[:4] == b"RIFF" and body[8:12] == b"WAVE":
        lump.fmt = "wav"
        wav_info(body, lump)
    elif body[:4] == b"MThd":
        lump.fmt = "midi"
    elif body[:4] == b"MUS\x1a":
        lump.fmt = "mus"
    elif body[:3] == b"ID3" or body[:2] in (b"\xff\xfb", b"\xff\xf3"):
        lump.fmt = "mp3"
    elif body[:4] == b"fLaC":
        lump.fmt = "flac"
    elif body[:2] == b"\x1f\x8b":
        lump.fmt = "gzip(vgz)"
    elif body[:17] == b"Extended Module: ":
        lump.fmt = "xm"
    elif body[:4] == b"IMPM":
        lump.fmt = "it"
    elif body[1080:1084] == b"M.K.":
        lump.fmt = "mod"
    elif body[:4] == b"Vgm ":
        lump.fmt = "vgm"
    return lump


def wav_info(body: bytes, lump: Lump) -> None:
    pos = 12
    while pos + 8 <= len(body):
        chunk, size = struct.unpack_from("<4sI", body, pos)
        if chunk == b"fmt ":
            _, lump.channels, lump.rate = struct.unpack_from("<HHI", body, pos + 8)
            bits = struct.unpack_from("<H", body, pos + 22)[0]
        elif chunk == b"data" and lump.channels:
            lump.samples = size // (lump.channels * max(bits // 8, 1))
        pos += 8 + size + (size & 1)


def ogg_pages(body: bytes):
    """`(granule, packet bytes)` of each Ogg page, in order."""
    pos = 0
    while pos + 27 <= len(body) and body[pos:pos + 4] == b"OggS":
        granule = struct.unpack_from("<q", body, pos + 6)[0]
        segments = body[pos + 26]
        table = body[pos + 27:pos + 27 + segments]
        start = pos + 27 + segments
        end = start + sum(table)
        yield granule, body[start:end]
        pos = end


def ogg_info(body: bytes, lump: Lump) -> None:
    packets = b""
    last = 0
    for granule, payload in ogg_pages(body):
        if len(packets) < 65536:
            packets += payload
        if granule >= 0:
            last = granule
    ident = packets.find(b"\x01vorbis")
    if ident >= 0:
        lump.channels = packets[ident + 11]
        lump.rate = struct.unpack_from("<I", packets, ident + 12)[0]
    lump.samples = last
    comment = packets.find(b"\x03vorbis")
    if comment < 0:
        return
    pos = comment + 7
    try:
        vendor = struct.unpack_from("<I", packets, pos)[0]
        pos += 4 + vendor
        count = struct.unpack_from("<I", packets, pos)[0]
        pos += 4
        for _ in range(min(count, 64)):
            size = struct.unpack_from("<I", packets, pos)[0]
            text = packets[pos + 4:pos + 4 + size].decode("utf-8", "replace")
            pos += 4 + size
            key, _, value = text.partition("=")
            if key.upper().startswith("LOOP"):
                lump.tags[key.upper()] = value
    except struct.error:
        pass


#: The engine's load order (`d_main.c`: the base, the startup files, then
#: `MUSICTEST`'s two); a later file's lump hides an earlier one of the same
#: name. Files not listed (`bonuschars.kart`, an addon) come last.
LOAD_ORDER = ["srb2.srb", "gfx.kart", "textures.kart", "chars.kart", "maps.kart",
              "patch.kart", "sounds.kart", "music.kart"]


def load_rank(path: Path) -> tuple[int, str]:
    name = path.name.lower()
    rank = LOAD_ORDER.index(name) if name in LOAD_ORDER else len(LOAD_ORDER)
    return rank, name


def census(data: Path) -> tuple[list[Lump], int]:
    """The audio lumps the engine would find, by name (the last file wins),
    and how many `DS*`/`O_*`/`D_*` lumps are not audio at all (textures and
    flats whose names happen to start with `DS`)."""
    found: dict[str, Lump] = {}
    not_audio = 0
    wads = [p for p in data.iterdir() if p.suffix.lower() in (".kart", ".srb", ".wad")]
    for wad in sorted(wads, key=load_rank):
        for name, body in wad_lumps(wad):
            kind = kind_of(name)
            if not kind:
                continue
            lump = classify(wad.name, name, kind, body)
            if lump.fmt == "other":
                not_audio += 1
                continue
            found[name] = lump
    return list(found.values()), not_audio


def mib(count: int) -> str:
    return f"{count / (1 << 20):.1f} MiB"


def report(lumps: list[Lump]) -> None:
    groups: dict[tuple, list[Lump]] = defaultdict(list)
    for lump in lumps:
        groups[(lump.kind, lump.fmt)].append(lump)
    print("| kind | format | lumps | bytes | rates (Hz: count) | channels | decoded i16 |")
    print("|---|---|---|---|---|---|---|")
    for (kind, fmt), items in sorted(groups.items()):
        rates: dict[int, int] = defaultdict(int)
        chans: dict[int, int] = defaultdict(int)
        for lump in items:
            rates[lump.rate] += 1
            chans[lump.channels] += 1
        rate_text = ", ".join(f"{r}: {n}" for r, n in sorted(rates.items()))
        chan_text = ", ".join(f"{c}: {n}" for c, n in sorted(chans.items()))
        size = sum(lump.size for lump in items)
        decoded = sum(lump.decoded_bytes() for lump in items)
        print(f"| {kind} | {fmt} | {len(items)} | {mib(size)} | {rate_text} | {chan_text} | {mib(decoded)} |")
    music = [lump for lump in lumps if lump.kind == "music" and lump.fmt == "ogg"]
    tagged = defaultdict(int)
    for lump in music:
        tagged[",".join(sorted(lump.tags)) or "none"] += 1
    print()
    print("Ogg music loop tags:", dict(sorted(tagged.items())))
    sfx = [lump for lump in lumps if lump.kind == "sfx"]
    if sfx:
        longest = max(sfx, key=lambda lump: lump.decoded_bytes())
        seconds = longest.samples / longest.rate if longest.rate else 0
        print(f"Largest effect: {longest.name} ({longest.wad}, {longest.fmt}, "
              f"{seconds:.1f} s, {mib(longest.decoded_bytes())} decoded)")
    if music:
        longest = max(music, key=lambda lump: lump.samples / max(lump.rate, 1))
        print(f"Longest track: {longest.name} ({longest.samples / max(longest.rate, 1):.0f} s, "
              f"{mib(longest.decoded_bytes())} decoded)")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--data", type=Path, default=DEFAULT_DATA)
    parser.add_argument("--list", action="store_true", help="one line per lump")
    args = parser.parse_args(argv)
    if not args.data.is_dir():
        print(f"no data at {args.data}; run tools/srb2kart/build.py first", file=sys.stderr)
        return 1
    lumps, not_audio = census(args.data)
    if args.list:
        for lump in lumps:
            print(f"{lump.wad}\t{lump.name}\t{lump.kind}\t{lump.fmt}\t{lump.size}\t"
                  f"{lump.rate}\t{lump.channels}\t{lump.samples}\t{lump.tags}")
        return 0
    report(lumps)
    print(f"Skipped {not_audio} lumps named like sounds that are not audio (textures, flats, markers).")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
