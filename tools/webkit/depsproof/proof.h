// depsproof: shared declarations. One check per library of the bundle
// (tools/webkit/README.md); each prints WEBKIT:W3:PASS:<lib> or
// WEBKIT:W3:FAIL:<lib>:<reason> through the helpers below.
#pragma once

#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>

// Guest paths come from libs/fhs, not from this file: proof.py passes them as
// -D macros (tools/webkit/fhspaths.py reads the fhs crate), the same values the
// libraries were built with.
#if !defined(DEPSPROOF_CA_BUNDLE) || !defined(DEPSPROOF_FONT_CACHE_DIR) || !defined(DEPSPROOF_TMP)
#error "compile through tools/webkit/proof.py: it defines the libs/fhs paths"
#endif

namespace proof {

// A check's verdict. `detail` is printed on failure (colons are replaced).
struct Outcome {
    bool ok = true;
    std::string detail;
};

inline Outcome fail(std::string why) { return {false, std::move(why)}; }
inline Outcome pass() { return {}; }

// Evidence that a judge can compare between hosts: WEBKIT:W3:INFO:<lib>:<key>=<value>.
void info(const char* lib, const char* key, const std::string& value);
std::string hex(const uint8_t* data, size_t size);
uint32_t crc32_of(const uint8_t* data, size_t size);

// Settings from the command line.
struct Context {
    std::string url;  // the HTTPS page curl fetches; empty = offline (host reference run)
};

// The embedded Liberation Sans Regular (fontdata.cpp is generated at build time).
extern const unsigned char kFont[];
extern const unsigned long kFontSize;
// A directory holding it, written on first use (fontconfig scans a directory).
const std::string& font_directory();

Outcome check_zlib();
Outcome check_brotli();
Outcome check_icu();
Outcome check_libpng();
Outcome check_libjpeg();
Outcome check_libwebp();
Outcome check_libxml2();
Outcome check_expat();
Outcome check_sqlite();
Outcome check_freetype();
Outcome check_harfbuzz();
Outcome check_fontconfig();
Outcome check_skia();
Outcome check_libpsl();
Outcome check_openssl();
Outcome check_nghttp2();
Outcome check_curl(const Context&);

// Synthetic test picture shared by the three image checks: 32x32 RGB.
constexpr int kPicture = 32;
std::vector<uint8_t> test_picture();

}  // namespace proof
