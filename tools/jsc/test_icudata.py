"""Tests for the ICU data policy (``icudata.py``) that need no ICU: the verdicts on item names."""

from __future__ import annotations

import unittest

import icudata


class VerdictTests(unittest.TestCase):
    def test_structural_resources_stay(self) -> None:
        for item in ("root.res", "pool.res", "res_index.res", "coll/root.res", "coll/res_index.res",
                     "zone/pool.res", "curr/pool.res", "lang/pool.res", "region/root.res", "unit/root.res",
                     "supplementalData.res", "plurals.res", "zoneinfo64.res", "metaZones.res",
                     "numberingSystems.res", "keyTypeData.res", "icustd.res", "langInfo.res"):
            self.assertTrue(icudata.verdict(item), item)

    def test_res_index_is_not_a_locale(self) -> None:
        # "res_index" has the shape of a locale id (three letters, an underscore, a word).
        self.assertFalse(icudata.is_locale("res_index"))
        self.assertTrue(icudata.is_locale("zh_Hant_TW"))
        self.assertTrue(icudata.is_locale("ff_Adlm"))
        self.assertFalse(icudata.is_locale("supplementalData"))

    def test_kept_languages_keep_every_variant(self) -> None:
        for item in ("en.res", "en_001.res", "en_US_POSIX.res", "de_CH.res", "zh_Hant_HK.res", "sr_Latn.res",
                     "pt_BR.res", "es_419.res", "yue_Hans.res", "coll/sv.res", "coll/zh.res", "curr/fr_CA.res",
                     "zone/ja.res"):
            self.assertTrue(icudata.verdict(item), item)

    def test_other_languages_go(self) -> None:
        for item in ("cy.res", "ta_IN.res", "ff_Adlm.res", "agq_CM.res", "coll/my.res", "curr/zu.res",
                     "zone/sw_KE.res"):
            self.assertFalse(icudata.verdict(item), item)

    def test_display_trees_are_english_only(self) -> None:
        for item in ("lang/en.res", "lang/en_GB.res", "region/en.res", "unit/en_001.res", "lang/pool.res",
                     "unit/root.res"):
            self.assertTrue(icudata.verdict(item), item)
        for item in ("lang/de.res", "region/fr.res", "unit/ja.res"):
            self.assertFalse(icudata.verdict(item), item)

    def test_converters(self) -> None:
        self.assertTrue(icudata.verdict("ibm-5348_P100-1997.cnv"))  # windows-1252
        self.assertTrue(icudata.verdict("gb18030-2022.cnv"))
        self.assertTrue(icudata.verdict("ibm-1363_P11B-1998.cnv"))  # windows-949 extends it
        self.assertFalse(icudata.verdict("euc-tw-2014.cnv"))
        self.assertFalse(icudata.verdict("ibm-37_P100-1995.cnv"))
        self.assertFalse(icudata.verdict("icu-internal-compound-t.cnv"))

    def test_code_driven_data_stays(self) -> None:
        for item in ("nfkc.nrm", "nfkc_cf.nrm", "uts46.nrm", "rfc3491.spp", "cnvalias.icu", "uemoji.icu",
                     "ulayout.icu", "confusables.cfu", "coll/ucadata.icu", "brkitr/word.brk", "brkitr/line.brk",
                     "brkitr/char.brk", "brkitr/sent.brk", "brkitr/root.res", "brkitr/ja.res"):
            self.assertTrue(icudata.verdict(item), item)

    def test_dictionaries(self) -> None:
        self.assertTrue(icudata.verdict("brkitr/thaidict.dict"))
        for item in ("brkitr/cjdict.dict", "brkitr/khmerdict.dict", "brkitr/laodict.dict",
                     "brkitr/burmesedict.dict"):
            self.assertFalse(icudata.verdict(item), item)

    def test_transliteration_rbnf_and_names(self) -> None:
        for item in ("translit/root.res", "translit/en.res", "rbnf/de.res", "unames.icu"):
            self.assertFalse(icudata.verdict(item), item)
        for item in ("rbnf/root.res", "rbnf/en.res", "rbnf/res_index.res"):
            self.assertTrue(icudata.verdict(item), item)

    def test_plan_splits_the_listing(self) -> None:
        kept, dropped = icudata.plan(["en.res", "cy.res", "unames.icu", "root.res"])
        self.assertEqual(kept, ["en.res", "root.res"])
        self.assertEqual(dropped, ["cy.res", "unames.icu"])

    def test_signature_follows_the_policy(self) -> None:
        before = icudata.signature()
        self.assertEqual(before, icudata.signature())
        icudata.KEEP_LANGUAGES = icudata.KEEP_LANGUAGES | {"cy"}
        try:
            self.assertNotEqual(before, icudata.signature())
        finally:
            icudata.KEEP_LANGUAGES = icudata.KEEP_LANGUAGES - {"cy"}


if __name__ == "__main__":
    unittest.main()
