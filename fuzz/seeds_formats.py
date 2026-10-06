"""Seeds for the file-format fuzz targets (`gen_corpus.py`): `.lzp` zip
archives, IPP messages and PWG Raster pages.
"""
import struct
import zlib

# ---- lazypkg zip grammar ------------------------------------------------------


def _crc32(data):
    return zlib.crc32(data) & 0xFFFFFFFF


def _zip(entries):
    """Serialize `(name, data, deflate)` tuples into a deterministic zip."""
    local = b""
    central = b""
    for name, data, deflate in entries:
        name_bytes = name.encode("utf-8")
        crc = _crc32(data)
        if deflate:
            compressor = zlib.compressobj(9, zlib.DEFLATED, -15)
            stored = compressor.compress(data) + compressor.flush()
            method = 8
        else:
            stored = data
            method = 0
        offset = len(local)
        local += struct.pack(
            "<IHHHHHIIIHH",
            0x04034B50, 20, 0, method, 0, 0, crc, len(stored), len(data), len(name_bytes), 0,
        )
        local += name_bytes + stored
        central += struct.pack(
            "<IHHHHHHIIIHHHHHII",
            0x02014B50, 20, 20, 0, method, 0, 0, crc, len(stored), len(data),
            len(name_bytes), 0, 0, 0, 0, 0, offset,
        )
        central += name_bytes
    eocd = struct.pack(
        "<IHHHHIIH", 0x06054B50, 0, 0, len(entries), len(entries), len(central), len(local), 0,
    )
    return local + central + eocd


_MANIFEST = (
    b'[app]\nname = "Demo"\nsystem_name = "org.lazy.demo"\nauthor = "Tester"\nversion = "1.0.0"\n'
    b'\n[entry]\nbinary = "bin/app.elf"\n'
)
_PNG = b"\x89PNG\r\n\x1a\n" + b"\x00\x00\x00\x0dIHDR"


def lazypkg_seeds():
    members = [
        ("manifest.toml", _MANIFEST, False),
        ("bin/app.elf", b"ELF fake binary", False),
        ("icons/app-16.png", _PNG, False),
        ("icons/app-32.png", _PNG, False),
        ("icons/app-128.png", _PNG, False),
    ]
    valid_stored = _zip(members)
    valid_deflated = _zip([(name, data, True) for name, data, _ in members])
    bad_path = _zip(members + [("../evil", b"escape", False)])
    return {
        "valid_stored": valid_stored,
        "valid_deflated": valid_deflated,
        "bad_path": bad_path,
        "truncated": valid_stored[: len(valid_stored) // 2],
    }


# ---- IPP messages (libs/ipp, RFC 8010) and PWG Raster (libs/raster) -----------


def ipp_attr(tag, name, value):
    """One attribute value: tag, name, value (an empty name adds a value)."""
    n = name.encode()
    return struct.pack(">BH", tag, len(n)) + n + struct.pack(">H", len(value)) + value


def ipp_message(code, groups):
    body = struct.pack(">BBHI", 2, 0, code, 1)
    for group_tag, attrs in groups:
        body += bytes([group_tag]) + b"".join(attrs)
    return body + b"\x03"


def ipp_seeds():
    operation = [
        ipp_attr(0x47, "attributes-charset", b"utf-8"),
        ipp_attr(0x48, "attributes-natural-language", b"en"),
        ipp_attr(0x45, "printer-uri", b"ipp://10.0.2.2:8631/ipp/print"),
    ]
    get_printer = ipp_message(0x000B, [(1, operation + [
        ipp_attr(0x44, "requested-attributes", b"all"),
        ipp_attr(0x44, "", b"media-col-database"),
    ])])
    size = (ipp_attr(0x34, "", b"")
            + ipp_attr(0x4A, "", b"x-dimension") + ipp_attr(0x21, "", struct.pack(">i", 21000))
            + ipp_attr(0x4A, "", b"y-dimension") + ipp_attr(0x21, "", struct.pack(">i", 29700))
            + ipp_attr(0x37, "", b""))
    printer = [
        ipp_attr(0x41, "printer-make-and-model", b"HP DeskJet 3700 series"),
        ipp_attr(0x23, "printer-state", struct.pack(">i", 3)),
        ipp_attr(0x32, "pwg-raster-document-resolution-supported", struct.pack(">iiB", 300, 300, 3)),
        ipp_attr(0x33, "copies-supported", struct.pack(">ii", 1, 99)),
        ipp_attr(0x22, "page-ranges-supported", b"\x01"),
        ipp_attr(0x42, "marker-names", b"tri-color ink"),
        ipp_attr(0x42, "", b"black ink"),
        ipp_attr(0x21, "marker-levels", struct.pack(">i", 90)),
        ipp_attr(0x21, "", struct.pack(">i", 50)),
        ipp_attr(0x34, "media-col-default", b"") + ipp_attr(0x4A, "", b"media-size") + size
        + ipp_attr(0x37, "", b""),
        ipp_attr(0x35, "printer-info", struct.pack(">H", 2) + b"en" + struct.pack(">H", 6) + b"DeskJe"),
        ipp_attr(0x12, "printer-current-time", b""),
    ]
    reply = ipp_message(0x0000, [(1, operation[:2]), (4, printer)])
    job = [
        ipp_attr(0x21, "job-id", struct.pack(">i", 7)),
        ipp_attr(0x23, "job-state", struct.pack(">i", 5)),
        ipp_attr(0x44, "job-state-reasons", b"media-empty"),
    ]
    return {
        "get_printer_attributes": get_printer,
        "printer_reply": reply,
        "job_reply": ipp_message(0x0000, [(1, operation[:2]), (2, job)]),
        "print_job_with_document": ipp_message(0x0002, [(1, operation)]) + b"RaS2",
        "error_reply": ipp_message(0x040A, [(1, operation[:2])]),
        "empty": b"",
    }


def pwg_header(width, height, gray=True):
    """A PWG Raster page header (libs/raster/src/header.rs offsets)."""
    h = bytearray(1796)
    put = lambda at, v: struct.pack_into(">I", h, at, v)  # noqa: E731
    bpp = 8 if gray else 24
    h[0:9] = b"PwgRaster"
    put(276, 300); put(280, 300)  # noqa: E702
    put(372, width); put(376, height)  # noqa: E702
    put(384, 8); put(388, bpp); put(392, width * bpp // 8)  # noqa: E702
    put(400, 18 if gray else 19)
    put(420, 1 if gray else 3)
    h[1732:1748] = b"iso_a4_210x297mm"
    return bytes(h)


def pwgraster_seeds():
    gray = pwg_header(8, 4) + bytes([
        1, 7, 0xFF,                    # two lines of eight white pixels
        0, 0xFD, 1, 2, 3, 4, 3, 0x00,  # four literals (257 - 4), four black
        0, 4, 0x00, 2, 0x80,           # five black, three grey
    ])
    rgb = pwg_header(2, 1, gray=False) + bytes([0, 1, 255, 0, 0])
    return {
        "gray_page": b"RaS2" + gray,
        "rgb_page": b"RaS2" + rgb,
        "two_pages": b"RaS2" + rgb + rgb,
        "header_only": b"RaS2" + pwg_header(8, 4),
        "empty": b"",
    }
