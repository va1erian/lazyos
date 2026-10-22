#!/usr/bin/env python3
"""Boot LazyOS with QEMU's USB speaker, record it, judge the recording
(docs/usb-audio-plan.md, stage A3).

QEMU's `usb-audio` (UAC 1.0, full speed, 48 kHz stereo) sits on its own
`qemu-xhci` controller with `-audiodev wav`, so whatever `usbd` sends over the
isochronous endpoint lands in a WAV file. The image is a services session
built with `LAZYOS_USB_AUDIO=1`: `usbd` binds the device, `sndd usb=1` serves
it as the card `usb-audio0` and plays its 440 Hz self-test tone through it,
and `audiod demo=1` plays `beep`'s 880 Hz (and its probes) through the mixer
onto the same card. The recording must hold both tones, in that order.

`--both` adds a virtio-sound card recording to a second file: two outputs.
The mixer must list both (`AUDIOD:OUTPUTS`) and play through the USB one (a
USB device ranks first); `beep`'s 880 Hz lands on whichever card the mixer
held when it played (the virtio card can come up first), each card's own
driver tone on its own recording.

`--hotplug` (implies `--both`) then pulls the USB speaker out over QMP: the
mixer must fall back to the virtio card; and plugs it in again: the mixer
must move back to USB (`AUDIOD:CARD id=...` each time).

    python tools/sound/usb_run.py                 # build, boot, record, judge
    python tools/sound/usb_run.py --both          # two outputs, the mixer on USB
    python tools/sound/usb_run.py --hotplug       # --both, then unplug (back to virtio) and replug (USB again)
    python tools/sound/usb_run.py --no-build --accel none

Exit status is non-zero on any failure.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from qemu_qmp import DEFAULT_MEMORY, Qmp, accel_args, build_qemu_command, find_qemu, free_port  # noqa: E402

import analyze_wav  # noqa: E402
from run import stop_qemu, wait_for_marker  # noqa: E402

#: What must appear on serial: the device bound and configured by `usbd`,
#: the USB card served and its self-test played, the mixer on that card, and
#: `audiod demo=1`'s clients done.
PASS_MARKERS = (
    "USBD:AUDIO:BIND",
    "USBD:AUDIO:UP",
    "USBD:AUDIO:SERVE name=os.lazy.usb.audio",
    "SNDD:USB:CARD",
    "SNDD:READY name=os.lazy.audio.card/usb-audio0",
    "SND:PLAY:PASS",
    "AUDIOD:CARD id=usb-audio0",
    "BEEP:PLAY:PASS",
    "BEEP:PROBE:PASS",
    "MIXER:PROBE:PASS",
)
FAIL_MARKERS = (
    "SND:PLAY:FAIL",
    "BEEP:PLAY:FAIL",
    "BEEP:PROBE:FAIL",
    "BEEP:SOAK:FAIL",
    "BEEP:INTRUDER:FAIL",
    "MIXER:PROBE:FAIL",
    "AUDIOD:FAIL",
    "USBD:AUDIO:FAIL",
    "USBD:FATAL",
    "USBD:PANIC",
)
#: `--both`: the virtio card registered and listed beside the USB one.
BOTH_MARKERS = (
    "SNDD:READY name=os.lazy.audio.card/virtio-snd0",
)
DEMO_FREQS_HZ = "440,880"
#: The USB speaker's QEMU device, for `device_del` / `device_add`.
SPEAKER = "usbspeaker"
DEMO_MIN_MS = 640


def build_image(both: bool) -> Path:
    env = dict(os.environ, LAZYOS_SOUND="1", LAZYOS_USB="1", LAZYOS_USB_AUDIO="1",
               LAZYOS_SERVICES="1")
    print("building: LAZYOS_SOUND=1 LAZYOS_USB=1 LAZYOS_USB_AUDIO=1 LAZYOS_SERVICES=1 cargo build",
          flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"cargo build failed:\n{result.stderr[-4000:]}")
    image = ROOT / "target" / "lazyos.img"
    if not image.is_file():
        sys.exit(f"build succeeded but {image} does not exist")
    return image


def wav_arg(name: str, path: Path) -> list[str]:
    # Commas in a path are doubled for QEMU's option parser.
    return ["-audiodev", f"wav,id={name},path={path.resolve().as_posix().replace(',', ',,')}"]


def judge(text: str, usb_wav: Path, virtio_wav: Path | None, replugged: bool = False) -> int:
    """The verdict. After a replug QEMU has started the USB recording over,
    so it holds only the replugged card's own self-test tone."""
    markers = PASS_MARKERS + (BOTH_MARKERS if virtio_wav else ())
    missing = [m for m in markers if m not in text]
    failed = [m for m in FAIL_MARKERS if m in text]
    if failed or missing:
        for line in text.splitlines():
            if "AUDIO" in line or "SNDD" in line or any(m in line for m in FAIL_MARKERS):
                print(f"  {line}")
        what = f"reported {', '.join(failed)}" if failed else f"never reported {', '.join(missing)}"
        print(f"USBAUDIO:HARNESS:FAIL the guest {what}")
        return 1
    outputs = [line for line in text.splitlines() if "AUDIOD:OUTPUTS " in line]
    if virtio_wav and not any("usb-audio0" in o and "virtio-snd0" in o for o in outputs):
        print(f"USBAUDIO:HARNESS:FAIL the mixer never listed both outputs ({outputs})")
        return 1
    print("USBAUDIO:GUEST:PASS")
    if not usb_wav.is_file():
        print("USBAUDIO:HARNESS:FAIL QEMU wrote no WAV file for the USB device")
        return 1
    if not virtio_wav:
        print(f"judging the USB recording {usb_wav}:", flush=True)
        code = analyze_wav.main([str(usb_wav), "--expect-freq", DEMO_FREQS_HZ, "--min-ms", str(DEMO_MIN_MS)])
    else:
        # Each driver's own 440 Hz is on its card; `beep`'s 880 Hz went to
        # whichever output the mixer held when it played (the virtio card
        # can come up first; the switch to USB is judged from serial).
        code = 1
        layouts = ((("440", "440"), ("440", DEMO_FREQS_HZ)) if replugged
                   else ((DEMO_FREQS_HZ, "440"), ("440", DEMO_FREQS_HZ)))
        for usb, virtio in layouts:
            print(f"judging: USB {usb} Hz, virtio {virtio} Hz", flush=True)
            if (analyze_wav.main([str(usb_wav), "--expect-freq", usb, "--min-ms", str(DEMO_MIN_MS)]) == 0
                    and analyze_wav.main([str(virtio_wav), "--expect-freq", virtio,
                                          "--min-ms", str(DEMO_MIN_MS)]) == 0):
                code = 0
                break
        cards = [line.split("AUDIOD:CARD ", 1)[1].split()[0] for line in text.splitlines()
                 if "AUDIOD:CARD id=" in line]
        if code == 0 and "id=usb-audio0" not in cards[:2]:
            print(f"USBAUDIO:HARNESS:FAIL the mixer did not move to USB ({cards})")
            code = 1
    print("USBAUDIO:HARNESS:" + ("PASS" if code == 0 else "FAIL"))
    return code


def wait_count(serial_log: Path, marker: str, count: int, timeout: float) -> bool:
    """Wait until `marker` appeared `count` times on serial."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if serial_log.read_text(errors="replace").count(marker) >= count:
            return True
        time.sleep(0.25)
    return False


def hotplug(qmp: Qmp, serial_log: Path, timeout: float) -> str:
    """Unplug the speaker, see the mixer on virtio; plug it in, see it on
    USB again. Returns one line per step that held."""
    notes = []
    usb, virtio = "AUDIOD:CARD id=usb-audio0", "AUDIOD:CARD id=virtio-snd0"
    log = serial_log.read_text(errors="replace")
    seen, before = log.count(usb), log.count(virtio)
    qmp.execute("device_del", id=SPEAKER)
    if wait_count(serial_log, virtio, before + 1, timeout):
        notes.append("USBAUDIO:HOTPLUG:unplugged: virtio")
        print("  unplugged: the mixer is on virtio-snd0", flush=True)
        qmp.execute("device_add", driver="usb-audio", bus="xhciaudio.0", audiodev="usbsnd", id=SPEAKER)
        if wait_count(serial_log, usb, seen + 1, timeout):
            notes.append("USBAUDIO:HOTPLUG:replugged: usb")
            print("  replugged: the mixer is on usb-audio0", flush=True)
    return "\n".join(notes)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--image", default=str(ROOT / "target" / "lazyos.img"))
    parser.add_argument("--out", default="shots/usb_audio", help="output dir (serial.log, usb.wav)")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--memory", default=DEFAULT_MEMORY, help="guest RAM (default: %(default)s)")
    parser.add_argument("--timeout", type=float, default=180.0, help="seconds to wait for the guest")
    parser.add_argument("--no-build", action="store_true", help="skip the cargo build")
    parser.add_argument("--both", action="store_true",
                        help="also a virtio-sound card: two outputs, the mixer must pick USB")
    parser.add_argument("--hotplug", action="store_true",
                        help="--both, then unplug and replug the USB speaker: the mixer follows")
    args = parser.parse_args()
    args.both = args.both or args.hotplug

    out_dir = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    out_dir.mkdir(parents=True, exist_ok=True)
    serial_log, usb_wav, virtio_wav = out_dir / "serial.log", out_dir / "usb.wav", out_dir / "virtio.wav"
    for stale in (serial_log, usb_wav, virtio_wav):
        stale.unlink(missing_ok=True)
    image = Path(args.image) if args.no_build else build_image(args.both)

    qemu = find_qemu(args.qemu)
    extra = list(accel_args(args.accel, qemu) or [])
    extra += wav_arg("usbsnd", usb_wav)
    extra += ["-device", "qemu-xhci,id=xhciaudio",
              "-device", f"usb-audio,bus=xhciaudio.0,audiodev=usbsnd,id={SPEAKER}"]
    if args.both:
        extra += wav_arg("vsnd", virtio_wav) + ["-device", "virtio-sound-pci,audiodev=vsnd"]
    port = free_port()
    command = build_qemu_command(qemu, str(image), port, serial_log, args.memory, extra)
    print(f"launching: {' '.join(command)}", flush=True)
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    qmp = None
    notes = ""
    markers = PASS_MARKERS + (BOTH_MARKERS if args.both else ())
    try:
        qmp = Qmp("127.0.0.1", port, min(30.0, args.timeout))
        text = wait_for_marker(serial_log, proc, args.timeout, markers, FAIL_MARKERS)
        if args.hotplug and all(m in text for m in markers):
            notes = hotplug(qmp, serial_log, args.timeout)
        # Let the device play out what is queued, then the backend flush.
        time.sleep(1.5)
        text = serial_log.read_text(errors="replace") if serial_log.is_file() else text
        text += "\n" + notes
    finally:
        stop_qemu(proc, qmp)
    if args.hotplug:
        for step in ("unplugged: virtio", "replugged: usb"):
            if f"USBAUDIO:HOTPLUG:{step}" not in text:
                print(f"USBAUDIO:HARNESS:FAIL hot-plug: the mixer did not follow ({step})")
                return 1
        print("USBAUDIO:HOTPLUG:PASS")
    return judge(text, usb_wav, virtio_wav if args.both else None, args.hotplug)


if __name__ == "__main__":
    sys.exit(main())
