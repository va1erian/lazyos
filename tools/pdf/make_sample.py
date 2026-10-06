#!/usr/bin/env python3
"""Writes the PDF Viewer's sample documents (docs/pdf-reader-plan.md).

    python tools/pdf/make_sample.py            # write them
    python tools/pdf/make_sample.py --check    # fail if the checked-in ones are stale

`assets/samples/lazyos-sample.pdf` ships in every desktop image
(`/system/share/samples/lazyos-sample.pdf`); the viewer's session opens it.
`xui-app/crates/pdf/testdata/` gets the same document plus encrypted twins
for the library's tests (and an uncompressed copy the fuzz test mutates). Every page exercises something a renderer can get
wrong: standard 14 fonts that are not embedded, an embedded TrueType font,
an outline, an internal and an external link, transparency, a JPEG and a
JPEG 2000 image, a page with /Rotate 90 and a landscape page.

Needs PyMuPDF (`pip install pymupdf`) and Pillow; it is a host tool only and
nothing it links ends up in the image.
"""

from __future__ import annotations

import argparse
import io
import sys
from pathlib import Path

import fitz  # PyMuPDF
from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
SAMPLE = ROOT / "assets" / "samples" / "lazyos-sample.pdf"
TESTDATA = ROOT / "xui-app" / "crates" / "pdf" / "testdata"
SERIF = ROOT / "assets" / "fonts" / "DroidSerif-Regular.ttf"

# The password of the encrypted twins (test data only).
PASSWORD = "lazyos"
# A fixed date so a rerun writes the same document.
DATE = "D:20261006120000Z"

A4 = fitz.paper_rect("a4")
MARGIN = 56

INTRO = (
    "LazyOS reads PDF documents with hayro, a PDF rasterizer written in pure "
    "Rust. This sample exercises the parts of a renderer that are easy to get "
    "wrong, one page at a time, and doubles as the document the viewer's "
    "screenshot session opens."
)

FIND_WORD = "lighthouse"

# Where the title page's two links sit.
GOTO_RECT = fitz.Rect(MARGIN + 12, 478, MARGIN + 260, 494)
URI_RECT = fitz.Rect(MARGIN + 12, 498, MARGIN + 260, 514)


def gradient_image(fmt: str, size: int = 256) -> bytes:
    """A smooth colour gradient with a grid, encoded as `fmt`."""
    img = Image.new("RGB", (size, size))
    px = img.load()
    for y in range(size):
        for x in range(size):
            grid = 40 if (x // 32 + y // 32) % 2 else 0
            px[x, y] = (x * 255 // size, y * 255 // size, 160 + grid // 2)
    out = io.BytesIO()
    img.save(out, format=fmt, **({"quality": 90} if fmt == "JPEG" else {}))
    return out.getvalue()


def text_block(page: fitz.Page, rect: fitz.Rect, text: str, font: str, size: float) -> None:
    left = page.insert_textbox(rect, text, fontname=font, fontsize=size)
    if left < 0:
        raise SystemExit(f"text does not fit on page {page.number + 1}")


def page_title(doc: fitz.Document) -> None:
    page = doc.new_page(width=A4.width, height=A4.height)
    page.insert_text((MARGIN, 110), "LazyOS PDF Viewer", fontname="hebo", fontsize=30)
    page.insert_text((MARGIN, 140), "A sample document", fontname="helv", fontsize=16,
                     color=(0.3, 0.3, 0.3))
    text_block(page, fitz.Rect(MARGIN, 170, A4.width - MARGIN, 300), INTRO, "tiro", 12)
    page.insert_text((MARGIN, 330), "Standard 14 fonts, not embedded:", fontname="hebo", fontsize=12)
    for i, (font, name) in enumerate([("helv", "Helvetica"), ("tiro", "Times-Roman"),
                                      ("cour", "Courier"), ("tiit", "Times-Italic"),
                                      ("cobo", "Courier-Bold")]):
        page.insert_text((MARGIN + 12, 352 + i * 18), f"{name}: The quick brown fox jumps over the lazy dog.",
                         fontname=font, fontsize=11)
    page.insert_text((MARGIN, 470), "Links:", fontname="hebo", fontsize=12)
    goto = GOTO_RECT
    page.insert_text((goto.x0, goto.y1 - 4), "Go to the pictures (page 3)", fontname="helv",
                     fontsize=11, color=(0, 0.2, 0.8))
    uri = URI_RECT
    page.insert_text((uri.x0, uri.y1 - 4), "https://example.com/", fontname="helv",
                     fontsize=11, color=(0, 0.2, 0.8))
    page.insert_text((MARGIN, A4.height - 40), "Page 1", fontname="helv", fontsize=9)


def page_embedded(doc: fitz.Document) -> None:
    page = doc.new_page(width=A4.width, height=A4.height)
    page.insert_font(fontname="droid", fontfile=str(SERIF))
    page.insert_text((MARGIN, 90), "An embedded TrueType font", fontname="droid", fontsize=22)
    body = (
        "This page is set in Droid Serif, embedded (subset) in the file, so "
        "no substitute is needed. Accents and symbols: café, naïve, Ærøskøbing, "
        "Ångström, déjà vu, € 42, ±1 °C, “quotes” and ‘single’.\n\n"
        "A keeper climbed the lighthouse every evening to light the lamp. "
        "The word lighthouse appears three times on this page, which the "
        "find bar counts: lighthouse."
    )
    text_block(page, fitz.Rect(MARGIN, 110, A4.width - MARGIN, 330), body, "droid", 13)
    page.insert_text((MARGIN, A4.height - 40), "Page 2", fontname="helv", fontsize=9)


def page_pictures(doc: fitz.Document) -> None:
    page = doc.new_page(width=A4.width, height=A4.height)
    page.insert_text((MARGIN, 90), "Pictures and transparency", fontname="hebo", fontsize=22)
    colours = [(0.9, 0.1, 0.1), (0.1, 0.7, 0.1), (0.1, 0.2, 0.9)]
    for i, c in enumerate(colours):
        centre = fitz.Point(MARGIN + 90 + i * 60, 200 + (i % 2) * 50)
        page.draw_circle(centre, 70, color=None, fill=c, fill_opacity=0.5)
    page.draw_rect(fitz.Rect(330, 130, 540, 300), color=(0, 0, 0), width=3, dashes="[6 3] 0")
    page.draw_bezier((340, 290), (380, 120), (480, 320), (530, 140), color=(0.8, 0.4, 0), width=4)
    page.insert_text((MARGIN, 370), "JPEG (DCTDecode):", fontname="helv", fontsize=11)
    page.insert_image(fitz.Rect(MARGIN, 380, MARGIN + 200, 580), stream=gradient_image("JPEG"))
    page.insert_text((300, 370), "JPEG 2000 (JPXDecode):", fontname="helv", fontsize=11)
    page.insert_image(fitz.Rect(300, 380, 500, 580), stream=gradient_image("JPEG2000"))
    page.insert_text((MARGIN, A4.height - 40), "Page 3", fontname="helv", fontsize=9)


def page_rotated(doc: fitz.Document) -> None:
    page = doc.new_page(width=A4.width, height=A4.height)
    page.insert_text((MARGIN, 90), "This page has /Rotate 90", fontname="hebo", fontsize=22)
    page.insert_text((MARGIN, 120), "Viewers show it in landscape, turned clockwise.",
                     fontname="helv", fontsize=12)
    page.draw_rect(fitz.Rect(MARGIN, 140, MARGIN + 120, 200), color=None, fill=(0.95, 0.75, 0.1))
    page.insert_text((MARGIN, A4.height - 40), "Page 4", fontname="helv", fontsize=9)
    page.set_rotation(90)


def page_landscape(doc: fitz.Document) -> None:
    page = doc.new_page(width=A4.height, height=A4.width)
    page.insert_text((MARGIN, 90), "A landscape page (MediaBox wider than tall)",
                     fontname="hebo", fontsize=22)
    for i in range(10):
        x = MARGIN + i * 70
        page.draw_rect(fitz.Rect(x, 140, x + 60, 140 + (i + 1) * 30), color=None,
                       fill=(i / 10, 0.4, 1 - i / 10))
    page.insert_text((MARGIN, A4.width - 40), "Page 5", fontname="helv", fontsize=9)


def page_long_text(doc: fitz.Document) -> None:
    page = doc.new_page(width=A4.width, height=A4.height)
    page.insert_text((MARGIN, 80), "Plenty of text", fontname="hebo", fontsize=22)
    lines = [f"Line {n:02}: the {FIND_WORD} beam swept the bay once every ten seconds."
             if n % 7 == 0 else f"Line {n:02}: Lorem ipsum dolor sit amet, consectetur adipiscing elit."
             for n in range(1, 45)]
    text_block(page, fitz.Rect(MARGIN, 100, A4.width - MARGIN, A4.height - 60),
               "\n".join(lines), "tiro", 11)
    page.insert_text((MARGIN, A4.height - 40), "Page 6", fontname="helv", fontsize=9)


def build() -> fitz.Document:
    doc = fitz.open()
    for make in (page_title, page_embedded, page_pictures, page_rotated, page_landscape, page_long_text):
        make(doc)
    # Links go in once every page exists (a GoTo names its target page).
    title = doc[0]
    title.insert_link({"kind": fitz.LINK_GOTO, "from": GOTO_RECT, "page": 2, "to": fitz.Point(0, 0)})
    title.insert_link({"kind": fitz.LINK_URI, "from": URI_RECT, "uri": "https://example.com/"})
    doc.set_metadata({
        "title": "LazyOS PDF Viewer sample",
        "author": "LazyOS",
        "subject": "Renderer test document",
        "creator": "tools/pdf/make_sample.py",
        "producer": "PyMuPDF",
        "creationDate": DATE,
        "modDate": DATE,
    })
    doc.set_toc([
        [1, "Title page", 1],
        [2, "Links", 1],
        [1, "Embedded font", 2],
        [1, "Pictures", 3],
        [1, "Rotated page", 4],
        [1, "Landscape page", 5],
        [1, "Plenty of text", 6],
    ])
    return doc


def render(doc: fitz.Document, **save_opts) -> bytes:
    save_opts.setdefault("deflate", True)
    return doc.tobytes(garbage=4, no_new_id=True, **save_opts)


def outputs() -> dict[Path, bytes]:
    doc = build()
    plain = render(doc)
    aes = render(doc, encryption=fitz.PDF_ENCRYPT_AES_256, user_pw=PASSWORD, owner_pw=PASSWORD + "-owner")
    rc4 = render(doc, encryption=fitz.PDF_ENCRYPT_RC4_128, user_pw=PASSWORD, owner_pw=PASSWORD + "-owner")
    # Every stream decompressed: the fuzz test's mutations then hit the
    # content-stream and font parsers instead of failing in Flate.
    plain_streams = render(doc, deflate=False, expand=255)
    return {
        SAMPLE: plain,
        TESTDATA / "sample.pdf": plain,
        TESTDATA / "sample-aes256.pdf": aes,
        TESTDATA / "sample-rc4.pdf": rc4,
        TESTDATA / "sample-uncompressed.pdf": plain_streams,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="fail if a written file is missing or differs")
    args = ap.parse_args()
    stale = []
    for path, data in outputs().items():
        if args.check:
            # Encryption salts are random, so the encrypted twins are only checked to exist.
            if not path.exists() or ("aes" not in path.name and "rc4" not in path.name
                                     and path.read_bytes() != data):
                stale.append(path)
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        print(f"wrote {path.relative_to(ROOT)} ({len(data)} bytes)")
    if stale:
        for path in stale:
            print(f"stale: {path.relative_to(ROOT)} (run tools/pdf/make_sample.py)", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
