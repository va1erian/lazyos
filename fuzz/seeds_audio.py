"""Seeds for the `usbaudio` target: USB Audio Class configuration chains
(`libs/usbaudio/src/fuzz.rs`), the same shapes as the crate's host tests."""


def _record(*body):
    return bytes([len(body) + 1]) + bytes(body)


def _config(*records):
    chain = b"".join(records)
    total = 9 + len(chain)
    return bytes([9, 2, total & 0xFF, total >> 8, 2, 1, 0, 0x80, 0x32]) + chain


# QEMU's `usb-audio`: UAC 1.0, 48 kHz stereo 16-bit, synchronous.
QEMU = _config(
    _record(0x04, 0, 0, 0, 1, 1, 0, 0),
    _record(0x24, 1, 0x00, 0x01, 0x28, 0x00, 1, 1),
    _record(0x24, 2, 1, 0x01, 0x01, 0, 2, 3, 0, 0, 0),
    _record(0x24, 6, 2, 1, 1, 3, 0, 0, 0),
    _record(0x24, 3, 3, 0x01, 0x03, 0, 2, 0),
    _record(0x04, 1, 0, 0, 1, 2, 0, 0),
    _record(0x04, 1, 1, 1, 1, 2, 0, 0),
    _record(0x24, 1, 1, 1, 1, 0),
    _record(0x24, 2, 1, 2, 2, 16, 1, 0x80, 0xBB, 0x00),
    _record(0x05, 0x01, 0x0D, 0xC4, 0x00, 1, 0, 0),
    _record(0x25, 1, 0, 0, 0, 0),
)

# A UAC 2.0 asynchronous DAC with a clock, a feedback endpoint and a range.
DAC = _config(
    _record(0x0B, 0, 2, 1, 0, 0x20, 0),
    _record(0x04, 0, 0, 0, 1, 1, 0x20, 0),
    _record(0x24, 1, 0x00, 0x02, 8, 0x40, 0, 0),
    _record(0x24, 0x0A, 0x29, 3, 7, 0, 0),
    _record(0x24, 2, 2, 0x01, 0x01, 0, 0x29, 2, 3, 0, 0, 0, 0, 0, 0, 0),
    _record(0x24, 6, 0x0A, 2, 0x0F, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
    _record(0x24, 3, 0x14, 0x01, 0x03, 0, 0x0A, 0x29, 0, 0, 0),
    _record(0x04, 1, 0, 0, 1, 2, 0x20, 0),
    _record(0x04, 1, 1, 2, 1, 2, 0x20, 0),
    _record(0x24, 1, 2, 0, 1, 1, 0, 0, 0, 2, 3, 0, 0, 0, 0),
    _record(0x24, 2, 1, 4, 24),
    _record(0x05, 0x01, 0x05, 0xC8, 0x00, 1),
    _record(0x25, 1, 0, 0, 0, 0, 0),
    _record(0x05, 0x81, 0x11, 4, 0, 4),
)


def _patched(data, at, value):
    out = bytearray(data)
    out[at] = value
    return bytes(out)


def _range_reply():
    out = bytes([4, 0])
    for rate in (44100, 48000, 96000, 192000):
        for value in (rate, rate, 0):
            out += value.to_bytes(4, "little")
    return out


def usbaudio_seeds():
    return {
        "qemu": QEMU,
        "uac2_dac": DAC,
        "range_reply": _range_reply(),
        "zero_blength": _patched(QEMU, 18, 0),
        "big_subslot": _patched(QEMU, QEMU.index(bytes([0x0B, 0x24, 0x02])) + 5, 9),
        "selector_loop": DAC.replace(bytes([0x24, 0x0A, 0x29]), bytes([0x24, 0x0B, 0x29])),
        "empty": b"",
    }
