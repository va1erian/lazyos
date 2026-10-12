#!/usr/bin/env python3
"""Generate the RTL8822C register tables of `libs/rtw88` from upstream C.

Linux's `drivers/net/wireless/realtek/rtw88/rtw8822c_table.c` (dual-licensed
`GPL-2.0 OR BSD-3-Clause`, taken under BSD-3-Clause, docs/wifi-rtw88-plan.md
section 2) holds about 46 000 lines of chip data: the MAC/BB/AGC/RF
initialisation tables with their condition headers, the calibration init
table, the TX power by-rate and limit tables and the DPK register tables.
This script turns them into compact Rust statics under
`libs/rtw88/src/rtw8822c/tables/` (`gen_*.rs`, `@generated`, so the 500-line
rule does not apply). Nothing is interpreted here: the condition headers stay
in the word stream exactly as upstream has them, and `phy::cond` evaluates
them at run time (`rtw_parse_tbl_phy_cond`), so a reader can hold a generated
array next to the C one.

Each table's `RTW_DECL_TABLE_*` line is checked against the declaration this
crate expects (which parser, which RF path), so an upstream change in how a
table is used fails here instead of loading wrong.

    python tools/wifi/gen_rtw88_tables.py --src PATH/rtw8822c_table.c
    python tools/wifi/gen_rtw88_tables.py --fetch DIR      # download the pinned file into DIR, then generate
    python tools/wifi/gen_rtw88_tables.py --check [--src PATH]

`--check` regenerates in memory and fails if a generated file differs; with
no source (no `--src`, `RTW88_TABLE_SRC` unset or pointing nowhere) it says so
and passes, since the C file is not vendored.
"""
from __future__ import annotations

import argparse
import hashlib
import os
import re
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT_DIR = ROOT / "libs" / "rtw88" / "src" / "rtw8822c" / "tables"
COMMIT = "d6d3455d8751854e0d1820f14184682e23ac4475"
URL = (
    "https://raw.githubusercontent.com/torvalds/linux/"
    f"{COMMIT}/drivers/net/wireless/realtek/rtw88/rtw8822c_table.c"
)
PAIRS_PER_LINE = 8
TRIPLES_PER_LINE = 4
ROWS_PER_LINE = 6

# kind: "pairs" (u32 address/data words, condition headers included),
# "triples" (address, mask, data: the DPK tables), "pg" (rtw_phy_pg_cfg_pair),
# "lmt" (rtw_txpwr_lmt_cfg_pair).
# (Rust name, upstream array, kind, the declaration upstream must have)
TABLES = {
    "gen_bb.rs": [
        ("MAC", "rtw8822c_mac", "pairs", "RTW_DECL_TABLE_PHY_COND(rtw8822c_mac, rtw_phy_cfg_mac)"),
        ("AGC", "rtw8822c_agc", "pairs", "RTW_DECL_TABLE_PHY_COND(rtw8822c_agc, rtw_phy_cfg_agc)"),
        ("BB", "rtw8822c_bb", "pairs", "RTW_DECL_TABLE_PHY_COND(rtw8822c_bb, rtw_phy_cfg_bb)"),
        ("CAL_INIT", "rtw8822c_array_mp_cal_init", "pairs",
         "RTW_DECL_TABLE_PHY_COND(rtw8822c_array_mp_cal_init, rtw_phy_cfg_bb)"),
    ],
    "gen_rf_a.rs": [
        ("RF_A", "rtw8822c_rf_a", "pairs", "RTW_DECL_TABLE_RF_RADIO(rtw8822c_rf_a, A)"),
    ],
    "gen_rf_b.rs": [
        ("RF_B", "rtw8822c_rf_b", "pairs", "RTW_DECL_TABLE_RF_RADIO(rtw8822c_rf_b, B)"),
    ],
    "gen_txpwr.rs": [
        ("BB_PG_TYPE0", "rtw8822c_bb_pg_type0", "pg", "RTW_DECL_TABLE_BB_PG(rtw8822c_bb_pg_type0)"),
        ("TXPWR_LMT_TYPE0", "rtw8822c_txpwr_lmt_type0", "lmt",
         "RTW_DECL_TABLE_TXPWR_LMT(rtw8822c_txpwr_lmt_type0)"),
        ("TXPWR_LMT_TYPE5", "rtw8822c_txpwr_lmt_type5", "lmt",
         "RTW_DECL_TABLE_TXPWR_LMT(rtw8822c_txpwr_lmt_type5)"),
    ],
    "gen_dpk.rs": [
        ("DPK_AFE_NO_DPK", "rtw8822c_dpk_afe_no_dpk", "triples",
         "RTW_DECL_TABLE_DPK(rtw8822c_dpk_afe_no_dpk)"),
        ("DPK_AFE_IS_DPK", "rtw8822c_dpk_afe_is_dpk", "triples",
         "RTW_DECL_TABLE_DPK(rtw8822c_dpk_afe_is_dpk)"),
        ("DPK_MAC_BB", "rtw8822c_dpk_mac_bb", "triples", "RTW_DECL_TABLE_DPK(rtw8822c_dpk_mac_bb)"),
    ],
}

DOCS = {
    "pairs": "Address/data word pairs, condition headers included (`phy::cond`).",
    "triples": "`(address, mask, data)` writes (`rtw8822c_parse_tbl_dpk`).",
    "pg": "`[band, rf_path, tx_num, addr, bitmask, data]` (`struct rtw_phy_pg_cfg_pair`).",
    "lmt": "`[regd, band, bw, rate_section, channel, limit as i8]` (`struct rtw_txpwr_lmt_cfg_pair`).",
}


class GenError(Exception):
    pass


def strip_comments(src: str) -> str:
    src = re.sub(r"/\*.*?\*/", " ", src, flags=re.S)
    return re.sub(r"//[^\n]*", " ", src)


def eval_word(tok: str) -> int:
    """One initialiser item: a number or the masks the DPK tables use."""
    tok = tok.strip()
    named = {"MASKDWORD": 0xFFFFFFFF, "MASKBYTE0": 0xFF, "MASKBYTE1": 0xFF00,
             "MASKBYTE2": 0xFF0000, "MASKBYTE3": 0xFF000000, "MASKLWORD": 0xFFFF,
             "MASKHWORD": 0xFFFF0000, "RFREG_MASK": 0xFFFFF}
    if tok in named:
        return named[tok]
    m = re.fullmatch(r"BIT\((\d+)\)", tok)
    if m:
        return 1 << int(m.group(1))
    m = re.fullmatch(r"GENMASK\((\d+),\s*(\d+)\)", tok)
    if m:
        hi, lo = int(m.group(1)), int(m.group(2))
        return ((1 << (hi - lo + 1)) - 1) << lo
    m = re.fullmatch(r"-?(0[xX][0-9A-Fa-f]+|\d+)", tok)
    if m:
        return int(tok, 0)
    raise GenError(f"cannot evaluate initialiser {tok!r}")


def split_items(body: str) -> list[str]:
    """Top-level comma split (GENMASK(a, b) keeps its comma)."""
    items, depth, cur = [], 0, []
    for ch in body:
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        if ch == "," and depth == 0:
            items.append("".join(cur))
            cur = []
        else:
            cur.append(ch)
    items.append("".join(cur))
    return [i.strip() for i in items if i.strip()]


def array_body(src: str, name: str) -> str:
    m = re.search(r"static const [\w ]+\b" + re.escape(name) + r"\[\]\s*=\s*\{(.*?)\n\};", src, re.S)
    if not m:
        raise GenError(f"array {name} not found")
    return m.group(1)


def parse(src: str, name: str, kind: str) -> list:
    body = array_body(src, name)
    if kind in ("pairs", "triples"):
        words = [eval_word(t) & 0xFFFFFFFF for t in split_items(body)]
        width = 2 if kind == "pairs" else 3
        if len(words) % width:
            raise GenError(f"{name}: {len(words)} words is not a multiple of {width}")
        return words
    rows = []
    for row in re.findall(r"\{([^{}]*)\}", body):
        vals = [eval_word(t) for t in split_items(row)]
        if len(vals) != 6:
            raise GenError(f"{name}: row {row!r} has {len(vals)} fields")
        if kind == "lmt":
            if not all(0 <= v <= 255 for v in vals[:5]) or not -128 <= vals[5] <= 127:
                raise GenError(f"{name}: row {row!r} out of range")
            vals[5] &= 0xFF
        rows.append(vals)
    return rows


def check_decl(src: str, decl: str) -> None:
    norm = lambda s: re.sub(r"\s+", "", s)
    if norm(decl) + ";" not in norm(src):
        raise GenError(f"upstream no longer declares `{decl}`")


def fmt_words(words: list[int], per_line: int) -> list[str]:
    out = []
    for i in range(0, len(words), per_line):
        out.append("    " + ", ".join(f"{w:#x}" for w in words[i:i + per_line]) + ",")
    return out


def render_table(rust: str, upstream: str, kind: str, data: list) -> list[str]:
    lines = [f"/// Upstream `{upstream}`. {DOCS[kind]}", "#[rustfmt::skip]"]
    if kind == "pairs":
        lines.append(f"pub static {rust}: &[u32] = &[")
        lines += fmt_words(data, 2 * PAIRS_PER_LINE)
    elif kind == "triples":
        lines.append(f"pub static {rust}: &[[u32; 3]] = &[")
        for i in range(0, len(data), 3 * TRIPLES_PER_LINE):
            chunk = data[i:i + 3 * TRIPLES_PER_LINE]
            rows = [f"[{a:#x}, {m:#x}, {d:#x}]" for a, m, d in zip(chunk[0::3], chunk[1::3], chunk[2::3])]
            lines.append("    " + ", ".join(rows) + ",")
    else:
        ty = "u32" if kind == "pg" else "u8"
        lines.append(f"pub static {rust}: &[[{ty}; 6]] = &[")
        fmt = (lambda v: f"{v:#x}") if kind == "pg" else str
        for i in range(0, len(data), ROWS_PER_LINE):
            rows = ["[" + ", ".join(fmt(v) for v in r) + "]" for r in data[i:i + ROWS_PER_LINE]]
            lines.append("    " + ", ".join(rows) + ",")
    lines.append("];")
    return lines


def generate(src: str) -> dict[str, str]:
    digest = hashlib.sha256(src.encode("utf-8")).hexdigest()
    clean = strip_comments(src)
    files = {}
    for fname, tables in TABLES.items():
        out = [
            "// @generated by tools/wifi/gen_rtw88_tables.py: do not edit.",
            "// From Linux drivers/net/wireless/realtek/rtw88/rtw8822c_table.c",
            f"// (torvalds/linux {COMMIT},",
            f"// sha256 {digest}),",
            "// GPL-2.0 OR BSD-3-Clause, taken under BSD-3-Clause.",
            "",
        ]
        for rust, upstream, kind, decl in tables:
            check_decl(clean, decl)
            out += render_table(rust, upstream, kind, parse(clean, upstream, kind))
            out.append("")
        files[fname] = "\n".join(out[:-1]) + "\n"
    return files


def read_source(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def fetch(dest: Path) -> Path:
    dest.mkdir(parents=True, exist_ok=True)
    target = dest / "rtw8822c_table.c"
    with urllib.request.urlopen(URL, timeout=60) as resp:
        target.write_bytes(resp.read())
    return target


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--src", type=Path, help="upstream rtw8822c_table.c")
    ap.add_argument("--fetch", type=Path, metavar="DIR",
                    help=f"download the file at {COMMIT[:12]} into DIR and use it")
    ap.add_argument("--check", action="store_true", help="fail if the generated files are stale")
    ap.add_argument("--out", type=Path, default=OUT_DIR, help=argparse.SUPPRESS)
    args = ap.parse_args(argv)

    src_path = args.src or (Path(os.environ["RTW88_TABLE_SRC"]) if os.environ.get("RTW88_TABLE_SRC") else None)
    if args.fetch:
        src_path = fetch(args.fetch)
    if src_path is None or not src_path.is_file():
        if args.check:
            print("gen_rtw88_tables: no upstream rtw8822c_table.c (--src, RTW88_TABLE_SRC or --fetch); check skipped")
            return 0
        print("gen_rtw88_tables: need --src PATH or --fetch DIR", file=sys.stderr)
        return 2

    try:
        files = generate(read_source(src_path))
    except GenError as e:
        print(f"gen_rtw88_tables: {e}", file=sys.stderr)
        return 1

    if args.check:
        stale = [n for n, text in files.items()
                 if not (args.out / n).is_file() or (args.out / n).read_text(encoding="utf-8") != text]
        for n in stale:
            print(f"gen_rtw88_tables: {args.out / n} is stale; rerun without --check", file=sys.stderr)
        return 1 if stale else 0

    args.out.mkdir(parents=True, exist_ok=True)
    for name, text in files.items():
        (args.out / name).write_text(text, encoding="utf-8", newline="\n")
        print(f"wrote {args.out / name} ({len(text)} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
