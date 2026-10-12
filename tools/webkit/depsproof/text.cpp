// FreeType, HarfBuzz (with the ICU and FreeType glue) and Fontconfig, all on
// the embedded Liberation Sans.

#include "proof.h"

#include <fontconfig/fontconfig.h>
#include <ft2build.h>
#include FT_FREETYPE_H
#include <hb-ft.h>
#include <hb-icu.h>
#include <hb-ot.h>
#include <hb.h>

#include <cstdio>
#include <cstring>

namespace proof {

Outcome check_freetype() {
    FT_Library library = nullptr;
    if (FT_Init_FreeType(&library)) return fail("FT_Init_FreeType");
    FT_Face face = nullptr;
    if (FT_New_Memory_Face(library, kFont, static_cast<FT_Long>(kFontSize), 0, &face)) {
        FT_Done_FreeType(library);
        return fail("FT_New_Memory_Face");
    }
    std::string problem;
    if (std::strcmp(face->family_name, "Liberation Sans") != 0) problem = std::string("family ") + face->family_name;
    if (problem.empty() && FT_Set_Pixel_Sizes(face, 0, 32)) problem = "FT_Set_Pixel_Sizes";
    long coverage = 0;
    uint32_t crc = 0;
    if (problem.empty()) {
        if (FT_Load_Char(face, 'A', FT_LOAD_RENDER | FT_LOAD_TARGET_NORMAL)) {
            problem = "FT_Load_Char";
        } else {
            const FT_Bitmap& bitmap = face->glyph->bitmap;
            for (unsigned row = 0; row < bitmap.rows; ++row)
                for (unsigned col = 0; col < bitmap.width; ++col) coverage += bitmap.buffer[row * bitmap.pitch + col];
            crc = crc32_of(bitmap.buffer, static_cast<size_t>(bitmap.pitch) * bitmap.rows);
            if (bitmap.rows < 15 || coverage < 5000) problem = "A is blank";
        }
    }
    const FT_Long glyphs = face->num_glyphs;
    FT_Done_Face(face);
    FT_Done_FreeType(library);
    if (!problem.empty()) return fail(problem);
    if (glyphs < 1000) return fail("few glyphs");
    char text[16];
    std::snprintf(text, sizeof text, "%08x", crc);
    info("freetype", "glyph-A", text);
    return pass();
}

Outcome check_harfbuzz() {
    hb_blob_t* blob = hb_blob_create(reinterpret_cast<const char*>(kFont), static_cast<unsigned>(kFontSize),
                                     HB_MEMORY_MODE_READONLY, nullptr, nullptr);
    hb_face_t* face = hb_face_create(blob, 0);
    hb_font_t* font = hb_font_create(face);
    hb_ot_font_set_funcs(font);
    const unsigned upem = hb_face_get_upem(face);
    hb_font_set_scale(font, static_cast<int>(upem), static_cast<int>(upem));

    const char text[] = "Office affinity: WAVE 1,234.5 \xc3\xa9\xc3\xa0";
    hb_buffer_t* buffer = hb_buffer_create();
    hb_buffer_set_unicode_funcs(buffer, hb_icu_get_unicode_funcs());  // the ICU glue
    hb_buffer_add_utf8(buffer, text, -1, 0, -1);
    hb_buffer_guess_segment_properties(buffer);
    hb_shape(font, buffer, nullptr, 0);
    unsigned count = 0;
    const hb_glyph_info_t* info_array = hb_buffer_get_glyph_infos(buffer, &count);
    const hb_glyph_position_t* positions = hb_buffer_get_glyph_positions(buffer, &count);
    long advance = 0;
    std::vector<unsigned> glyphs;
    for (unsigned i = 0; i < count; ++i) {
        advance += positions[i].x_advance;
        glyphs.push_back(info_array[i].codepoint);
    }

    // The same text through the FreeType font functions must pick the same glyphs.
    FT_Library library = nullptr;
    FT_Face ft_face = nullptr;
    std::string problem;
    if (FT_Init_FreeType(&library) || FT_New_Memory_Face(library, kFont, static_cast<FT_Long>(kFontSize), 0, &ft_face))
        problem = "FreeType face";
    if (problem.empty()) {
        hb_font_t* ft_font = hb_ft_font_create_referenced(ft_face);
        hb_buffer_t* second = hb_buffer_create();
        hb_buffer_add_utf8(second, text, -1, 0, -1);
        hb_buffer_guess_segment_properties(second);
        hb_shape(ft_font, second, nullptr, 0);
        unsigned second_count = 0;
        const hb_glyph_info_t* second_info = hb_buffer_get_glyph_infos(second, &second_count);
        if (second_count != count) problem = "hb-ft glyph count " + std::to_string(second_count);
        for (unsigned i = 0; problem.empty() && i < count; ++i)
            if (second_info[i].codepoint != glyphs[i]) problem = "hb-ft glyph " + std::to_string(i);
        hb_buffer_destroy(second);
        hb_font_destroy(ft_font);
    }
    if (ft_face) FT_Done_Face(ft_face);
    if (library) FT_Done_FreeType(library);
    hb_buffer_destroy(buffer);
    hb_font_destroy(font);
    hb_face_destroy(face);
    hb_blob_destroy(blob);
    if (!problem.empty()) return fail(problem);
    if (count < 20 || count > std::strlen(text)) return fail("glyph count " + std::to_string(count));
    if (advance < static_cast<long>(upem) * 8) return fail("advance " + std::to_string(advance));
    info("harfbuzz", "glyphs", std::to_string(count));
    info("harfbuzz", "advance", std::to_string(advance));
    return pass();
}

Outcome check_fontconfig() {
    FcConfig* config = FcConfigCreate();
    if (!config) return fail("FcConfigCreate");
    if (!FcConfigAppFontAddDir(config, reinterpret_cast<const FcChar8*>(font_directory().c_str()))) {
        FcConfigDestroy(config);
        return fail("FcConfigAppFontAddDir");
    }
    FcPattern* pattern = FcNameParse(reinterpret_cast<const FcChar8*>("Liberation Sans:weight=regular"));
    FcConfigSubstitute(config, pattern, FcMatchPattern);
    FcDefaultSubstitute(pattern);
    FcResult result;
    FcPattern* match = FcFontMatch(config, pattern, &result);
    std::string problem;
    if (!match) {
        problem = "no match";
    } else {
        FcChar8* file = nullptr;
        FcChar8* family = nullptr;
        if (FcPatternGetString(match, FC_FILE, 0, &file) != FcResultMatch ||
            FcPatternGetString(match, FC_FAMILY, 0, &family) != FcResultMatch)
            problem = "match has no file";
        else if (std::strstr(reinterpret_cast<const char*>(file), "LiberationSans-Regular.ttf") == nullptr)
            problem = std::string("matched ") + reinterpret_cast<const char*>(file);
        else if (std::strcmp(reinterpret_cast<const char*>(family), "Liberation Sans") != 0)
            problem = "family";
    }
    if (match) FcPatternDestroy(match);
    FcPatternDestroy(pattern);
    FcConfigDestroy(config);
    return problem.empty() ? pass() : fail(problem);
}

}  // namespace proof
