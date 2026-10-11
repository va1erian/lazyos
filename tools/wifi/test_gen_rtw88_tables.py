#!/usr/bin/env python3
"""Tests for gen_rtw88_tables.py on a small synthetic rtw8822c_table.c.

    python tools/wifi/test_gen_rtw88_tables.py
"""
from __future__ import annotations

import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen_rtw88_tables as gen  # noqa: E402


def pairs(name: str, body: str, decl: str) -> str:
    return f"static const u32 {name}[] = {{\n{body}\n}};\n\n{decl};\n\n"


def synthetic() -> str:
    src = "// SPDX-License-Identifier: GPL-2.0 OR BSD-3-Clause\n/* header */\n"
    src += pairs("rtw8822c_mac", "", "RTW_DECL_TABLE_PHY_COND(rtw8822c_mac, rtw_phy_cfg_mac)")
    src += pairs("rtw8822c_agc", "\t0x83000000,\t0x00000000,\t0x40000000,\t0x00000000,\n\t\t0x1D90, 0x300001FF,\n"
                 "\t0xB0000000,\t0x00000000,",
                 "RTW_DECL_TABLE_PHY_COND(rtw8822c_agc, rtw_phy_cfg_agc)")
    src += pairs("rtw8822c_bb", "\t\t0x800, 0x00000000, /* a comment */\n\t\t0x0FE, 0x0,",
                 "RTW_DECL_TABLE_PHY_COND(rtw8822c_bb, rtw_phy_cfg_bb)")
    src += ("static const struct rtw_phy_pg_cfg_pair rtw8822c_bb_pg_type0[] = {\n"
            "\t{ 0, 0, 0, 0x00000c20, 0xffffffff, 0x484c5054, },\n"
            "\t{ 1, 1, 1, 0x00000e4c, 0xffffffff, 0x383c4044, },\n};\n\n"
            "RTW_DECL_TABLE_BB_PG(rtw8822c_bb_pg_type0);\n\n")
    src += pairs("rtw8822c_rf_a", "\t\t0x000, 0x00030000,\n\t\t0x018, 0x00013124,",
                 "RTW_DECL_TABLE_RF_RADIO(rtw8822c_rf_a, A)")
    src += pairs("rtw8822c_rf_b", "\t\t0x0DE, 0x00000020,", "RTW_DECL_TABLE_RF_RADIO(rtw8822c_rf_b, B)")
    for t in ("0", "5"):
        src += (f"static const struct rtw_txpwr_lmt_cfg_pair rtw8822c_txpwr_lmt_type{t}[] = {{\n"
                "\t{ 0, 0, 0, 0, 1, 72, },\n\t{ 12, 1, 2, 4, 42, -8, },\n};\n\n"
                f"RTW_DECL_TABLE_TXPWR_LMT(rtw8822c_txpwr_lmt_type{t});\n\n")
    src += ("static const u32 rtw8822c_dpk_afe_no_dpk[] = {\n\t0x18a4, BIT(7), 0,\n\t0x1c38, MASKDWORD, 0xffa1005e,\n};\n\n"
            "RTW_DECL_TABLE_DPK(rtw8822c_dpk_afe_no_dpk);\n\n")
    src += ("static const u32 rtw8822c_dpk_afe_is_dpk[] = {\n\t0x18a4, BIT(7), 1,\n};\n\n"
            "RTW_DECL_TABLE_DPK(rtw8822c_dpk_afe_is_dpk);\n\n")
    src += ("static const u32 rtw8822c_dpk_mac_bb[] = {\n\t0x1d58, GENMASK(11, 3), 0x1ff,\n};\n\n"
            "RTW_DECL_TABLE_DPK(rtw8822c_dpk_mac_bb);\n\n")
    src += pairs("rtw8822c_array_mp_cal_init", "\t0x1b00, 0x00000008,",
                 "RTW_DECL_TABLE_PHY_COND(rtw8822c_array_mp_cal_init, rtw_phy_cfg_bb)")
    return src


class Generate(unittest.TestCase):
    def test_tables_render_as_upstream_words(self):
        files = gen.generate(synthetic())
        self.assertEqual(sorted(files), sorted(gen.TABLES))
        for text in files.values():
            self.assertIn("@generated", "\n".join(text.splitlines()[:5]))
        bb = files["gen_bb.rs"]
        self.assertIn("pub static MAC: &[u32] = &[\n];", bb)
        self.assertIn("0x83000000, 0x0, 0x40000000, 0x0, 0x1d90, 0x300001ff, 0xb0000000, 0x0,", bb)
        self.assertIn("0x800, 0x0, 0xfe, 0x0,", bb)
        dpk = files["gen_dpk.rs"]
        self.assertIn("[0x18a4, 0x80, 0x0], [0x1c38, 0xffffffff, 0xffa1005e],", dpk)
        self.assertIn("[0x1d58, 0xff8, 0x1ff],", dpk)
        pw = files["gen_txpwr.rs"]
        self.assertIn("[0x1, 0x1, 0x1, 0xe4c, 0xffffffff, 0x383c4044]", pw)
        self.assertIn("[0, 0, 0, 0, 1, 72], [12, 1, 2, 4, 42, 248]", pw, "negative limits stored as u8")

    def test_changed_declaration_is_refused(self):
        src = synthetic().replace("RTW_DECL_TABLE_RF_RADIO(rtw8822c_rf_a, A)", "RTW_DECL_TABLE_RF_RADIO(rtw8822c_rf_a, B)")
        with self.assertRaises(gen.GenError):
            gen.generate(src)

    def test_odd_pair_table_is_refused(self):
        src = synthetic().replace("0x0DE, 0x00000020,", "0x0DE,")
        with self.assertRaises(gen.GenError):
            gen.generate(src)


class Check(unittest.TestCase):
    def run_main(self, *args: str) -> int:
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return gen.main(list(args))

    def test_write_then_check_then_stale(self):
        with tempfile.TemporaryDirectory() as d:
            src = Path(d) / "rtw8822c_table.c"
            src.write_text(synthetic(), encoding="utf-8")
            out = Path(d) / "out"
            self.assertEqual(self.run_main("--src", str(src), "--out", str(out)), 0)
            self.assertEqual(self.run_main("--check", "--src", str(src), "--out", str(out)), 0)
            (out / "gen_dpk.rs").write_text("// stale\n", encoding="utf-8")
            self.assertEqual(self.run_main("--check", "--src", str(src), "--out", str(out)), 1)

    def test_check_without_source_is_skipped(self):
        old = os.environ.pop("RTW88_TABLE_SRC", None)
        try:
            self.assertEqual(self.run_main("--check", "--src", "/nonexistent/rtw8822c_table.c"), 0)
            self.assertEqual(self.run_main("--check"), 0)
            self.assertEqual(self.run_main(), 2, "generating needs a source")
        finally:
            if old is not None:
                os.environ["RTW88_TABLE_SRC"] = old


if __name__ == "__main__":
    unittest.main()
