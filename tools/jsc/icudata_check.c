/* Proof that a trimmed ICU data package still does what WebKit and JavaScriptCore ask of ICU.
 *
 *   icudata_check [package.dat]
 *
 * With a package it becomes ICU's common data (udata_setCommonData, before anything else
 * runs); without, the data compiled into the library is used. Either way the output is the
 * same kind of lines, which tools/jsc/icudata.py compares:
 *
 *   SAME <name> <value>   what kept data must produce identically with and without the cut
 *   SOFT <name> <ok|err>  what the cut removed: must not crash; the status is only logged
 *   ICUDATA:CHECK:PASS    every step ran
 *
 * Plain C against ICU's C API (host build, gcc), no output depends on the machine.
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <unicode/ubrk.h>
#include <unicode/ucal.h>
#include <unicode/ucnv.h>
#include <unicode/udata.h>
#include <unicode/ucol.h>
#include <unicode/udat.h>
#include <unicode/uidna.h>
#include <unicode/uldnames.h>
#include <unicode/ulistformatter.h>
#include <unicode/uloc.h>
#include <unicode/unorm2.h>
#include <unicode/unum.h>
#include <unicode/upluralrules.h>
#include <unicode/uchar.h>
#include <unicode/uregex.h>
#include <unicode/ureldatefmt.h>
#include <unicode/uspoof.h>
#include <unicode/ustring.h>
#include <unicode/utypes.h>

static uint64_t fnv(const void* data, size_t length, uint64_t seed) {
    const unsigned char* p = (const unsigned char*)data;
    uint64_t h = seed ? seed : 1469598103934665603ULL;
    for (size_t i = 0; i < length; i++) { h ^= p[i]; h *= 1099511628211ULL; }
    return h;
}

static void same(const char* name, const char* value) { printf("SAME %s %s\n", name, value); }

static void same_hash(const char* name, const void* data, size_t length) {
    char value[40];
    snprintf(value, sizeof value, "%016llx", (unsigned long long)fnv(data, length, 0));
    same(name, value);
}

static void soft(const char* name, UErrorCode status) {
    printf("SOFT %s %s\n", name, U_SUCCESS(status) ? "ok" : u_errorName(status));
}

static void same_ustr(const char* prefix, const char* locale, const UChar* text, int32_t length, UErrorCode status) {
    char name[160];
    snprintf(name, sizeof name, "%s.%s", prefix, locale);
    if (U_FAILURE(status)) { same(name, u_errorName(status)); return; }
    same_hash(name, text, (size_t)length * sizeof(UChar));
}

static const char* const KEPT_LOCALES[] = {"en", "en_US", "en_GB", "de", "fr", "es", "ja", "zh_Hant", "zh_Hans_CN",
                                           "ar", "ru", "tr", "pt_BR", "sv", "th", "ko", "he", "hi", "nb"};
static const char* const DROPPED_LOCALES[] = {"cy", "ta", "mi", "zu", "sw"};
#define COUNT(a) (sizeof(a) / sizeof((a)[0]))

static void converters(void) {
    static const char* const names[] = {
        "ISO-8859-2", "ISO-8859-3", "ISO-8859-4", "ISO-8859-5", "ISO-8859-6", "ISO-8859-7", "ISO-8859-8",
        "ISO-8859-9", "ISO-8859-10", "ISO-8859-11", "ISO-8859-13", "ISO-8859-14", "ISO-8859-15", "KOI8-R", "KOI8-U",
        "IBM866", "macintosh", "x-mac-cyrillic", "x-mac-greek", "x-mac-centraleurroman", "x-mac-turkish",
        "windows-874", "windows-1250", "windows-1251", "windows-1252", "windows-1253", "windows-1254",
        "windows-1255", "windows-1256", "windows-1257", "windows-1258", "EUC-JP", "Big-5", "windows-949",
        "gb18030", "Shift_JIS", "EUC-KR", "GBK", "UTF-8", "UTF-16LE", "UTF-16BE", "UTF-32", "ISO-8859-1", "US-ASCII"};
    /* Every byte, then every lead/trail pair of the double-byte tables, decoded with substitution. */
    unsigned char input[256 + 126 * 191 * 2];
    size_t n = 0;
    for (int b = 0; b < 256; b++) input[n++] = (unsigned char)b;
    for (int lead = 0x81; lead <= 0xFE; lead++)
        for (int trail = 0x40; trail <= 0xFE; trail++) { input[n++] = (unsigned char)lead; input[n++] = (unsigned char)trail; }
    static UChar output[sizeof input + 16];
    for (size_t i = 0; i < COUNT(names); i++) {
        UErrorCode status = U_ZERO_ERROR;
        UConverter* converter = ucnv_open(names[i], &status);
        char name[96];
        snprintf(name, sizeof name, "cnv.%s", names[i]);
        if (U_FAILURE(status)) { same(name, u_errorName(status)); continue; }
        int32_t length = ucnv_toUChars(converter, output, (int32_t)(sizeof output / sizeof(UChar)), (const char*)input, (int32_t)n, &status);
        ucnv_close(converter);
        same_ustr("cnv", names[i], output, length, status);
    }
    static const char* const gone[] = {"EUC-TW", "ibm-37", "ibm-1390", "ISO_2022,locale=zh,version=1", "Big5-HKSCS"};
    for (size_t i = 0; i < COUNT(gone); i++) {
        UErrorCode status = U_ZERO_ERROR;
        UConverter* converter = ucnv_open(gone[i], &status);
        char name[96];
        snprintf(name, sizeof name, "cnv-dropped.%s", gone[i]);
        soft(name, status);
        if (U_SUCCESS(status)) ucnv_close(converter);
    }
}

static void numbers_and_dates(const char* const* locales, size_t count, int strict) {
    static const UChar zone[] = {'A', 'm', 'e', 'r', 'i', 'c', 'a', '/', 'N', 'e', 'w', '_', 'Y', 'o', 'r', 'k', 0};
    for (size_t i = 0; i < count; i++) {
        const char* locale = locales[i];
        UChar text[400];
        UErrorCode status = U_ZERO_ERROR;
        char name[96];
        UNumberFormat* number = unum_open(UNUM_DECIMAL, NULL, 0, locale, NULL, &status);
        int32_t length = U_SUCCESS(status) ? unum_formatDouble(number, 1234567.891, text, 400, NULL, &status) : 0;
        if (strict) same_ustr("unum", locale, text, length, status); else { snprintf(name, sizeof name, "unum.%s", locale); soft(name, status); }
        if (number) unum_close(number);

        status = U_ZERO_ERROR;
        UNumberFormat* money = unum_open(UNUM_CURRENCY, NULL, 0, locale, NULL, &status);
        if (U_SUCCESS(status)) {
            static const UChar eur[] = {'E', 'U', 'R', 0};
            unum_setTextAttribute(money, UNUM_CURRENCY_CODE, eur, 3, &status);
            length = unum_formatDouble(money, 1234.5, text, 400, NULL, &status);
        }
        if (strict) same_ustr("currency", locale, text, length, status); else { snprintf(name, sizeof name, "currency.%s", locale); soft(name, status); }
        if (money) unum_close(money);

        status = U_ZERO_ERROR;
        UDateFormat* date = udat_open(UDAT_LONG, UDAT_LONG, locale, zone, -1, NULL, 0, &status);
        length = U_SUCCESS(status) ? udat_format(date, 1700000000000.0, text, 400, NULL, &status) : 0;
        if (strict) same_ustr("udat", locale, text, length, status); else { snprintf(name, sizeof name, "udat.%s", locale); soft(name, status); }
        if (date) udat_close(date);

        status = U_ZERO_ERROR;
        UCalendar* calendar = ucal_open(zone, -1, locale, UCAL_DEFAULT, &status);
        length = U_SUCCESS(status) ? ucal_getTimeZoneDisplayName(calendar, UCAL_STANDARD, locale, text, 400, &status) : 0;
        if (strict) same_ustr("zonename", locale, text, length, status); else { snprintf(name, sizeof name, "zonename.%s", locale); soft(name, status); }
        if (calendar) ucal_close(calendar);

        status = U_ZERO_ERROR;
        UPluralRules* plural = uplrules_open(locale, &status);
        if (U_SUCCESS(status)) {
            length = 0;
            const double samples[] = {0, 1, 2, 3, 5.5, 11, 21, 100};
            for (size_t s = 0; s < COUNT(samples); s++) length += uplrules_select(plural, samples[s], text + length, 400 - length, &status);
        }
        if (strict) same_ustr("plural", locale, text, length, status); else { snprintf(name, sizeof name, "plural.%s", locale); soft(name, status); }
        if (plural) uplrules_close(plural);

        status = U_ZERO_ERROR;
        UListFormatter* list = ulistfmt_openForType(locale, ULISTFMT_TYPE_AND, ULISTFMT_WIDTH_WIDE, &status);
        if (U_SUCCESS(status)) {
            static const UChar a[] = {'a', 0}, b[] = {'b', 0}, c[] = {'c', 0};
            const UChar* strings[] = {a, b, c};
            int32_t lengths[] = {1, 1, 1};
            length = ulistfmt_format(list, strings, lengths, 3, text, 400, &status);
        }
        if (strict) same_ustr("list", locale, text, length, status); else { snprintf(name, sizeof name, "list.%s", locale); soft(name, status); }
        if (list) ulistfmt_close(list);

        status = U_ZERO_ERROR;
        URelativeDateTimeFormatter* relative = ureldatefmt_open(locale, NULL, UDAT_STYLE_LONG, UDISPCTX_CAPITALIZATION_NONE, &status);
        length = U_SUCCESS(status) ? ureldatefmt_formatNumeric(relative, -1.0, UDAT_REL_UNIT_DAY, text, 400, &status) : 0;
        if (strict) same_ustr("reldate", locale, text, length, status); else { snprintf(name, sizeof name, "reldate.%s", locale); soft(name, status); }
        if (relative) ureldatefmt_close(relative);

        status = U_ZERO_ERROR;
        ULocaleDisplayNames* names = uldn_open(locale, ULDN_STANDARD_NAMES, &status);
        if (U_SUCCESS(status)) {
            length = uldn_localeDisplayName(names, "zh_Hant_TW", text, 200, &status);
            length += uldn_regionDisplayName(names, "JP", text + length, 100, &status);
            length += uldn_languageDisplayName(names, "fr", text + length, 100, &status);
        }
        /* Display names are kept for English only (icudata.py DISPLAY_LANGUAGES). */
        if (strict && strncmp(locale, "en", 2) == 0) same_ustr("displaynames", locale, text, length, status); else { snprintf(name, sizeof name, "displaynames.%s", locale); soft(name, status); }
        if (names) uldn_close(names);
    }
}

static void collation(const char* const* locales, size_t count, int strict) {
    static const UChar words[][8] = {{'z', 0}, {0xF6, 0}, {'o', 0}, {'a', 0}, {0xE5, 0}, {0xE4, 0}, {'C', 0}, {'c', 0}, {0x10D, 0}, {'Z', 0}};
    for (size_t i = 0; i < count; i++) {
        UErrorCode status = U_ZERO_ERROR;
        UCollator* collator = ucol_open(locales[i], &status);
        char name[96];
        snprintf(name, sizeof name, "coll.%s", locales[i]);
        if (U_FAILURE(status)) { if (strict) same(name, u_errorName(status)); else soft(name, status); continue; }
        uint64_t digest = 0;
        for (size_t w = 0; w < COUNT(words); w++) {
            uint8_t key[64];
            int32_t length = ucol_getSortKey(collator, words[w], -1, key, 64);
            digest = fnv(key, (size_t)length, digest);
        }
        if (strict) { char value[40]; snprintf(value, sizeof value, "%016llx", (unsigned long long)digest); same(name, value); }
        else soft(name, status);
        ucol_close(collator);
    }
}

static int32_t utf16(const char* utf8, UChar* out, int32_t capacity) {
    UErrorCode status = U_ZERO_ERROR;
    int32_t length = 0;
    u_strFromUTF8(out, capacity, &length, utf8, -1, &status);
    return U_SUCCESS(status) ? length : 0;
}

static void breaks(const char* label, UBreakIteratorType type, const char* locale, const char* utf8, int strict) {
    UChar text[512];
    int32_t length = utf16(utf8, text, 512);
    UErrorCode status = U_ZERO_ERROR;
    UBreakIterator* iterator = ubrk_open(type, locale, text, length, &status);
    char name[160];
    snprintf(name, sizeof name, "brk.%s.%d.%s", label, (int)type, locale);
    if (U_FAILURE(status)) { if (strict) same(name, u_errorName(status)); else soft(name, status); return; }
    int32_t boundaries[512];
    int count = 0;
    for (int32_t b = ubrk_first(iterator); b != UBRK_DONE && count < 512; b = ubrk_next(iterator)) boundaries[count++] = b;
    ubrk_close(iterator);
    if (strict) same_hash(name, boundaries, (size_t)count * sizeof(int32_t));
    else { printf("SOFT %s ok(%d breaks)\n", name, count); }
}

static void segmentation(void) {
    static const UBreakIteratorType kinds[] = {UBRK_CHARACTER, UBRK_WORD, UBRK_LINE, UBRK_SENTENCE};
    static const struct { const char* label; const char* locale; const char* text; } kept[] = {
        {"english", "en", "Hello, world! It's 3.14 o'clock; e-mail me at a.b@c.org. Mr. Smith left?"},
        {"german", "de", "Donaudampfschifffahrtsgesellschaft, bitte warten Sie."},
        {"thai", "th", "\xe0\xb8\xaa\xe0\xb8\xa7\xe0\xb8\xb1\xe0\xb8\xaa\xe0\xb8\x94\xe0\xb8\xb5\xe0\xb8\x84\xe0\xb8\xa3\xe0\xb8\xb1\xe0\xb8\x9a\xe0\xb8\x9c\xe0\xb8\xa1\xe0\xb8\x8a\xe0\xb8\xb7\xe0\xb9\x88\xe0\xb8\xad\xe0\xb8\xaa\xe0\xb8\xa1\xe0\xb8\x8a\xe0\xb8\xb2\xe0\xb8\xa2"},
        {"emoji", "en", "a\xf0\x9f\x91\xa8\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7 \xf0\x9f\x87\xaf\xf0\x9f\x87\xb5 \xe2\x9d\xa4\xef\xb8\x8f"},
        {"devanagari", "hi", "\xe0\xa4\x95\xe0\xa5\x8d\xe0\xa4\xb7\xe0\xa4\xbf \xe0\xa4\xa8\xe0\xa4\xae\xe0\xa4\xb8\xe0\xa5\x8d\xe0\xa4\xa4\xe0\xa5\x87"},
        {"arabic", "ar", "\xd9\x85\xd8\xb1\xd8\xad\xd8\xa8\xd8\xa7 \xd8\xa8\xd8\xa7\xd9\x84\xd8\xb9\xd8\xa7\xd9\x84\xd9\x85"},
        {"japanese-kanji-line", "ja", "\xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e\xe3\x81\xae\xe6\x96\x87\xe7\xab\xa0\xe3\x80\x82"},
    };
    for (size_t i = 0; i < COUNT(kept); i++)
        for (size_t k = 0; k < COUNT(kinds); k++) {
            /* Japanese words need cjdict, which is cut: only its line and character breaks are exact. */
            int strict = !(kinds[k] == UBRK_WORD && strcmp(kept[i].locale, "ja") == 0);
            breaks(kept[i].label, kinds[k], kept[i].locale, kept[i].text, strict);
        }
    breaks("khmer", UBRK_WORD, "km", "\xe1\x9e\x9f\xe1\x9e\xbd\xe1\x9e\x9f\xe1\x9f\x92\xe1\x9e\x8a\xe1\x9e\xb8\xe1\x9e\x91\xe1\x9f\x86\xe1\x9e\x96\xe1\x9f\x90\xe1\x9e\x9a", 0);
    breaks("lao", UBRK_LINE, "lo", "\xe0\xba\xaa\xe0\xba\xb0\xe0\xba\x9a\xe0\xba\xb2\xe0\xba\xa2\xe0\xba\x94\xe0\xba\xb5", 0);
    breaks("chinese-words", UBRK_WORD, "zh", "\xe6\x88\x91\xe5\xaf\xb9\xe4\xb8\xad\xe6\x96\x87\xe5\x88\x86\xe8\xaf\x8d\xe5\xbe\x88\xe6\x84\x9f\xe5\x85\xb4\xe8\xb6\xa3", 0);
}

static void text_processing(void) {
    UErrorCode status = U_ZERO_ERROR;
    UChar in[128], out[256];
    int32_t length;

    const UNormalizer2* nfc = unorm2_getNFCInstance(&status);
    const UNormalizer2* nfd = unorm2_getNFDInstance(&status);
    const UNormalizer2* nfkc = unorm2_getNFKCInstance(&status);
    const UNormalizer2* fold = unorm2_getNFKCCasefoldInstance(&status);
    length = utf16("e\xcc\x81\xef\xac\x81\xe2\x84\xab\xc3\x84", in, 128);
    int32_t n = unorm2_normalize(nfc, in, length, out, 256, &status);
    same_ustr("nfc", "x", out, n, status);
    n = unorm2_normalize(nfd, in, length, out, 256, &status);
    same_ustr("nfd", "x", out, n, status);
    n = unorm2_normalize(nfkc, in, length, out, 256, &status);
    same_ustr("nfkc", "x", out, n, status);
    n = unorm2_normalize(fold, in, length, out, 256, &status);
    same_ustr("nfkc_cf", "x", out, n, status);

    UIDNAInfo info = UIDNA_INFO_INITIALIZER;
    status = U_ZERO_ERROR;
    UIDNA* idna = uidna_openUTS46(UIDNA_DEFAULT, &status);
    static const char* const hosts[] = {"b\xc3\xbc" "cher.example", "\xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e.jp", "EXAMPLE.com",
                                        "\xd0\xbf\xd1\x80\xd0\xb8\xd0\xbc\xd0\xb5\xd1\x80.\xd1\x80\xd1\x84", "fa\xc3\x9f.de", "\xe2\x82\xac.com"};
    for (size_t i = 0; i < COUNT(hosts); i++) {
        char ascii[256];
        status = U_ZERO_ERROR;
        length = utf16(hosts[i], in, 128);
        UChar result[256];
        n = U_SUCCESS(status) ? uidna_nameToASCII(idna, in, length, result, 256, &info, &status) : 0;
        char name[64];
        snprintf(name, sizeof name, "idna.%zu", i);
        if (U_FAILURE(status)) { same(name, u_errorName(status)); continue; }
        for (int32_t c = 0; c < n; c++) ascii[c] = (char)result[c];
        ascii[n] = 0;
        char value[300];
        snprintf(value, sizeof value, "%s/err=%x", ascii, (unsigned)info.errors);
        same(name, value);
    }
    uidna_close(idna);

    status = U_ZERO_ERROR;
    length = utf16("istanbul i\xcc\x87 \xc3\x9f", in, 128);
    n = u_strToUpper(out, 256, in, length, "tr", &status);
    same_ustr("upper", "tr", out, n, status);
    n = u_strToLower(out, 256, in, length, "lt", &status);
    same_ustr("lower", "lt", out, n, status);
    n = u_strFoldCase(out, 256, in, length, U_FOLD_CASE_DEFAULT, &status);
    same_ustr("fold", "x", out, n, status);

    static const UChar32 sample[] = {0x1F600, 0x0915, 0x00E9, 0x4E2D, 0x0627, 0x200D, 0x2764, 0};
    char bits[64];
    int count = 0;
    for (const UChar32* p = sample; *p; p++) bits[count++] = (char)('0' + u_hasBinaryProperty(*p, UCHAR_EMOJI) + 2 * u_hasBinaryProperty(*p, UCHAR_EXTENDED_PICTOGRAPHIC));
    bits[count++] = (char)('a' + u_getIntPropertyValue(0x093F, UCHAR_INDIC_POSITIONAL_CATEGORY));
    bits[count++] = (char)('a' + u_getIntPropertyValue(0x3002, UCHAR_VERTICAL_ORIENTATION));
    bits[count++] = (char)('a' + u_getIntPropertyValue(0x0915, UCHAR_SCRIPT));
    bits[count++] = (char)('a' + u_getIntPropertyValue(0x0627, UCHAR_BIDI_CLASS));
    bits[count] = 0;
    same("uprops", bits);
    same("propname", u_getPropertyValueName(UCHAR_SCRIPT, USCRIPT_ARABIC, U_LONG_PROPERTY_NAME));

    status = U_ZERO_ERROR;
    USpoofChecker* checker = uspoof_open(&status);
    if (U_SUCCESS(status)) {
        UChar a[32], b[32];
        int32_t la = utf16("paypal", a, 32), lb = utf16("\xd1\x80\xd0\xb0\xd1\x83" "pal", b, 32);
        char value[40];
        snprintf(value, sizeof value, "%d", (int)uspoof_areConfusable(checker, a, la, b, lb, &status));
        same("spoof", value);
        uspoof_close(checker);
    } else {
        same("spoof", u_errorName(status));
    }

    status = U_ZERO_ERROR;
    char display[200];
    int32_t shown = uloc_getDisplayName("zh_Hant_TW", "en", out, 256, &status);
    for (int32_t c = 0; c < shown && c < 199; c++) display[c] = (char)out[c];
    display[shown < 199 ? shown : 199] = 0;
    same("localename", display);
    char tag[64];
    status = U_ZERO_ERROR;
    uloc_addLikelySubtags("zh", tag, 64, &status);
    same("likely.zh", tag);
    status = U_ZERO_ERROR;
    uloc_addLikelySubtags("sr", tag, 64, &status);
    same("likely.sr", tag);

    /* What the cut removed must fail cleanly. */
    status = U_ZERO_ERROR;
    UChar name[64];
    u_charName(0x41, U_UNICODE_CHAR_NAME, (char*)name, 64, &status);
    soft("charname", status);
}

int main(int argc, char** argv) {
    if (argc > 1) {
        FILE* file = fopen(argv[1], "rb");
        if (!file) { fprintf(stderr, "cannot open %s\n", argv[1]); return 2; }
        fseek(file, 0, SEEK_END);
        long size = ftell(file);
        fseek(file, 0, SEEK_SET);
        void* memory = NULL;
        if (posix_memalign(&memory, 16, (size_t)size) != 0 || fread(memory, 1, (size_t)size, file) != (size_t)size) return 2;
        fclose(file);
        UErrorCode status = U_ZERO_ERROR;
        udata_setCommonData(memory, &status);
        if (U_FAILURE(status)) { fprintf(stderr, "udata_setCommonData: %s\n", u_errorName(status)); return 2; }
    }
    converters();
    numbers_and_dates(KEPT_LOCALES, COUNT(KEPT_LOCALES), 1);
    numbers_and_dates(DROPPED_LOCALES, COUNT(DROPPED_LOCALES), 0);
    collation(KEPT_LOCALES, COUNT(KEPT_LOCALES), 1);
    collation(DROPPED_LOCALES, COUNT(DROPPED_LOCALES), 0);
    segmentation();
    text_processing();
    printf("ICUDATA:CHECK:PASS\n");
    return 0;
}
