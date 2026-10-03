#!/usr/bin/env python3
"""Generate the MOD player sample's built-in song.

`lazyrad-os/samples/modplayer` plays ProTracker modules. A packaged LazyRAD app
ships only its forms and scripts, so the sample carries its demo song inside a
script module, `demo_song.rhai`, as base64 text that `modplay::decode` turns
back into a module. This script composes that song from scratch: every sample
is synthesized here (pulse, saw and triangle cycles, a swept-sine kick, noise
snare and hat) and the patterns are written out below, so the song is an
original work with no third-party audio. It is dedicated to the public domain
(CC0), like the rest of this generator's output.

Usage::

    python tools/lazyrad/gen_demo_song.py            # rewrite demo_song.rhai
    python tools/lazyrad/gen_demo_song.py --check    # fail if it is stale
    python tools/lazyrad/gen_demo_song.py --mod out.mod   # also write the .mod
"""

from __future__ import annotations

import argparse
import base64
import math
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
OUT = ROOT / "lazyrad-os" / "samples" / "modplayer" / "demo_song.rhai"

TITLE = "LazyOS Groove"
ROWS = 64
CHANNELS = 4

# ProTracker periods (finetune 0), octaves 1-3.
NOTE_NAMES = ["C-", "C#", "D-", "D#", "E-", "F-", "F#", "G-", "G#", "A-", "A#", "B-"]
PERIODS = [
    856, 808, 762, 720, 678, 640, 604, 570, 538, 508, 480, 453,
    428, 404, 381, 360, 339, 320, 302, 285, 269, 254, 240, 226,
    214, 202, 190, 180, 170, 160, 151, 143, 135, 127, 120, 113,
]


def period(name: str) -> int:
    """`"A-2"` -> its period."""
    note, octave = name[:2], int(name[2])
    return PERIODS[(octave - 1) * 12 + NOTE_NAMES.index(note)]


# --- samples -----------------------------------------------------------------
# One cycle of 32 bytes at C-2 (period 428, 8287 bytes/s) sounds at 259 Hz,
# middle C, so the waveforms below are in tune with the period table.

def clamp(v: float) -> int:
    return max(-128, min(127, int(round(v))))


def pulse(duty: float) -> list[int]:
    return [100 if i < 32 * duty else -100 for i in range(32)]


def saw() -> list[int]:
    return [clamp(-110 + 220 * i / 31) for i in range(32)]


def triangle() -> list[int]:
    return [clamp(110 - 440 * abs(i / 32 - 0.5)) for i in range(32)]


def kick() -> list[int]:
    """A sine swept from 160 Hz to 45 Hz with an exponential decay, at C-2."""
    rate = 8287
    out, phase = [], 0.0
    for i in range(1600):
        t = i / rate
        freq = 45 + 115 * math.exp(-t * 18)
        phase += 2 * math.pi * freq / rate
        out.append(clamp(120 * math.exp(-t * 7) * math.sin(phase)))
    return out


def noise(length: int, decay: float, seed: int) -> list[int]:
    """Deterministic white noise (an LCG) with an exponential decay."""
    out, state = [], seed
    for i in range(length):
        state = (state * 1103515245 + 12345) & 0x7FFFFFFF
        value = ((state >> 16) & 0xFF) - 128
        out.append(clamp(value * math.exp(-i * decay)))
    return out


# (name, data, volume, loop start, loop length) in bytes; no loop = (0, 0).
SAMPLES = [
    ("lead (pulse 25%)", pulse(0.25), 40, 0, 32),
    ("bass (saw)", saw(), 52, 0, 32),
    ("kick (swept sine)", kick(), 64, 0, 0),
    ("snare (noise)", noise(1400, 0.0035, 7), 44, 0, 0),
    ("hat (noise)", noise(360, 0.012, 99), 26, 0, 0),
    ("pad (triangle)", triangle(), 30, 0, 32),
    ("", [], 0, 0, 0),
    ("made with LazyRAD", [], 0, 0, 0),
    ("on LazyOS, in Rhai", [], 0, 0, 0),
    ("", [], 0, 0, 0),
    ("public domain (CC0)", [], 0, 0, 0),
]
LEAD, BASS, KICK, SNARE, HAT, PAD = 1, 2, 3, 4, 5, 6

# --- patterns ----------------------------------------------------------------

Cell = tuple[int, int, int, int]  # (period, sample, effect, param)
EMPTY: Cell = (0, 0, 0, 0)

# A minor: Am F C G, one chord per 16 rows.
CHORDS = [("A-1", 0x37), ("F-1", 0x47), ("C-2", 0x47), ("G-1", 0x47)]


def blank() -> list[list[Cell]]:
    return [[EMPTY] * CHANNELS for _ in range(ROWS)]


def put(pattern, row, channel, note: str | None, sample=0, effect=0, param=0):
    pattern[row][channel] = (period(note) if note else 0, sample, effect, param)


def drums(pattern, full: bool = True) -> None:
    for bar in range(4):
        base = bar * 16
        put(pattern, base, 2, "C-2", KICK)
        if full:
            put(pattern, base + 8, 2, "C-2", KICK)
            put(pattern, base + 4, 2, "C-2", SNARE)
            put(pattern, base + 12, 2, "C-2", SNARE)
            for row in (2, 6, 10, 14):
                put(pattern, base + row, 2, "C-3", HAT)
            put(pattern, base + 15, 2, "C-3", HAT, 0xC, 0x10)


def bass(pattern) -> None:
    for bar, (root, _) in enumerate(CHORDS):
        up = root[:2] + str(int(root[2]) + 1)
        for step in range(8):
            put(pattern, bar * 16 + step * 2, 1, up if step % 2 else root, BASS)


def pad(pattern) -> None:
    """Arpeggiated chords (effect 0xy) on channel 3, kept going row by row."""
    for bar, (root, arp) in enumerate(CHORDS):
        note = root[:2] + str(int(root[2]) + 1)
        put(pattern, bar * 16, 3, note, PAD, 0x0, arp)
        for row in range(1, 16):
            put(pattern, bar * 16 + row, 3, None, 0, 0x0, arp)


MELODY_A = [
    [(0, "A-2"), (4, "C-3"), (6, "B-2"), (8, "A-2"), (12, "E-2"), (14, "G-2")],
    [(0, "F-2"), (4, "A-2"), (6, "C-3"), (8, "A-2"), (12, "G-2"), (14, "F-2")],
    [(0, "E-2"), (4, "G-2"), (6, "C-3"), (8, "E-3"), (12, "D-3"), (14, "C-3")],
    [(0, "B-2"), (4, "D-3"), (8, "G-2"), (12, "B-2")],
]
MELODY_B = [
    [(0, "E-3"), (2, "D-3"), (4, "C-3"), (8, "A-2"), (10, "C-3"), (12, "E-3")],
    [(0, "F-3"), (4, "E-3"), (6, "D-3"), (8, "C-3"), (12, "A-2")],
    [(0, "G-2"), (2, "C-3"), (4, "E-3"), (8, "G-3"), (12, "E-3")],
    [(0, "D-3"), (4, "B-2"), (8, "G-2")],
]


def melody(pattern, bars, volume: int | None = None) -> None:
    for bar, notes in enumerate(bars):
        for row, note in notes:
            if volume is None:
                put(pattern, bar * 16 + row, 0, note, LEAD)
            else:
                put(pattern, bar * 16 + row, 0, note, LEAD, 0xC, volume)


def flourish(pattern) -> None:
    """Effects worth seeing in the pattern view: vibrato, a volume fade and a
    tone portamento."""
    for row in range(57, 60):
        put(pattern, row, 0, None, 0, 0x4, 0x46)  # vibrato on the held G-2
    put(pattern, 60, 0, "D-3", 0, 0x3, 0x0C)  # glide up to D-3
    for row in range(61, 64):
        put(pattern, row, 0, None, 0, 0xA, 0x02)  # fade out


def patterns() -> list[list[list[Cell]]]:
    intro = blank()
    drums(intro)
    bass(intro)
    put(intro, 0, 0, None, 0, 0xF, 6)  # speed 6
    groove = blank()
    drums(groove)
    bass(groove)
    pad(groove)
    verse = blank()
    drums(verse)
    bass(verse)
    pad(verse)
    melody(verse, MELODY_A)
    chorus = blank()
    drums(chorus)
    bass(chorus)
    pad(chorus)
    melody(chorus, MELODY_B)
    flourish(chorus)
    breakdown = blank()
    drums(breakdown, full=False)
    pad(breakdown)
    melody(breakdown, MELODY_A, volume=0x18)
    return [intro, groove, verse, chorus, breakdown]


ORDERS = [0, 1, 2, 3, 4, 2, 3]

# --- the file ----------------------------------------------------------------


def build() -> bytes:
    out = bytearray(TITLE.encode().ljust(20, b"\0"))
    slots = SAMPLES + [("", [], 0, 0, 0)] * (31 - len(SAMPLES))
    for name, data, volume, loop_start, loop_len in slots:
        out += name.encode().ljust(22, b"\0")[:22]
        loop_words = loop_len // 2 if loop_len else 1
        out += struct.pack(">HBBHH", len(data) // 2, 0, volume, loop_start // 2, loop_words)
    out += bytes([len(ORDERS), 0])
    out += bytes(ORDERS).ljust(128, b"\0")
    out += b"M.K."
    for pattern in patterns():
        for row in pattern:
            for per, sample, effect, param in row:
                out += bytes([
                    (sample & 0xF0) | (per >> 8),
                    per & 0xFF,
                    ((sample & 0x0F) << 4) | effect,
                    param,
                ])
    for _, data, *_ in slots:
        if len(data) % 2:
            data = data + [0]
        out += bytes(v & 0xFF for v in data)
    return bytes(out)


def module(song: bytes) -> str:
    """The Rhai module carrying `song`."""
    encoded = base64.b64encode(song).decode()
    lines = [encoded[i:i + 76] for i in range(0, len(encoded), 76)]
    body = "\n".join(lines)
    return (
        "// The MOD player's built-in song, \"" + TITLE + "\": an original\n"
        "// four-channel ProTracker module, dedicated to the public domain (CC0).\n"
        "//\n"
        "// GENERATED by tools/lazyrad/gen_demo_song.py; do not edit. A packaged\n"
        "// LazyRAD app ships only forms and scripts, so the song travels as base64\n"
        "// text; `modplay::decode(demo_song::data())` turns it back into a Song.\n"
        "\n"
        "fn title() {\n"
        f"    \"{TITLE}\"\n"
        "}\n"
        "\n"
        "fn data() {\n"
        "    `\n" + body + "\n`\n"
        "}\n"
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="fail if the module is stale")
    parser.add_argument("--mod", type=Path, help="also write the .mod file here")
    args = parser.parse_args(argv)
    song = build()
    text = module(song)
    if args.mod:
        args.mod.write_bytes(song)
    if args.check:
        current = OUT.read_text(encoding="utf-8") if OUT.is_file() else ""
        if current != text:
            print(f"{OUT} is stale; run python tools/lazyrad/gen_demo_song.py", file=sys.stderr)
            return 1
        print(f"{OUT.name} is current ({len(song)} bytes of module)")
        return 0
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {OUT} ({len(song)} bytes of module, {len(text)} bytes of script)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
