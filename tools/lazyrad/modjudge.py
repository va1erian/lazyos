"""Judge a recording of the LazyRAD MOD player against a host render.

The verdict on sound is the recording (`AGENTS.md`, "Sound harness"): QEMU
writes what the guest played to a WAV, and this module checks it is the song,
whole. The reference is the same module rendered on the host by `libs/modplay`
(`examples/render_wav.rs`), the code the player mixes with.

Both are compared as *mono* (left + right): the player's stereo separation
moves sound between the channels but not out of their sum, so the sum matches
whatever separation the session used. Volume only scales, which correlation
ignores. The recording is aligned with the reference once, at the start; from
then on every audible half second must correlate at that same offset (give or
take a few samples), so a skipped or repeated stretch anywhere (a pause that
lost or replayed audio) fails, as does a gap or a recording cut short.

QEMU's WAV header sizes stay zero when QEMU is stopped rather than quit; the
reader takes the data to the end of the file then.
"""

from __future__ import annotations

import math
import struct
from dataclasses import dataclass

#: Chunks quieter than this (RMS of the mono reference) are not judged.
QUIET = 300.0
#: The correlation every judged chunk must reach.
MIN_CORRELATION = 0.9
#: How far (in reference samples) a chunk may drift from the start alignment.
DRIFT = 3
#: How far around the first audible samples the start alignment is searched
#: (in reference samples).
SEARCH = 200
#: Chunk length in seconds.
CHUNK_S = 0.5


@dataclass
class Audio:
    rate: int
    mono: list[float]

    @property
    def seconds(self) -> float:
        return len(self.mono) / self.rate


def read_wav(data: bytes) -> Audio:
    """16-bit PCM WAV as mono (sum of the channels). Tolerates zero sizes."""
    if len(data) < 12 or data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise ValueError("not a RIFF/WAVE file")
    at, fmt = 12, None
    while at + 8 <= len(data):
        tag, size = data[at:at + 4], struct.unpack_from("<I", data, at + 4)[0]
        body = at + 8
        if tag == b"fmt ":
            _, channels, rate, _, _, bits = struct.unpack_from("<HHIIHH", data, body)
            fmt = (channels, rate, bits)
        elif tag == b"data":
            if fmt is None:
                raise ValueError("data before fmt")
            channels, rate, bits = fmt
            if bits != 16:
                raise ValueError(f"{bits}-bit PCM is not supported")
            end = len(data) if size == 0 or body + size > len(data) else body + size
            frames = (end - body) // (2 * channels)
            samples = struct.unpack_from(f"<{frames * channels}h", data, body)
            mono = [float(sum(samples[i * channels:(i + 1) * channels])) for i in range(frames)]
            return Audio(rate, mono)
        at = body + size + (size & 1)
    raise ValueError("no data chunk")


def resample(audio: Audio, rate: int) -> list[float]:
    """`audio` at `rate` by nearest sample (enough to compare envelopes and
    waveforms of the same mix)."""
    if audio.rate == rate:
        return audio.mono
    n = int(len(audio.mono) * rate / audio.rate)
    return [audio.mono[min(len(audio.mono) - 1, round(i * audio.rate / rate))] for i in range(n)]


def correlation(a: list[float], b: list[float]) -> float:
    n = min(len(a), len(b))
    if n == 0:
        return 0.0
    num = sum(x * y for x, y in zip(a[:n], b[:n]))
    den = math.sqrt(sum(x * x for x in a[:n]) * sum(y * y for y in b[:n]))
    return num / den if den else 0.0


def rms(xs: list[float]) -> float:
    return math.sqrt(sum(x * x for x in xs) / len(xs)) if xs else 0.0


def first_audible(xs: list[float], level: float = 200.0) -> int:
    return next((i for i, x in enumerate(xs) if abs(x) > level), 0)


@dataclass
class Verdict:
    ok: bool
    lines: list[str]


def judge(recording: Audio, reference: Audio) -> Verdict:
    """Whether `recording` is all of `reference`, without skips or repeats."""
    rate = reference.rate
    ref = reference.mono
    rec = resample(recording, rate)
    lines = [f"recording {recording.seconds:.2f} s at {recording.rate} Hz, "
             f"reference {reference.seconds:.2f} s at {rate} Hz"]
    if rms(rec) < QUIET / 10:
        return Verdict(False, lines + ["FAIL: the recording is silent"])
    chunk = int(rate * CHUNK_S)
    # Align on the first audible second.
    start_ref, start_rec = first_audible(ref), first_audible(rec)
    window = ref[start_ref:start_ref + rate]
    offset = max(range(start_rec - start_ref - SEARCH, start_rec - start_ref + SEARCH + 1),
                 key=lambda o: correlation(rec[max(0, start_ref + o):], window)
                 if start_ref + o >= 0 else -1.0)
    lines.append(f"aligned at {offset * 1000 / rate:+.1f} ms")
    judged, worst, failed = 0, (1.0, 0.0), []
    for at in range(0, len(ref) - chunk + 1, chunk):
        piece = ref[at:at + chunk]
        if rms(piece) < QUIET:
            continue
        judged += 1
        best = max(correlation(rec[at + offset + d:at + offset + d + chunk], piece)
                   if at + offset + d >= 0 else 0.0
                   for d in range(-DRIFT, DRIFT + 1))
        worst = min(worst, (best, at / rate))
        if best < MIN_CORRELATION:
            failed.append(f"{at / rate:.1f} s (r={best:.3f})")
    lines.append(f"{judged} audible chunks judged; worst r={worst[0]:.3f} at {worst[1]:.1f} s")
    if judged == 0:
        return Verdict(False, lines + ["FAIL: the reference has nothing audible"])
    if failed:
        shown = ", ".join(failed[:6]) + (" ..." if len(failed) > 6 else "")
        return Verdict(False, lines + [f"FAIL: {len(failed)} chunk(s) differ from the song: {shown}"])
    return Verdict(True, lines + ["PASS: the whole song, in order, no skips or repeats"])
