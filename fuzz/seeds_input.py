"""Seeds for the input fuzz targets (`gen_corpus.py`): inputmap pointer
scripts, USB HID descriptors, reports and report descriptors.
"""
import struct

# ---- inputmap pointer grammar -------------------------------------------------
#
# `libs/inputmap/src/fuzz.rs`: width and height (u16 LE), then ops. Each op is
# a byte (low 3 bits select it, bit 7 flushes after it) and a device byte.

P_REL, P_ABS, P_BUTTON, P_SCROLL, P_DROPPED, P_BOUNDS, P_RAW = 0, 1, 2, 4, 5, 6, 7
P_FLUSH = 0x80


def p_screen(width, height):
    return struct.pack("<HH", width, height)


def p_rel(dx, dy, device=2, flush=False):
    return bytes([P_REL | (P_FLUSH if flush else 0), device]) + struct.pack("<hh", dx, dy)


def p_abs(x, y, device=3):
    return bytes([P_ABS, device]) + struct.pack("<HH", x, y)


def p_button(usage, value, device=2):
    return bytes([P_BUTTON, device, usage, value])


def p_scroll(axis, notches, device=2):
    return bytes([P_SCROLL, device, axis]) + struct.pack("<b", notches)


def p_raw(kind, code, value, device=2):
    return bytes([P_RAW, device, kind]) + struct.pack("<Hi", code, value)


def inputmap_pointer_seeds():
    return {
        # Every edge of an 800x600 screen.
        "edges": p_screen(800, 600) + b"".join(p_rel(dx, dy, flush=True) for dx, dy in
                                                ((-32768, 0), (0, -32768), (32767, 0), (0, 32767), (-5, -5))),
        # Absolute corners, then a resize that re-clamps.
        "absolute_resize": p_screen(1024, 768) + p_abs(0xFFFF, 0xFFFF) + p_abs(0, 0) + p_abs(0x8000, 0x8000)
        + bytes([P_BOUNDS, 0]) + struct.pack("<HH", 320, 200) + p_abs(0xFFFF, 0xFFFF),
        # Two devices hold one button; a loss marker releases everything.
        "two_devices_dropped": p_screen(640, 480) + p_button(1, 1, 2) + p_button(1, 1, 3) + p_button(1, 0, 2)
        + p_rel(3, 3) + bytes([P_DROPPED, 0]) + p_button(1, 0, 3),
        # Wheel on both axes around button edges.
        "wheel": p_screen(640, 480) + p_scroll(0, 3) + p_button(2, 1) + p_scroll(0, -1) + p_scroll(1, 2)
        + p_button(2, 0) + p_rel(0, 0, flush=True),
        # Hostile records: wrong codes, non-edge values, foreign kinds.
        "hostile": p_screen(1, 1) + p_raw(4, 0, 1) + p_raw(4, 6, 1) + p_raw(4, 1, 2) + p_raw(5, 2, 1)
        + p_raw(2, 1, 5) + p_raw(1, 4, 1) + p_raw(200, 0xFFFF, -1) + p_raw(5, 0, 0x7FFFFFFF),
        "degenerate_screen": p_screen(0, 0) + p_rel(100, 100, flush=True) + p_abs(0xFFFF, 0xFFFF),
        "empty": b"",
    }


# ---- usbhid (libs/usbhid/src/fuzz.rs) ---------------------------------------
#
# `usbdesc` takes raw descriptor bytes; the QEMU HID devices' high-speed
# configurations (libs/usbhid/src/tests/golden.rs) plus hostile edits.

KBD_CONFIG = bytes([9, 2, 34, 0, 1, 1, 8, 0xA0, 50, 9, 4, 0, 0, 1, 3, 1, 1, 0,
                    9, 0x21, 0x11, 0x01, 0, 1, 0x22, 0x3F, 0, 7, 5, 0x81, 3, 8, 0, 7])
MOUSE_CONFIG = bytes([9, 2, 34, 0, 1, 1, 6, 0xA0, 50, 9, 4, 0, 0, 1, 3, 1, 2, 0,
                      9, 0x21, 0x01, 0x00, 0, 1, 0x22, 52, 0, 7, 5, 0x81, 3, 4, 0, 7])
TABLET_CONFIG = bytes([9, 2, 34, 0, 1, 1, 7, 0xA0, 50, 9, 4, 0, 0, 1, 3, 0, 0, 0,
                       9, 0x21, 0x01, 0x00, 0, 1, 0x22, 74, 0, 7, 5, 0x81, 3, 8, 0, 4])
KBD_DEVICE = bytes([18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x27, 0x06, 0x01, 0x00, 0, 0, 1, 4, 11, 1])


def _patched(data, at, value):
    out = bytearray(data)
    out[at] = value
    return bytes(out)


def usbdesc_seeds():
    return {
        "kbd_config": KBD_CONFIG,
        "mouse_config": MOUSE_CONFIG,
        "tablet_config": TABLET_CONFIG,
        "kbd_device": KBD_DEVICE,
        "zero_blength": _patched(KBD_CONFIG, 9, 0),
        "record_overrun": _patched(KBD_CONFIG, 27, 9),
        "total_past_end": _patched(KBD_CONFIG, 2, 200),
        # A composite device: keyboard then mouse interface in one chain.
        "composite": bytes([9, 2, 59, 0, 2, 1, 0, 0xA0, 50]) + KBD_CONFIG[9:] + MOUSE_CONFIG[9:11]
        + bytes([1]) + MOUSE_CONFIG[12:],
        "empty": b"",
    }


def _report(selector, data):
    return bytes([selector, len(data)]) + bytes(data)


def hidreport_seeds():
    kbd, mouse = 0, 1
    return {
        "typing": _report(kbd, [0x02, 0, 0x04, 0, 0, 0, 0, 0]) + _report(kbd, [0x02, 0, 0x04, 0x05, 0, 0, 0, 0])
        + _report(kbd, [0, 0, 0x05, 0, 0, 0, 0, 0]) + _report(kbd, [0, 0, 0, 0, 0, 0, 0, 0]),
        "rollover": _report(kbd, [0, 0, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09]) + _report(kbd, [0, 0, 1, 1, 1, 1, 1, 1])
        + _report(kbd, [0, 0, 0, 0, 0, 0, 0, 0]),
        "hostile_keys": _report(kbd, [0xFF, 0, 0xFF, 0xE8, 0x03, 0x04, 0, 0]) + _report(kbd, [0x00])
        + _report(kbd, [0x01, 0, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x05, 0x06, 0x07]),
        "mouse": _report(mouse, [0x01, 5, 0xFB, 0x01]) + _report(mouse, [0x03, 0, 0]) + _report(mouse, [0xFF, 0x80, 0x7F, 0x80])
        + _report(mouse, [0, 0]) + _report(mouse, [0, 0, 0, 0]),
        "empty": b"",
    }


# `hidreportdesc`: a length byte, then a report descriptor and that many
# report bytes (QEMU's tablet and mouse, a report-id device, hostile edits).
TABLET_REPORT = bytes([
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29,
    0x05, 0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x03,
    0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x7F, 0x35, 0x00,
    0x46, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0x05, 0x01, 0x09, 0x38, 0x15, 0x81,
    0x25, 0x7F, 0x35, 0x00, 0x45, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xC0, 0xC0])
MOUSE_REPORT = bytes([
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29,
    0x05, 0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x03,
    0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75,
    0x08, 0x95, 0x03, 0x81, 0x06, 0xC0, 0xC0])


def hidreportdesc_seeds():
    two_reports = bytes([
        0x85, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95,
        0x08, 0x81, 0x02, 0x85, 0x02, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x95, 0x03, 0x75, 0x01,
        0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x03, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15,
        0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06])
    return {
        "tablet": bytes([6]) + TABLET_REPORT + bytes([0x01, 0x00, 0x40, 0xFF, 0x7F, 0xFF]),
        "mouse": bytes([4]) + MOUSE_REPORT + bytes([0x06, 0xFE, 0x05, 0x01]),
        "report_ids": bytes([4]) + two_reports + bytes([2, 1, 3, 0xFD]),
        "push_pop": bytes([0]) + bytes([0xA4] * 8 + [0xB4] * 12) + TABLET_REPORT,
        "truncated": bytes([0]) + TABLET_REPORT[:41],
        "long_item": bytes([0]) + bytes([0xFE, 0x05, 0x10]) + TABLET_REPORT,
        "empty": b"",
    }
