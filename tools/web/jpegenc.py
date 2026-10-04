"""A baseline JPEG encoder in the standard library, for the LazyWeb fixtures.

Sequential DCT, Huffman coded with the example tables of ITU T.81 Annex K,
YCbCr 4:4:4 (no subsampling: simpler, and the fixture photo is small), JFIF.
Deterministic, so `gen_fixtures.py --check` can compare the checked-in photo
byte for byte. Slow (pure Python), which is fine for a 240x160 picture.
"""

from __future__ import annotations

import math
import struct

#: Annex K.1 quantisation tables, natural (row-major) order.
LUMA_Q = [16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55,
          14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51, 87, 80, 62,
          18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92,
          49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99]
CHROMA_Q = [17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99,
            24, 26, 56, 99, 99, 99, 99, 99, 47, 66, 99, 99, 99, 99, 99, 99] + [99] * 32
ZIGZAG = [0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48,
          41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22,
          15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55,
          62, 63]
#: Annex K.3 Huffman tables: (code counts per length 1..16, symbols).
DC_LUMA = ([0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0], list(range(12)))
DC_CHROMA = ([0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0], list(range(12)))
AC_LUMA = ([0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D], bytes.fromhex(
    "01020300041105122131410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a"
    "25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475767778"
    "797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7"
    "c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9fa"))
AC_CHROMA = ([0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77], bytes.fromhex(
    "000102031104052131061241510761711322328108144291a1b1c109233352f0156272d10a162434e125f1"
    "1718191a262728292a35363738393a434445464748494a535455565758595a636465666768696a73747576"
    "7778797a82838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4"
    "c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9fa"))
#: The DCT's cosine basis, cos((2x+1)u pi/16), with the 1/sqrt(2) for u = 0.
COS = [[math.cos((2 * x + 1) * u * math.pi / 16) * (1 / math.sqrt(2) if u == 0 else 1)
        for x in range(8)] for u in range(8)]


def _codes(table) -> dict[int, tuple[int, int]]:
    """Symbol -> (code, length) for a (counts, symbols) table (Annex C)."""
    counts, symbols = table
    out, code, k = {}, 0, 0
    for length, count in enumerate(counts, 1):
        for _ in range(count):
            out[symbols[k]] = (code, length)
            code, k = code + 1, k + 1
        code <<= 1
    return out


class _Bits:
    def __init__(self) -> None:
        self.out = bytearray()
        self.acc = self.count = 0

    def put(self, value: int, length: int) -> None:
        self.acc = (self.acc << length) | (value & ((1 << length) - 1))
        self.count += length
        while self.count >= 8:
            byte = (self.acc >> (self.count - 8)) & 0xFF
            self.out.append(byte)
            if byte == 0xFF:
                self.out.append(0)  # byte stuffing
            self.count -= 8

    def flush(self) -> bytes:
        if self.count:
            self.put(0x7F, 8 - self.count)  # pad with ones
        return bytes(self.out)


def _category(value: int) -> tuple[int, int]:
    """(size, bits) of a coefficient in JPEG's magnitude coding."""
    size = abs(value).bit_length()
    return size, value if value >= 0 else value + (1 << size) - 1


def _fdct(block: list[float]) -> list[float]:
    rows = [[sum(block[y * 8 + x] * COS[u][x] for x in range(8)) for u in range(8)] for y in range(8)]
    return [sum(rows[y][u] * COS[v][y] for y in range(8)) / 4 for v in range(8) for u in range(8)]


def _encode_block(bits: _Bits, block, quant, dc_codes, ac_codes, previous: int) -> int:
    coefficients = _fdct(block)
    q = [int(round(coefficients[i] / quant[i])) for i in range(64)]
    zz = [q[i] for i in ZIGZAG]
    size, value = _category(zz[0] - previous)
    bits.put(*dc_codes[size])
    if size:
        bits.put(value, size)
    run = 0
    for coefficient in zz[1:]:
        if coefficient == 0:
            run += 1
            continue
        while run > 15:
            bits.put(*ac_codes[0xF0])
            run -= 16
        size, value = _category(coefficient)
        bits.put(*ac_codes[(run << 4) | size])
        bits.put(value, size)
        run = 0
    if run:
        bits.put(*ac_codes[0x00])  # end of block
    return zz[0]


def _segment(marker: int, data: bytes) -> bytes:
    return struct.pack(">BBH", 0xFF, marker, len(data) + 2) + data


def _dht(cls_id: int, table) -> bytes:
    counts, symbols = table
    return bytes([cls_id]) + bytes(counts) + bytes(symbols)


def jpeg(width: int, height: int, pixels: list[tuple[int, int, int]]) -> bytes:
    """A baseline JFIF JPEG of RGB `pixels` (quality 50, Annex K tables as is)."""
    if len(pixels) != width * height:
        raise ValueError("pixel count does not match the size")
    planes: list[list[float]] = [[], [], []]
    for r, g, b in pixels:
        planes[0].append(0.299 * r + 0.587 * g + 0.114 * b - 128)
        planes[1].append(-0.168736 * r - 0.331264 * g + 0.5 * b)
        planes[2].append(0.5 * r - 0.418688 * g - 0.081312 * b)
    head = b"\xff\xd8" + _segment(0xE0, b"JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00")
    head += _segment(0xDB, b"\x00" + bytes(LUMA_Q[i] for i in ZIGZAG)
                     + b"\x01" + bytes(CHROMA_Q[i] for i in ZIGZAG))
    head += _segment(0xC0, struct.pack(">BHHB", 8, height, width, 3)
                     + b"\x01\x11\x00\x02\x11\x01\x03\x11\x01")
    head += _segment(0xC4, _dht(0x00, DC_LUMA) + _dht(0x10, AC_LUMA)
                     + _dht(0x01, DC_CHROMA) + _dht(0x11, AC_CHROMA))
    head += _segment(0xDA, b"\x03\x01\x00\x02\x11\x03\x11\x00\x3f\x00")
    tables = [(LUMA_Q, _codes(DC_LUMA), _codes(AC_LUMA))] + [(CHROMA_Q, _codes(DC_CHROMA),
                                                               _codes(AC_CHROMA))] * 2
    bits, previous = _Bits(), [0, 0, 0]
    for by in range(0, height, 8):
        for bx in range(0, width, 8):
            for c, plane in enumerate(planes):
                # Edge blocks repeat the last row and column.
                block = [plane[min(by + y, height - 1) * width + min(bx + x, width - 1)]
                         for y in range(8) for x in range(8)]
                previous[c] = _encode_block(bits, block, *tables[c], previous[c])
    return head + bits.flush() + b"\xff\xd9"
