// libpng, libjpeg-turbo and libwebp: each encodes the synthetic picture to a
// byte array, then decodes that array (and, for WebP, demuxes it).

#include "proof.h"

#include <jpeglib.h>
#include <png.h>
#include <webp/decode.h>
#include <webp/demux.h>
#include <webp/encode.h>

#include <csetjmp>
#include <cstdio>
#include <cstdlib>
#include <cstring>

namespace proof {

namespace {

constexpr int kStride = kPicture * 3;

// Largest per-channel difference between two RGB pictures.
int max_difference(const uint8_t* a, const uint8_t* b, size_t size) {
    int worst = 0;
    for (size_t i = 0; i < size; ++i) {
        const int d = a[i] > b[i] ? a[i] - b[i] : b[i] - a[i];
        if (d > worst) worst = d;
    }
    return worst;
}

std::string checksum(const std::vector<uint8_t>& pixels) {
    char text[16];
    std::snprintf(text, sizeof text, "%08x", crc32_of(pixels.data(), pixels.size()));
    return text;
}

struct JpegErrors {
    jpeg_error_mgr base;
    jmp_buf jump;
};

void jpeg_failed(j_common_ptr cinfo) {
    std::longjmp(reinterpret_cast<JpegErrors*>(cinfo->err)->jump, 1);
}

// Plain functions with plain locals around setjmp.
bool jpeg_encode(const std::vector<uint8_t>& rgb, unsigned char** out, unsigned long* size) {
    jpeg_compress_struct cinfo;
    JpegErrors errors;
    cinfo.err = jpeg_std_error(&errors.base);
    errors.base.error_exit = jpeg_failed;
    if (setjmp(errors.jump)) {
        jpeg_destroy_compress(&cinfo);
        return false;
    }
    jpeg_create_compress(&cinfo);
    jpeg_mem_dest(&cinfo, out, size);
    cinfo.image_width = kPicture;
    cinfo.image_height = kPicture;
    cinfo.input_components = 3;
    cinfo.in_color_space = JCS_RGB;
    jpeg_set_defaults(&cinfo);
    jpeg_set_quality(&cinfo, 90, TRUE);
    jpeg_start_compress(&cinfo, TRUE);
    while (cinfo.next_scanline < cinfo.image_height) {
        JSAMPROW row = const_cast<uint8_t*>(&rgb[cinfo.next_scanline * kStride]);
        jpeg_write_scanlines(&cinfo, &row, 1);
    }
    jpeg_finish_compress(&cinfo);
    jpeg_destroy_compress(&cinfo);
    return true;
}

bool jpeg_decode(const unsigned char* data, unsigned long size, std::vector<uint8_t>* rgb, int* width,
                 int* height) {
    jpeg_decompress_struct cinfo;
    JpegErrors errors;
    cinfo.err = jpeg_std_error(&errors.base);
    errors.base.error_exit = jpeg_failed;
    if (setjmp(errors.jump)) {
        jpeg_destroy_decompress(&cinfo);
        return false;
    }
    jpeg_create_decompress(&cinfo);
    jpeg_mem_src(&cinfo, data, size);
    jpeg_read_header(&cinfo, TRUE);
    cinfo.out_color_space = JCS_RGB;
    jpeg_start_decompress(&cinfo);
    *width = static_cast<int>(cinfo.output_width);
    *height = static_cast<int>(cinfo.output_height);
    rgb->assign(static_cast<size_t>(*width) * *height * 3, 0);
    while (cinfo.output_scanline < cinfo.output_height) {
        JSAMPROW row = &(*rgb)[cinfo.output_scanline * *width * 3];
        jpeg_read_scanlines(&cinfo, &row, 1);
    }
    jpeg_finish_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    return true;
}

}  // namespace

Outcome check_libpng() {
    const std::vector<uint8_t> rgb = test_picture();
    png_image write{};
    write.version = PNG_IMAGE_VERSION;
    write.width = kPicture;
    write.height = kPicture;
    write.format = PNG_FORMAT_RGB;
    png_alloc_size_t size = 0;
    if (!png_image_write_to_memory(&write, nullptr, &size, 0, rgb.data(), kStride, nullptr))
        return fail("size query");
    std::vector<uint8_t> file(size);
    if (!png_image_write_to_memory(&write, file.data(), &size, 0, rgb.data(), kStride, nullptr))
        return fail(std::string("write ") + write.message);
    file.resize(size);
    if (file.size() < 8 || std::memcmp(file.data(), "\x89PNG\r\n\x1a\n", 8) != 0) return fail("no PNG signature");

    png_image read{};
    read.version = PNG_IMAGE_VERSION;
    if (!png_image_begin_read_from_memory(&read, file.data(), file.size())) return fail(read.message);
    read.format = PNG_FORMAT_RGB;
    std::vector<uint8_t> back(PNG_IMAGE_SIZE(read));
    if (!png_image_finish_read(&read, nullptr, back.data(), kStride, nullptr)) return fail(read.message);
    if (read.width != kPicture || read.height != kPicture) return fail("size");
    if (back != rgb) return fail("lossless round trip differs");
    info("libpng", "bytes", std::to_string(file.size()));
    info("libpng", "pixels", checksum(back));
    return pass();
}

Outcome check_libjpeg() {
    const std::vector<uint8_t> rgb = test_picture();
    unsigned char* data = nullptr;
    unsigned long size = 0;
    if (!jpeg_encode(rgb, &data, &size)) return fail("encode");
    if (size < 4 || data[0] != 0xff || data[1] != 0xd8) {
        std::free(data);
        return fail("no JPEG signature");
    }
    std::vector<uint8_t> back;
    int width = 0, height = 0;
    const bool ok = jpeg_decode(data, size, &back, &width, &height);
    std::free(data);
    if (!ok) return fail("decode");
    if (width != kPicture || height != kPicture) return fail("size");
    const int worst = max_difference(rgb.data(), back.data(), rgb.size());
    if (worst > 24) return fail("error " + std::to_string(worst));
    info("libjpeg-turbo", "pixels", checksum(back));
    return pass();
}

Outcome check_libwebp() {
    const std::vector<uint8_t> rgb = test_picture();
    uint8_t* lossless = nullptr;
    const size_t lossless_size = WebPEncodeLosslessRGB(rgb.data(), kPicture, kPicture, kStride, &lossless);
    if (lossless_size == 0) return fail("lossless encode");
    int width = 0, height = 0;
    uint8_t* back = WebPDecodeRGB(lossless, lossless_size, &width, &height);
    const bool exact = back && width == kPicture && height == kPicture &&
                       std::memcmp(back, rgb.data(), rgb.size()) == 0;
    std::string pixels = back ? checksum(std::vector<uint8_t>(back, back + rgb.size())) : "";
    WebPFree(back);
    if (!exact) {
        WebPFree(lossless);
        return fail("lossless round trip differs");
    }
    // The demuxer WebCore uses: canvas size and frame count of the container.
    WebPData container{lossless, lossless_size};
    WebPDemuxer* demux = WebPDemux(&container);
    if (!demux) {
        WebPFree(lossless);
        return fail("demux");
    }
    const uint32_t canvas = WebPDemuxGetI(demux, WEBP_FF_CANVAS_WIDTH);
    const uint32_t frames = WebPDemuxGetI(demux, WEBP_FF_FRAME_COUNT);
    WebPDemuxDelete(demux);
    WebPFree(lossless);
    if (canvas != kPicture || frames != 1) return fail("demux canvas " + std::to_string(canvas));

    uint8_t* lossy = nullptr;
    const size_t lossy_size = WebPEncodeRGB(rgb.data(), kPicture, kPicture, kStride, 90.0f, &lossy);
    if (lossy_size == 0) return fail("lossy encode");
    uint8_t* approx = WebPDecodeRGB(lossy, lossy_size, &width, &height);
    const int worst = approx ? max_difference(rgb.data(), approx, rgb.size()) : 255;
    WebPFree(approx);
    WebPFree(lossy);
    if (worst > 40) return fail("lossy error " + std::to_string(worst));
    info("libwebp", "pixels", pixels);
    return pass();
}

}  // namespace proof
