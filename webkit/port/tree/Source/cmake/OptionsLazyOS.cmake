# The LazyOS port of WebKit (docs/webkit-port-plan.md, W4).
#
# Derived from Source/cmake/OptionsPlayStation.cmake of the WebKit commit the
# webkitgtk-2.54.1 tag points to (628d6ce1850e17225185a5807a9d0aa375bae2be); the
# release tarball does not carry that file (webkit/README.md, "The glue").
#
# What is the same as the PlayStation port: the WebKit2 process model, curl and
# OpenSSL for the network, Skia for painting, FreeType, Fontconfig and HarfBuzz
# for fonts, libwpe's input and pasteboard types, the generic run loop and the
# POSIX file system and threads.
#
# What is different, and why:
#  * PORT is LazyOS and the platform macro is PLATFORM(LAZYOS) (the port's own
#    WTF_PLATFORM_LAZYOS, -DWTF_PLATFORM_LAZYOS=1 below). PLATFORM(PLAYSTATION)
#    makes WTF and JavaScriptCore choose PlayStation allocator and assertion
#    code that is wrong on Linux, so WebCore's and WebKit's own PLAYSTATION
#    checks are rewritten to LAZYOS by patches/0003, and WTF, JSC and bmalloc
#    are built exactly as the proven JSCOnly configuration (docs W2).
#  * Everything is a static library and the executables are static-PIE musl
#    programs built by zig, not shared modules signed for a console: no
#    PlayStationModule, Sign, sce_sys, WebKitRequirements, libdl or MemoryExtra.
#  * The JIT is on (LLInt, baseline, DFG; no FTL, which needs WebAssembly's B3
#    values at this revision), like jsc-jit; the PlayStation port turns it off.
#  * libwpe is built static, without libxkbcommon, and has no backend: the
#    PlayStation "WPE backend" and "WPE renderer" (USE_WPE_BACKEND_PLAYSTATION,
#    USE_WPE_RENDERER) are off, so WKViewCreate (the non-backend path) is the API.
#  * Painting is on the CPU: no EGL, GLESv2 or libepoxy exists; libEGL.a and
#    libGLESv2.a are stand-ins (tools/webkit/recipes_port.py) that report no
#    display, and acceleratedCompositingEnabled stays false (WebPreferencesLazyOS).
#  * Off, because LazyOS has no use for it yet: video, WebGL, WebRTC, Web Audio,
#    the GPU process, the remote inspector, the sandbox, WebAssembly.

set(PORT LazyOS)

include(GNUInstallDirs)
include(VersioningUtils)

add_definitions(-DWTF_PLATFORM_LAZYOS=1)

set(ENABLE_WEBKIT_LEGACY OFF)
set(ENABLE_WEBINSPECTORUI OFF)

find_package(ICU 70.1 REQUIRED COMPONENTS data i18n uc)
find_package(Threads REQUIRED)

if (ENABLE_WEBCORE)
    find_package(Brotli OPTIONAL_COMPONENTS dec)
    find_package(CURL 7.85.0 REQUIRED)
    find_package(EGL REQUIRED)
    find_package(Fontconfig REQUIRED)
    find_package(Freetype 2.9.0 REQUIRED)
    find_package(HarfBuzz 2.7.4 REQUIRED COMPONENTS ICU)
    find_package(JPEG REQUIRED)
    find_package(LibPSL 0.20.2 REQUIRED)
    find_package(LibXml2 2.9.7 REQUIRED)
    find_package(OpenGLES2 REQUIRED)
    find_package(OpenSSL REQUIRED)
    find_package(PNG REQUIRED)
    find_package(SQLite3 3.23.1 REQUIRED)
    find_package(WebP REQUIRED COMPONENTS demux)
    find_package(WPE 1.14.0 REQUIRED)
    find_package(ZLIB REQUIRED)

    if (NOT TARGET SQLite3::SQLite3) # CMake < 4.3
        add_library(SQLite3::SQLite3 ALIAS SQLite::SQLite3)
    endif ()
endif ()

WEBKIT_OPTION_BEGIN()

SET_PROJECT_VERSION(2 54 1)
CALCULATE_LIBRARY_VERSIONS_FROM_LIBTOOL_TRIPLE(WEBKIT 24 4 24)
CALCULATE_LIBRARY_VERSIONS_FROM_LIBTOOL_TRIPLE(JAVASCRIPTCORE 11 8 11)

# Where `install` would put things (WebKit's CMakeLists installs the targets); unused here.
set(LIB_INSTALL_DIR "${CMAKE_INSTALL_FULL_LIBDIR}" CACHE PATH "Absolute path to library installation directory")
set(EXEC_INSTALL_DIR "${CMAKE_INSTALL_FULL_BINDIR}" CACHE PATH "Absolute path to executable installation directory")
set(LIBEXEC_INSTALL_DIR "${CMAKE_INSTALL_FULL_LIBEXECDIR}/webkit" CACHE PATH "Absolute path to install executables executed by the library")

# PlayStation specific options that the LazyOS port keeps.
WEBKIT_OPTION_DEFINE(ENABLE_STATIC_JSC "Control whether to build a non-shared JSC" PUBLIC ON)

# JavaScriptCore: the configuration `jsc-jit` proved (tools/jsc/webkit.py).
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_JIT PRIVATE ON)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_C_LOOP PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_DFG_JIT PRIVATE ON)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_FTL_JIT PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_SAMPLING_PROFILER PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_WEBASSEMBLY PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(USE_SYSTEM_MALLOC PRIVATE ON)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(USE_ISO_MALLOC PRIVATE OFF)

# Raster backend and decoders.
WEBKIT_OPTION_DEFAULT_PORT_VALUE(USE_SKIA PRIVATE ON)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(USE_SKIA_ENCODERS PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(USE_AVIF PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(USE_LCMS PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(USE_JPEGXL PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(USE_WOFF2 PRIVATE OFF)

# Enabled features (the PlayStation port's choices).
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_FULLSCREEN_API PRIVATE ON)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_SMOOTH_SCROLLING PRIVATE ON)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_VARIATION_FONTS PRIVATE ON)

# Off for LazyOS: the PlayStation port has these on and the LazyOS one does not
# need them yet (each is a source tree to compile and a thread to start).
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_BUBBLEWRAP_SANDBOX PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_PERIODIC_MEMORY_MONITOR PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_REMOTE_INSPECTOR PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_RESOURCE_USAGE PRIVATE OFF)

# Same as the PlayStation port: not implemented, or no support planned.
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_ASYNC_SCROLLING PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_CONTEXT_MENUS PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_GEOLOCATION PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_GPU_PROCESS PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_IMAGE_DIFF PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_MATHML PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_NOTIFICATIONS PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_USER_MESSAGE_HANDLERS PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_WEBGL PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_WEB_AUDIO PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_XSLT PRIVATE OFF)

# What the brief fixes for W4.
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_VIDEO PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_WEB_RTC PRIVATE OFF)
WEBKIT_OPTION_DEFAULT_PORT_VALUE(ENABLE_JAVASCRIPT_SHELL PRIVATE OFF)

WEBKIT_OPTION_END()

SET_AND_EXPOSE_TO_BUILD(ENABLE_DEVELOPER_MODE OFF)
set(ENABLE_API_TESTS OFF)
set(ENABLE_MINIBROWSER OFF)

# Platform options.
SET_AND_EXPOSE_TO_BUILD(USE_UNIX_DOMAIN_SOCKETS ON)
SET_AND_EXPOSE_TO_BUILD(HAVE_PTHREAD_SETNAME_NP ON)
SET_AND_EXPOSE_TO_BUILD(ENABLE_OPENTYPE_MATH ON)
SET_AND_EXPOSE_TO_BUILD(USE_INSPECTOR_SOCKET_SERVER ${ENABLE_REMOTE_INSPECTOR})

if (ENABLE_WEBCORE)
    SET_AND_EXPOSE_TO_BUILD(USE_CURL ON)
    SET_AND_EXPOSE_TO_BUILD(USE_HARFBUZZ ON)
    SET_AND_EXPOSE_TO_BUILD(USE_LIBWPE ON)
    SET_AND_EXPOSE_TO_BUILD(USE_OPENSSL ON)
    SET_AND_EXPOSE_TO_BUILD(USE_CAIRO OFF)
    SET_AND_EXPOSE_TO_BUILD(USE_FREETYPE OFF)

    # See if OpenSSL implementation is BoringSSL
    cmake_push_check_state()
    set(CMAKE_REQUIRED_INCLUDES "${OPENSSL_INCLUDE_DIR}")
    set(CMAKE_REQUIRED_LIBRARIES "${OPENSSL_LIBRARIES}")
    WEBKIT_CHECK_HAVE_SYMBOL(USE_BORINGSSL OPENSSL_IS_BORINGSSL openssl/ssl.h)
    cmake_pop_check_state()

    # See if FreeType implementation supports WOFF2 fonts
    include(WOFF2Checks)
    if (FREETYPE_WOFF2_SUPPORT_IS_AVAILABLE)
        SET_AND_EXPOSE_TO_BUILD(HAVE_WOFF_SUPPORT ON)
    endif ()

    # Rendering: the coordinated graphics classes are compiled (the PlayStation
    # port's drawing area is one of them) but never leave the CPU path.
    SET_AND_EXPOSE_TO_BUILD(USE_TEXTURE_MAPPER ON)
    SET_AND_EXPOSE_TO_BUILD(USE_COORDINATED_GRAPHICS ON)
    SET_AND_EXPOSE_TO_BUILD(ENABLE_SCROLLING_THREAD OFF)
endif ()

# Libraries are static; the executables are linked static-PIE by build.
set(bmalloc_LIBRARY_TYPE STATIC)
set(WTF_LIBRARY_TYPE STATIC)
set(JavaScriptCore_LIBRARY_TYPE STATIC)
set(PAL_LIBRARY_TYPE STATIC)
set(WebCore_LIBRARY_TYPE STATIC)
set(WebKit_LIBRARY_TYPE STATIC)

# Default to hidden visibility, as the other ports.
set(CMAKE_C_VISIBILITY_PRESET hidden)
set(CMAKE_CXX_VISIBILITY_PRESET hidden)
set(CMAKE_VISIBILITY_INLINES_HIDDEN ON)

check_symbol_exists(memmem string.h HAVE_MEMMEM)
if (HAVE_MEMMEM)
    add_definitions(-DHAVE_MEMMEM=1)
endif ()
