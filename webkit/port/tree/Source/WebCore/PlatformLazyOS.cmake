# WebCore on LazyOS. Derived from Source/WebCore/PlatformPlayStation.cmake of
# the WebKit commit the webkitgtk-2.54.1 tag points to.
#
# Same as the PlayStation port: curl, OpenSSL, the built-in image decoders,
# Skia, the TextureMapper classes, the PlayStation theme, scrollbars, screen,
# MIME registry and user agent (all in the tarball under */playstation/), the
# libwpe keyboard and pasteboard glue (fetched, webkit/port/upstream.txt).
#
# Different, and why:
#  * No EGL / WPE renderer files (GLContextLibWPE, PlatformDisplayLibWPE,
#    gamepad, EGL extras, ResourceUsage*): USE_WPE_RENDERER and ENABLE_GAMEPAD are
#    off; the EGL entry points that GLContext.cpp and PlatformDisplay.cpp call are
#    the do-nothing stand-ins of tools/webkit/recipes_port.py.
#  * No WebKitRequirements::WebKitResources and no PlayStation module copying.
#  * WebCore's libraries are the static archives of the dependency bundle; the
#    ones that a static link needs in a particular order are listed once at the
#    end (LAZYOS_STATIC_LIBRARIES), after everything that uses them.

include(platform/Curl.cmake)
include(platform/ImageDecoders.cmake)
include(platform/OpenSSL.cmake)
include(platform/TextureMapper.cmake)
include(platform/Skia.cmake)

list(APPEND WebCore_PRIVATE_INCLUDE_DIRECTORIES
    ${WEBCORE_DIR}/platform
    ${WEBCORE_DIR}/platform/generic
    ${WEBCORE_DIR}/platform/graphics/egl
    ${WEBCORE_DIR}/platform/graphics/opengl
    ${WEBCORE_DIR}/platform/mediacapabilities
    ${WEBCORE_DIR}/platform/video-codecs
    ${WEBCORE_DIR}/rendering/playstation
)

list(APPEND WebCore_SOURCES
    accessibility/playstation/AXObjectCachePlayStation.cpp
    accessibility/playstation/AccessibilityObjectPlayStation.cpp

    editing/libwpe/EditorLibWPE.cpp

    platform/ScrollAnimationKinetic.cpp
    platform/ScrollAnimationSmooth.cpp

    platform/generic/KeyedDecoderGeneric.cpp
    platform/generic/KeyedEncoderGeneric.cpp

    platform/graphics/PlatformDisplay.cpp

    platform/graphics/egl/GLContext.cpp
    platform/graphics/egl/GLContextWrapper.cpp
    platform/graphics/egl/GLDisplay.cpp
    platform/graphics/egl/GLFence.cpp
    platform/graphics/egl/GLFenceEGL.cpp
    platform/graphics/egl/GLFenceGL.cpp

    platform/graphics/playstation/SystemFontDatabasePlayStation.cpp

    platform/libwpe/PasteboardLibWPE.cpp
    platform/libwpe/PlatformKeyboardEventLibWPE.cpp
    platform/libwpe/PlatformPasteboardLibWPE.cpp

    platform/network/lazyos/CurlSSLHandleLazyOS.cpp
    platform/network/playstation/NetworkStateNotifierPlayStation.cpp

    platform/playstation/MIMETypeRegistryPlayStation.cpp
    platform/playstation/PlatformScreenPlayStation.cpp
    platform/playstation/ScrollbarThemePlayStation.cpp
    platform/playstation/ThemePlayStation.cpp
    platform/playstation/UserAgentPlayStation.cpp

    platform/text/Hyphenation.cpp
    platform/text/LocaleICU.cpp

    platform/unix/LoggingUnix.cpp
    platform/unix/SharedMemoryUnix.cpp

    rendering/playstation/RenderThemePlayStation.cpp
)

list(APPEND WebCore_LIBRARIES
    WPE::libwpe
)

# The CA bundle's path in the image, from libs/fhs (tools/webkit/port_cmake.py passes it).
if (NOT LAZYOS_CA_BUNDLE_PATH)
    message(FATAL_ERROR "LAZYOS_CA_BUNDLE_PATH is not set (tools/webkit/port_cmake.py passes it from libs/fhs)")
endif ()
set_source_files_properties("${CMAKE_CURRENT_SOURCE_DIR}/platform/network/lazyos/CurlSSLHandleLazyOS.cpp"
    PROPERTIES COMPILE_DEFINITIONS "LAZYOS_CA_BUNDLE_PATH=\"${LAZYOS_CA_BUNDLE_PATH}\"")

list(APPEND WebCore_PRIVATE_FRAMEWORK_HEADERS
    rendering/playstation/RenderThemePlayStation.h
)

# LazyOS has no file-backed mmap yet (docs/linux-abi-plan.md) and Skia's fontconfig font
# manager maps font files with mmap and no fallback to reading, so it refuses every font and
# every family falls back to the same face (found by running W4: Liberation Serif drew as Sans).
# W3's lazyos_mmap shim copies the file instead; it is linked into Skia and SkOSFile_posix.cpp
# is compiled against it, as in tools/webkit/skiabuild. Remove with the kernel limitation.
if (TARGET Skia)
    target_sources(Skia PRIVATE "${LAZYOS_PORT_DIR}/../../tools/webkit/skiabuild/lazyos_mmap.c")
    set_source_files_properties("${CMAKE_SOURCE_DIR}/Source/ThirdParty/skia/src/ports/SkOSFile_posix.cpp"
        TARGET_DIRECTORY Skia PROPERTIES COMPILE_DEFINITIONS "mmap=lazyos_mmap")
endif ()

# The dependency bundle is static, and an archive must come after the archives
# that use it: FreeType (PNG, zlib, brotli), Fontconfig (FreeType, expat), curl
# (nghttp2, brotli, libpsl, OpenSSL, zlib), libxml2 (ICU, zlib).
if (LAZYOS_STATIC_LIBRARIES)
    list(APPEND WebCore_LIBRARIES ${LAZYOS_STATIC_LIBRARIES})
endif ()
