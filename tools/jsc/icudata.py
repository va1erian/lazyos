"""Which of ICU 74.2's data a browser engine on LazyOS keeps (``icu.py`` applies it).

ICU's release tarball carries the data prebuilt (``source/data/in/icudt74l.dat``, 30.8
MB; the sources for CLDR's locales are a separate download), so ICU's own data filter
(``ICU_DATA_FILTER_FILE``, a ``rules.mk`` generated from the sources) is not available.
``icupkg`` can still remove items from that package, and that is all this does: it
lists the package and removes items by the policy below, before ``pkgdata`` turns the
package into the assembly object ``libicudata.a`` carries. LazyOS loads the static ELF of
every WebKit process whole into RAM (docs/webkit-port-plan.md section 6), so the data is
paid once per view: 30.8 MB of the 107 MiB of ``WebProcess``.

What stays (everything WebKit, JavaScriptCore, HarfBuzz, libxml2, libpsl and the C++
programs of this repository call into):

* all of the code-driven data: normalization (``nfkc*.nrm``, ``uts46.nrm``), IDNA and
  stringprep (``*.spp``), ``cnvalias.icu``, ``uemoji.icu``, ``ulayout.icu``, the spoof
  check's ``confusables.cfu``, the zone, plural, numbering-system, unit, calendar, metadata
  and likely-subtags resources, the root collation (``coll/root.res``, ``ucadata.icu``), and
  every break iterator rule file (``brkitr/*.brk``, ``brkitr/*.res``) with Thai's dictionary;
* ``root`` and the locale data of ``KEEP_LANGUAGES`` (every region and script variant of
  them): the locale itself (calendars, number and date patterns, list and relative-time
  words), collation tailorings, currency names and symbols and time zone names; and for
  ``en`` alone (the ``DISPLAY_LANGUAGES``) the rarely used display names: language, region
  and unit names, and the number spell-out rules (``rbnf``);
* the converters ICU is asked for by name: ``TextCodecICU``'s registered encodings and the
  four ``EncodingTables.cpp`` builds its CJK tables from (``KEEP_CONVERTERS``), plus the
  single-byte Windows, ISO-8859, KOI8, IBM866 and Mac code pages libxml2 can meet.

What goes, and what that costs a page:

* the locale data of every other language (about 450 of CLDR's 500): ``Intl``,
  ``toLocaleString`` and form controls in such a locale format and sort like ``en``/root
  (ICU falls back through the default locale to root; nothing fails). ``res_index.res``
  is left as it was, so ``Intl.*.supportedLocalesOf`` still names the full CLDR set;
* the language, region and unit names of every language but English: ``Intl.DisplayNames``
  and ``Intl.NumberFormat`` with ``style: "unit"`` print the English names in any locale;
* the dictionaries for Chinese/Japanese (``cjdict``, 2 MB), Khmer, Lao and Burmese, replaced
  by one-word stand-ins (``brkitr/root.res`` names them, and a package that names a missing
  item is refused): word segmentation of such text (double-click selection,
  ``Intl.Segmenter`` words, find whole word) breaks per character or per rule run instead of
  per word; line breaking of CJK text does not use them (it breaks between ideographs by
  rule); Khmer, Lao and Burmese lines break at spaces only;
* the other 150 converters (EBCDIC and IBM code pages, EUC-TW, Big5-HKSCS, ISO-2022
  compound text, the CNS/JIS plane tables): a page declaring such a charset is decoded as
  an unsupported one. None is in the Encoding Standard WebKit implements;
* transliteration (``translit/*``, used by ``Transliterator`` only), the spell-out rules of
  other locales (``rbnf``), and ``unames.icu`` (character names: ``u_charName``, the
  ``\\N{...}`` UnicodeSet escape).

``plan`` is pure (the listing in, the verdicts out) so it is tested without ICU
(``test_icudata.py``); ``trim`` runs ``icupkg`` and ``check`` proves the result with a C
program against the host build's libraries (``icudata_check.c``).
"""

from __future__ import annotations

import re
import shutil
import subprocess
from pathlib import Path

#: Language subtags (ISO 639) whose locale data is kept: root, the web's most used languages
#: by share of sites, and their legacy codes. Every region and script of a language stays.
KEEP_LANGUAGES = frozenset("""
en es ru tr fa fr de ja pt zh yue it nl pl id ar cs vi ko uk he hu ro sv th el da fi sk bg
nb nn no hr sr bs lt sl ca hi bn et lv ms fil iw in tl
""".split())

#: The converters kept, as ICU names them (``.cnv`` files): what ``ucnv_open`` resolves for
#: ``TextCodecICU``'s names (ISO-8859-2/4/5/10/13/14/15, KOI8-R, macintosh, windows-1250/1251/1254/
#: 1256/1258, x-mac-cyrillic/greek/centraleurroman/turkish), ``EncodingTables.cpp``
#: (EUC-JP, Big-5, windows-949, gb18030) and the other single-byte code pages of the Encoding
#: Standard and the CJK ones of its legacy encodings (Shift_JIS, EUC-KR, GBK).
KEEP_CONVERTERS = frozenset("""
ibm-912_P100-1995 ibm-913_P100-2000 ibm-914_P100-1995 ibm-915_P100-1995 ibm-920_P100-1995
ibm-921_P100-1995 ibm-923_P100-1998 ibm-1089_P100-1995 iso-8859_10-1998 iso-8859_11-2001
iso-8859_14-1998 ibm-9005_X110-2007 ibm-5012_P100-1999 ibm-878_P100-1996 ibm-1168_P100-2002
ibm-866_P100-1995 ibm-5346_P100-1998 ibm-5347_P100-1998 ibm-5348_P100-1997 ibm-5349_P100-1998
ibm-5350_P100-1998 ibm-9447_P100-2002 ibm-9448_X100-2005 ibm-9449_P100-2002 ibm-5354_P100-1998
windows-874-2000 macos-0_2-10.2 macos-7_3-10.2 macos-6_2-10.4 macos-29-10.2 macos-35-10.2
euc-jp-2007 ibm-943_P15A-2003 ibm-970_P110_P110-2006_U2 windows-936-2000 windows-949-2000
windows-950-2000 gb18030-2022
ibm-1386_P100-2001 ibm-1363_P11B-1998 ibm-1373_P100-2002
""".split())

#: Items outside these kinds are kept; these are the ones with a policy.
LOCALE_ITEM = re.compile(r"^([a-z]{2,3})(?:_[A-Za-z0-9]+)*$")
#: Per-locale trees: a locale file is kept when its language is.
LOCALE_TREES = ("coll", "curr", "zone")
#: Language, region and unit names: per-locale trees of rarely used APIs, kept for these languages.
#: (Time zone names stay for every kept language: a long or short time style prints them.)
DISPLAY_TREES = ("lang", "region", "unit")
DISPLAY_LANGUAGES = frozenset({"en"})
#: Dictionaries kept (Thai is the one a browser meets outside CJK; it is 126 KB).
KEEP_DICTIONARIES = frozenset({"thaidict.dict"})
#: Always kept in the locale trees and at the top, whatever the language.
STRUCTURAL = frozenset({"root", "pool", "res_index"})
#: The spell-out rules kept: the root's (numbering systems that are algorithmic) and English's.
KEEP_RBNF = frozenset({"root", "en", "res_index"})
#: Single items dropped by name.
DROP_ITEMS = frozenset({"unames.icu"})


def is_locale(stem: str) -> bool:
    """Whether a file stem names a locale (``fr_CA``, ``zh_Hant_TW``), not a resource (``res_index``)."""
    return stem not in STRUCTURAL and LOCALE_ITEM.match(stem) is not None


def language_of(stem: str) -> str:
    return stem.split("_", 1)[0]


def wanted_locale(stem: str, languages: frozenset[str] = KEEP_LANGUAGES) -> bool:
    return not is_locale(stem) or language_of(stem) in languages


def verdict(item: str) -> bool:
    """True when `item` (a path in the package: ``coll/zh.res``, ``ja.res``) stays."""
    if item in DROP_ITEMS:
        return False
    tree, _, name = item.rpartition("/")
    stem, _, extension = name.partition(".")
    if not tree:
        if extension == "cnv":
            return stem in KEEP_CONVERTERS
        return extension != "res" or wanted_locale(stem)
    if tree in LOCALE_TREES:
        return extension != "res" or wanted_locale(stem)
    if tree in DISPLAY_TREES:
        return extension != "res" or wanted_locale(stem, DISPLAY_LANGUAGES)
    if tree == "brkitr":
        return extension != "dict" or name in KEEP_DICTIONARIES
    if tree == "rbnf":
        return stem in KEEP_RBNF
    if tree == "translit":
        return False
    return True


def plan(items: list[str]) -> tuple[list[str], list[str]]:
    """(kept, dropped) of the package's items, each in the listing's order."""
    kept = [item for item in items if verdict(item)]
    dropped = [item for item in items if not verdict(item)]
    return kept, dropped


def listing(icupkg: Path, package: Path) -> list[str]:
    done = subprocess.run([str(icupkg), "--auto_toc_prefix", "-l", str(package)], capture_output=True, text=True,
                          check=True)
    return [line.strip() for line in done.stdout.splitlines() if line.strip()]


def trim(icupkg: Path, source: Path, out: Path, work: Path) -> dict[str, int]:
    """Write `out`, `source` without the items `plan` drops; the counts and sizes."""
    items = listing(icupkg, source)
    kept, dropped = plan(items)
    work.mkdir(parents=True, exist_ok=True)
    added = standin_dictionaries(icupkg.with_name("gendict"), dropped, work)
    drop = work / "icudata-drop.txt"
    drop.write_text("\n".join(dropped) + "\n", encoding="utf-8")
    command = [str(icupkg), "--auto_toc_prefix", "-r", str(drop)]
    if added:
        add = work / "icudata-add.txt"
        add.write_text("\n".join(added) + "\n", encoding="utf-8")
        command += ["-s", str(work / "standins"), "-a", str(add)]
    log = work / "icudata-trim.log"
    # `--auto_toc_prefix`: the table of contents names its items `icudt74l/...` whatever the output file is
    # called (by default the prefix is taken from the output's name). icupkg removes, then adds, then checks
    # that no item names a missing one: that is how it reported the converters that extend others
    # (`KEEP_CONVERTERS`) and the dictionaries `brkitr/root.res` names (`standin_dictionaries`).
    with log.open("wb") as sink:
        done = subprocess.run([*command, str(source), str(out)], stdout=sink, stderr=subprocess.STDOUT)
    if done.returncode != 0:
        raise RuntimeError(f"icupkg failed removing {len(dropped)} items ({done.returncode}):\n"
                           + log.read_text(errors="replace")[-3000:])
    return {"items": len(items), "kept": len(kept) + len(added), "dropped": len(dropped) - len(added),
            "before": source.stat().st_size, "after": out.stat().st_size}


def standin_dictionaries(gendict: Path, dropped: list[str], work: Path) -> list[str]:
    """Write a valid dictionary with one word for each dropped one, under ``work/standins``; their names.

    ``brkitr/root.res`` names the dictionary of every script, and icupkg (and the ``make`` that unpacks the
    package again) refuse a package whose resource names an item it lacks. A dictionary that knows one
    word segments like none: ICU breaks the text by rule, as it does for a script it has no dictionary for.
    """
    names = [item for item in dropped if item.endswith(".dict")]
    if not names:
        return []
    root = work / "standins"
    shutil.rmtree(root, ignore_errors=True)
    (root / "brkitr").mkdir(parents=True)
    words = work / "standin-words.txt"
    words.write_text("a\n", encoding="utf-8")
    template = work / "standin.dict"
    done = subprocess.run([str(gendict), "--uchars", str(words), str(template)], capture_output=True, text=True)
    if done.returncode != 0:
        raise RuntimeError("gendict failed:\n" + done.stdout[-1500:] + done.stderr[-1500:])
    for name in names:
        shutil.copyfile(template, root / name)
    return names


def _run(program: Path, package: Path | None) -> list[str]:
    command = [str(program)] + ([str(package)] if package else [])
    done = subprocess.run(command, capture_output=True, text=True)
    if done.returncode != 0 or "ICUDATA:CHECK:PASS" not in done.stdout:
        raise RuntimeError(f"icudata_check {package or 'on the full data'} failed ({done.returncode}):\n"
                           + done.stdout[-3000:] + done.stderr[-1000:])
    return done.stdout.splitlines()


def check(host_build: Path, source_tree: Path, package: Path, work: Path) -> int:
    """Compile ``icudata_check.c`` against the host build and run it twice, once on the library's
    own (full) data and once with `package` as the common data. Every ``SAME`` line must match,
    and what the cut removed must really be gone; the number of lines compared."""
    common = source_tree / "source" / "common"
    i18n = source_tree / "source" / "i18n"
    flags = [f"-I{host_build / 'common'}", f"-I{common}", f"-I{i18n}", f"-L{host_build / 'lib'}"]
    libs = ["-licui18n", "-licuuc", "-lpthread", "-ldl", "-lstdc++", "-lm"]
    source = str(Path(__file__).with_name("icudata_check.c"))
    # `udata_setCommonData` adds a package in front of the library's own data and ICU falls through to
    # it for any item the package lacks, which would hide the cut: the second program links ICU's
    # empty stub data instead, so the package is all there is.
    stub = work / "stubdata.o"
    programs = {"full": ["-licudata"], "stub": [str(stub)]}
    for command in (["g++", "-c", "-o", str(stub), str(source_tree / "source" / "stubdata" / "stubdata.cpp"), *flags],
                    *(["gcc", "-O1", "-o", str(work / f"icudata_check_{name}"), source, *flags, *libs, *extra]
                      for name, extra in programs.items())):
        built = subprocess.run(command, capture_output=True, text=True)
        if built.returncode != 0:
            raise RuntimeError(f"{' '.join(command[:3])} failed:\n" + built.stderr[-3000:])
    full = [line for line in _run(work / "icudata_check_full", None) if line.startswith("SAME ")]
    cut_lines = _run(work / "icudata_check_stub", package)
    cut = [line for line in cut_lines if line.startswith("SAME ")]
    if full != cut:
        different = [f"{a}  !=  {b}" for a, b in zip(full, cut) if a != b][:20]
        raise RuntimeError(f"the trimmed ICU data changes {len(different)}+ results of kept features:\n"
                           + "\n".join(different))
    for must_fail in ("SOFT cnv-dropped.EUC-TW ok", "SOFT cnv-dropped.ibm-37 ok", "SOFT charname ok"):
        if must_fail in cut_lines:
            raise RuntimeError(f"{must_fail!r}: the data the cut removes is still there (does the package win?)")
    return len(cut)


def signature() -> str:
    """Changes whenever the policy does (not when a comment does), so a build made with another one is redone."""
    import hashlib
    digest = hashlib.sha256()
    for part in (sorted(KEEP_LANGUAGES), sorted(KEEP_CONVERTERS), sorted(KEEP_DICTIONARIES), sorted(KEEP_RBNF),
                 sorted(DROP_ITEMS), LOCALE_TREES, DISPLAY_TREES, sorted(DISPLAY_LANGUAGES), "v1"):
        digest.update(repr(part).encode())
    return digest.hexdigest()[:16]


def keep_copy(path: Path) -> Path:
    """The pristine package next to `path` (made once), what every trim starts from."""
    pristine = path.with_name(path.stem + "-full.dat")  # icupkg wants the .dat ending
    if not pristine.is_file():
        shutil.copy2(path, pristine)
    return pristine
