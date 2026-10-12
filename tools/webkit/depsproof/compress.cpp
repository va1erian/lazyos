// zlib, brotli and ICU.

#include "proof.h"

#include <brotli/decode.h>
#include <brotli/encode.h>
#include <unicode/ubrk.h>
#include <unicode/ucnv.h>
#include <unicode/ustring.h>
#include <unicode/unorm2.h>
#include <zlib.h>

#include <cstdio>
#include <cstring>

namespace proof {

namespace {

// Compressible but not trivial: a repeating phrase with a counter.
std::vector<uint8_t> sample(size_t size) {
    std::vector<uint8_t> out(size);
    for (size_t i = 0; i < size; ++i)
        out[i] = static_cast<uint8_t>("the quick brown fox jumps over the lazy dog "[i % 44] + (i / 4096));
    return out;
}

}  // namespace

Outcome check_zlib() {
    const std::vector<uint8_t> data = sample(1 << 17);
    uLongf packed_size = compressBound(data.size());
    std::vector<uint8_t> packed(packed_size);
    if (compress2(packed.data(), &packed_size, data.data(), data.size(), 6) != Z_OK)
        return fail("compress2");
    if (packed_size >= data.size() / 4) return fail("poor ratio " + std::to_string(packed_size));
    std::vector<uint8_t> back(data.size());
    uLongf back_size = back.size();
    if (uncompress(back.data(), &back_size, packed.data(), packed_size) != Z_OK)
        return fail("uncompress");
    if (back_size != data.size() || std::memcmp(back.data(), data.data(), data.size()) != 0)
        return fail("round trip differs");
    // A gzip stream through inflate (the Content-Encoding: gzip path).
    z_stream stream{};
    if (deflateInit2(&stream, 6, Z_DEFLATED, 15 + 16, 8, Z_DEFAULT_STRATEGY) != Z_OK) return fail("deflateInit2");
    std::vector<uint8_t> gz(compressBound(data.size()) + 32);
    stream.next_in = const_cast<uint8_t*>(data.data());
    stream.avail_in = static_cast<uInt>(data.size());
    stream.next_out = gz.data();
    stream.avail_out = static_cast<uInt>(gz.size());
    if (deflate(&stream, Z_FINISH) != Z_STREAM_END) return fail("deflate gzip");
    const size_t gz_size = gz.size() - stream.avail_out;
    deflateEnd(&stream);
    z_stream in{};
    if (inflateInit2(&in, 15 + 32) != Z_OK) return fail("inflateInit2");
    std::vector<uint8_t> out(data.size());
    in.next_in = gz.data();
    in.avail_in = static_cast<uInt>(gz_size);
    in.next_out = out.data();
    in.avail_out = static_cast<uInt>(out.size());
    const int status = inflate(&in, Z_FINISH);
    inflateEnd(&in);
    if (status != Z_STREAM_END || out != data) return fail("gzip inflate");
    char crc[16];
    std::snprintf(crc, sizeof crc, "%08x", static_cast<unsigned>(crc32(0, data.data(), static_cast<uInt>(data.size()))));
    info("zlib", "crc32", crc);
    info("zlib", "version", zlibVersion());
    return pass();
}

Outcome check_brotli() {
    const std::vector<uint8_t> data = sample(1 << 16);
    size_t packed_size = BrotliEncoderMaxCompressedSize(data.size());
    std::vector<uint8_t> packed(packed_size);
    if (!BrotliEncoderCompress(5, 22, BROTLI_MODE_GENERIC, data.size(), data.data(), &packed_size, packed.data()))
        return fail("encode");
    std::vector<uint8_t> back(data.size());
    size_t back_size = back.size();
    if (BrotliDecoderDecompress(packed_size, packed.data(), &back_size, back.data()) != BROTLI_DECODER_RESULT_SUCCESS)
        return fail("decode");
    if (back != data) return fail("round trip differs");
    return pass();
}

Outcome check_icu() {
    // Converters (data compiled in), normalisation, case mapping, word breaks.
    UErrorCode status = U_ZERO_ERROR;
    UConverter* latin1 = ucnv_open("ISO-8859-1", &status);
    if (U_FAILURE(status)) return fail("ucnv_open ISO-8859-1");
    UChar text[16];
    const int32_t length = ucnv_toUChars(latin1, text, 16, "caf\xe9", 4, &status);
    ucnv_close(latin1);
    if (U_FAILURE(status) || length != 4 || text[3] != 0x00e9) return fail("latin1 conversion");
    const UNormalizer2* nfd = unorm2_getNFDInstance(&status);
    UChar decomposed[16];
    const int32_t nfd_length = unorm2_normalize(nfd, text, length, decomposed, 16, &status);
    if (U_FAILURE(status) || nfd_length != 5) return fail("NFD length");
    UChar upper[32];
    const int32_t upper_length = u_strToUpper(upper, 32, u"straße", -1, "de", &status);
    if (U_FAILURE(status) || upper_length != 7) return fail("upper case of strasse");
    UBreakIterator* words = ubrk_open(UBRK_WORD, "en", u"one two, three", -1, &status);
    if (U_FAILURE(status)) return fail("ubrk_open");
    int count = 0;
    while (ubrk_next(words) != UBRK_DONE) ++count;
    ubrk_close(words);
    if (count != 6) return fail("word breaks " + std::to_string(count));
    return pass();
}

}  // namespace proof
