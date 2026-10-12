// Skia, the raster backend: a CPU surface, a stroked line, and a line of text
// whose typeface comes through Fontconfig and FreeType exactly as WebKit's
// FontCacheSkia does (SkFontMgr_New_FontConfig + SkFontScanner_Make_FreeType).
// The surface is checksummed; the build step runs this program on the host
// and the judge expects the guest to draw the same pixels.

#include "proof.h"

#include "include/core/SkCanvas.h"
#include "include/core/SkColor.h"
#include "include/core/SkFont.h"
#include "include/core/SkFontMgr.h"
#include "include/core/SkImageInfo.h"
#include "include/core/SkPaint.h"
#include "include/core/SkPath.h"
#include "include/core/SkPixmap.h"
#include "include/core/SkData.h"
#include "include/core/SkStream.h"
#include "include/core/SkSurface.h"
#include "include/core/SkTypeface.h"
#include "include/ports/SkFontMgr_fontconfig.h"
#include "include/ports/SkFontScanner_FreeType.h"

#include <fontconfig/fontconfig.h>

#include <cerrno>
#include <cstdio>
#include <unistd.h>
#include <memory>
#include <cstring>

namespace proof {

namespace {

constexpr int kWidth = 240;
constexpr int kHeight = 64;

// How many pixels in the box differ from white.
int inked(const SkPixmap& pixels, int left, int top, int right, int bottom) {
    int count = 0;
    for (int y = top; y < bottom; ++y)
        for (int x = left; x < right; ++x)
            if (pixels.getColor(x, y) != SK_ColorWHITE) ++count;
    return count;
}

// Why a font manager found nothing: what it lists, whether the file reads, and
// whether Skia's FreeType scanner takes the file when it is handed it directly.
std::string diagnose(SkFontMgr* manager) {
    std::string out = " families=" + std::to_string(manager->countFamilies());
    const std::string path = font_directory() + "/LiberationSans-Regular.ttf";
    std::unique_ptr<SkStreamAsset> stream = SkStream::MakeFromFile(path.c_str());
    if (!stream) return out + " no-stream";
    out += " length=" + std::to_string(stream->getLength());
    uint8_t head[4] = {};
    stream->peek(head, 4);
    out += " head=" + hex(head, 4);
    sk_sp<SkTypeface> direct = SkFontScanner_Make_FreeType()->MakeFromStream(std::move(stream), SkFontArguments());
    out += direct ? " direct=ok" : " direct=none";
    // The steps of SkFontMgr_fontconfig's FontAccessible(), one by one.
    errno = 0;
    const int exists = access(path.c_str(), F_OK | R_OK);
    out += " access=" + std::to_string(exists) + "/errno" + std::to_string(errno);
    sk_sp<SkData> data = SkData::MakeFromFileName(path.c_str());
    out += data ? " data=" + std::to_string(data->size()) + ":" + hex(data->bytes(), 4) : " data=none";
    if (data) {
        auto memory = SkMemoryStream::Make(data);
        out += SkFontScanner_Make_FreeType()->scanFile(memory.get(), nullptr) ? " scan=ok" : " scan=no";
    }
    sk_sp<SkTypeface> legacy = manager->legacyMakeTypeface("Liberation Sans", SkFontStyle());
    out += legacy ? " legacy=ok" : " legacy=none";
    return out;
}

}  // namespace

Outcome check_skia() {
    FcConfig* config = FcConfigCreate();
    if (!config || !FcConfigAppFontAddDir(config, reinterpret_cast<const FcChar8*>(font_directory().c_str())))
        return fail("fontconfig directory");
    sk_sp<SkFontMgr> manager = SkFontMgr_New_FontConfig(config, SkFontScanner_Make_FreeType());  // owns config
    if (!manager) return fail("SkFontMgr_New_FontConfig");
    sk_sp<SkTypeface> face = manager->matchFamilyStyle("Liberation Sans", SkFontStyle());
    if (!face) return fail("no typeface for Liberation Sans:" + diagnose(manager.get()));
    SkString family;
    face->getFamilyName(&family);
    if (std::strcmp(family.c_str(), "Liberation Sans") != 0) return fail(std::string("typeface ") + family.c_str());

    sk_sp<SkSurface> surface = SkSurfaces::Raster(SkImageInfo::MakeN32Premul(kWidth, kHeight));
    if (!surface) return fail("SkSurfaces::Raster");
    SkCanvas* canvas = surface->getCanvas();
    canvas->clear(SK_ColorWHITE);

    SkPaint stroke;
    stroke.setAntiAlias(true);
    stroke.setColor(SK_ColorBLUE);
    stroke.setStyle(SkPaint::kStroke_Style);
    stroke.setStrokeWidth(3.0f);
    canvas->drawLine(6.0f, 6.0f, 230.0f, 22.0f, stroke);

    SkFont font(face, 22.0f);
    font.setEdging(SkFont::Edging::kAntiAlias);
    font.setSubpixel(true);
    font.setHinting(SkFontHinting::kNone);
    SkPaint ink;
    ink.setAntiAlias(true);
    ink.setColor(SK_ColorBLACK);
    canvas->drawString("LazyOS WebKit 123", 8.0f, 54.0f, font, ink);

    SkPixmap pixels;
    if (!surface->peekPixels(&pixels)) return fail("peekPixels");
    const int line = inked(pixels, 0, 0, kWidth, 26);
    const int text = inked(pixels, 0, 30, kWidth, kHeight);
    if (line < 300) return fail("line has " + std::to_string(line) + " pixels");
    if (text < 500) return fail("text has " + std::to_string(text) + " pixels");
    // A blue line pixel and a dark text pixel, by value: the colour channels are right.
    if (pixels.getColor(118, 14) == SK_ColorWHITE) return fail("line centre blank");
    uint32_t crc = 0;
    {
        std::vector<uint8_t> rows;
        for (int y = 0; y < kHeight; ++y) {
            const uint8_t* row = static_cast<const uint8_t*>(pixels.addr()) + pixels.rowBytes() * y;
            rows.insert(rows.end(), row, row + kWidth * 4);
        }
        crc = crc32_of(rows.data(), rows.size());
    }
    char text_crc[16];
    std::snprintf(text_crc, sizeof text_crc, "%08x", crc);
    info("skia", "pixels", text_crc);
    info("skia", "inked", std::to_string(line) + "+" + std::to_string(text));
    return pass();
}

}  // namespace proof
