// depsproof: links every library of the WebKit dependency bundle and uses
// each one for real (docs/webkit-port-plan.md W3). Prints
//   WEBKIT:W3:PASS:<lib> | WEBKIT:W3:FAIL:<lib>:<reason>   per library,
//   WEBKIT:W3:INFO:<lib>:<key>=<value>                      evidence a judge compares,
//   WEBKIT:W3:PASS                                          when every library passed,
// and, right after WEBKIT:W3:UP, WEBKIT:W3:BIN:<sha256> of this very executable
// (read through /proc/self/exe), so a judge can tell which build it is judging.
// With a URL argument the curl check fetches it over HTTPS; without one the
// run is the offline host reference (WEBKIT:W3:SKIP:curl:offline, then
// WEBKIT:W3:OFFLINE), which the build step uses to compute expected checksums.

#include "proof.h"

#include <cstdlib>
#include <cstring>
#include <exception>
#include <fstream>
#include <openssl/evp.h>
#include <sys/stat.h>
#include <unistd.h>

namespace proof {

void info(const char* lib, const char* key, const std::string& value) {
    std::printf("WEBKIT:W3:INFO:%s:%s=%s\n", lib, key, value.c_str());
    std::fflush(stdout);
}

std::string hex(const uint8_t* data, size_t size) {
    static const char digits[] = "0123456789abcdef";
    std::string out;
    for (size_t i = 0; i < size; ++i) {
        out += digits[data[i] >> 4];
        out += digits[data[i] & 15];
    }
    return out;
}

uint32_t crc32_of(const uint8_t* data, size_t size) {
    uint32_t crc = 0xffffffffu;
    for (size_t i = 0; i < size; ++i) {
        crc ^= data[i];
        for (int bit = 0; bit < 8; ++bit)
            crc = (crc >> 1) ^ (0xedb88320u & (0u - (crc & 1u)));
    }
    return ~crc;
}

std::vector<uint8_t> test_picture() {
    std::vector<uint8_t> rgb(kPicture * kPicture * 3);
    for (int y = 0; y < kPicture; ++y)
        for (int x = 0; x < kPicture; ++x) {
            uint8_t* px = &rgb[(y * kPicture + x) * 3];
            px[0] = static_cast<uint8_t>(x * 8);
            px[1] = static_cast<uint8_t>(y * 8);
            px[2] = static_cast<uint8_t>((x + y) * 4);
        }
    return rgb;
}

const std::string& font_directory() {
    static std::string dir;
    if (dir.empty()) {
        const char* base = std::getenv("TMPDIR");
        dir = std::string(base ? base : DEPSPROOF_TMP) + "/depsproof-fonts";
        mkdir(dir.c_str(), 0755);
        mkdir(DEPSPROOF_FONT_CACHE_DIR, 0755);  // the cache directory Fontconfig was built with
        std::ofstream out(dir + "/LiberationSans-Regular.ttf", std::ios::binary | std::ios::trunc);
        out.write(reinterpret_cast<const char*>(kFont), static_cast<std::streamsize>(kFontSize));
    }
    return dir;
}

}  // namespace proof

namespace {

// SHA-256 of the running executable ("" when it cannot be read).
std::string self_digest() {
    char path[4096];
    const ssize_t length = readlink("/proc/self/exe", path, sizeof path - 1);
    if (length <= 0) return "";
    path[length] = '\0';
    std::FILE* file = std::fopen(path, "rb");
    if (!file) return "";
    EVP_MD_CTX* ctx = EVP_MD_CTX_new();
    EVP_DigestInit_ex(ctx, EVP_sha256(), nullptr);
    static uint8_t chunk[65536];
    size_t got;
    while ((got = std::fread(chunk, 1, sizeof chunk, file)) > 0) EVP_DigestUpdate(ctx, chunk, got);
    const bool whole = !std::ferror(file);
    std::fclose(file);
    uint8_t digest[EVP_MAX_MD_SIZE];
    unsigned size = 0;
    EVP_DigestFinal_ex(ctx, digest, &size);
    EVP_MD_CTX_free(ctx);
    return whole ? proof::hex(digest, size) : "";
}

std::string tidy(std::string text) {
    for (char& c : text)
        if (c == ':' || c == '\n' || c == '\r') c = ';';
    return text.substr(0, 200);
}

struct Entry {
    const char* lib;
    proof::Outcome (*run)();
};

bool report(const char* lib, const proof::Outcome& outcome) {
    if (outcome.ok)
        std::printf("WEBKIT:W3:PASS:%s\n", lib);
    else
        std::printf("WEBKIT:W3:FAIL:%s:%s\n", lib, tidy(outcome.detail).c_str());
    std::fflush(stdout);
    return outcome.ok;
}

template <typename F>
proof::Outcome guarded(F&& body) {
    try {
        return body();
    } catch (const std::exception& error) {
        return proof::fail(std::string("exception ") + error.what());
    } catch (...) {
        return proof::fail("unknown exception");
    }
}

}  // namespace

int main(int argc, char** argv) {
    std::setvbuf(stdout, nullptr, _IONBF, 0);
    proof::Context context;
    if (argc > 1) context.url = argv[1];
    std::printf("WEBKIT:W3:UP\n");
    const std::string self = self_digest();
    std::printf("WEBKIT:W3:BIN:%s\n", self.empty() ? "unreadable" : self.c_str());

    const Entry entries[] = {
        {"zlib", proof::check_zlib},         {"brotli", proof::check_brotli},
        {"icu", proof::check_icu},           {"libpng", proof::check_libpng},
        {"libjpeg-turbo", proof::check_libjpeg}, {"libwebp", proof::check_libwebp},
        {"libxml2", proof::check_libxml2},   {"expat", proof::check_expat},
        {"sqlite", proof::check_sqlite},     {"freetype", proof::check_freetype},
        {"harfbuzz", proof::check_harfbuzz}, {"fontconfig", proof::check_fontconfig},
        {"skia", proof::check_skia},         {"libpsl", proof::check_libpsl},
        {"openssl", proof::check_openssl},   {"nghttp2", proof::check_nghttp2},
    };
    bool all = true;
    for (const Entry& entry : entries)
        all &= report(entry.lib, guarded([&] { return entry.run(); }));
    if (context.url.empty()) {
        std::printf("WEBKIT:W3:SKIP:curl:offline\n");
        std::printf("WEBKIT:W3:%s\n", all ? "OFFLINE" : "FAIL:offline");
        return all ? 0 : 1;
    }
    all &= report("curl", guarded([&] { return proof::check_curl(context); }));
    std::printf(all ? "WEBKIT:W3:PASS\n" : "WEBKIT:W3:FAIL:see above\n");
    return all ? 0 : 1;
}
