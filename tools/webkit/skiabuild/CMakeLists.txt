# Builds the Skia that WebKitGTK 2.54.1 vendors (Source/ThirdParty/skia) as a
# static library, exactly as WebKit's own CMake does, without the rest of
# WebKit. Skia's CMakeLists.txt uses a handful of WebKit macros and variables
# (WebKitCompilerFlags.cmake and the port's Options*.cmake); they are stood in
# for here, nothing in the tree is edited. tools/webkit/skia.py drives it.
cmake_minimum_required(VERSION 3.20)
project(LazyOSSkia C CXX ASM)

set(WEBKIT_SOURCE_DIR "" CACHE PATH "The unpacked WebKitGTK tree")
if (NOT EXISTS "${WEBKIT_SOURCE_DIR}/Source/ThirdParty/skia/CMakeLists.txt")
    message(FATAL_ERROR "WEBKIT_SOURCE_DIR does not hold Source/ThirdParty/skia")
endif ()
list(APPEND CMAKE_MODULE_PATH "${WEBKIT_SOURCE_DIR}/Source/cmake")

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
include(CheckCXXCompilerFlag)

# WebKitCompilerFlags.cmake: add a flag to a target or source if the compiler takes it.
function(WEBKIT_ADD_COMPILER_FLAGS _compiler _kind _subject)
    foreach (_flag IN LISTS ARGN)
        string(MAKE_C_IDENTIFIER "wk_flag_${_flag}" _var)
        check_cxx_compiler_flag("${_flag}" ${_var})
        if (${_var})
            set_property(${_kind} ${_subject} APPEND PROPERTY COMPILE_OPTIONS "$<$<COMPILE_LANGUAGE:C,CXX>:${_flag}>")
        endif ()
    endforeach ()
endfunction()
macro(WEBKIT_ADD_TARGET_CXX_FLAGS _target)
    WEBKIT_ADD_COMPILER_FLAGS(CXX TARGET ${_target} ${ARGN})
endmacro()
# Whether the compiler takes all the flags together (result variable `_var`).
macro(WEBKIT_CHECK_COMPILER_FLAGS _compiler _var)
    string(REPLACE ";" " " _wk_flags "${ARGN}")
    check_cxx_compiler_flag("${_wk_flags}" ${_var})
endmacro()

# The port's choices (OptionsGTK.cmake / OptionsPlayStation.cmake): Skia with
# no libepoxy, no ANGLE, no encoders; OpenType-SVG glyphs stay on (GTK's default).
set(COMPILER_IS_CLANG ON)
set(WTF_CPU_X86_64 ON)
set(USE_LIBEPOXY OFF)
set(USE_ANGLE_EGL OFF)
set(USE_SKIA_ENCODERS OFF)
set(USE_SKIA_OPENTYPE_SVG ON)
set(Skia_FRAMEWORK_HEADERS_DIR "${CMAKE_BINARY_DIR}/Skia/Headers")

# What Skia links; the targets come from CMake's modules or WebKit's own FindHarfBuzz.
find_package(Freetype 2.9.0 REQUIRED)
find_package(Fontconfig 2.13.0 REQUIRED)
find_package(JPEG REQUIRED)
find_package(PNG REQUIRED)
find_package(EXPAT REQUIRED)
find_package(HarfBuzz REQUIRED COMPONENTS ICU)

set(SKIA_DIR "${WEBKIT_SOURCE_DIR}/Source/ThirdParty/skia")
add_subdirectory("${SKIA_DIR}" skia)

# Skia's Ganesh GL sources include <EGL/egl.h> and <GLES2/gl2.h>; WebKit ships
# the Khronos headers inside ThirdParty/ANGLE.
target_include_directories(Skia PRIVATE "${WEBKIT_SOURCE_DIR}/Source/ThirdParty/ANGLE/include")

# LazyOS has no file-backed mmap, and Skia's fontconfig font manager maps font
# files with no fallback: compile the one source that does the mmap against a
# function that copies instead (lazyos_mmap.c explains).
target_sources(Skia PRIVATE "${CMAKE_CURRENT_LIST_DIR}/lazyos_mmap.c")
set_source_files_properties("${SKIA_DIR}/src/ports/SkOSFile_posix.cpp"
    TARGET_DIRECTORY Skia PROPERTIES COMPILE_DEFINITIONS "mmap=lazyos_mmap")

# Install: the archive, the public headers laid out as WebKit sees them
# (<skia/core/SkSurface.h> and "include/core/SkSurface.h" both resolve), and
# the compile definitions a user of the headers must repeat.
install(TARGETS Skia ARCHIVE DESTINATION lib)
install(DIRECTORY "${SKIA_DIR}/include/" DESTINATION skia/include FILES_MATCHING PATTERN "*.h")
install(FILES "${SKIA_DIR}/modules/skcms/skcms.h" DESTINATION skia/modules/skcms)
install(DIRECTORY "${SKIA_DIR}/modules/svg/include/" DESTINATION skia/modules/svg/include
        FILES_MATCHING PATTERN "*.h")
install(CODE "file(MAKE_DIRECTORY \"\$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/include\")
file(CREATE_LINK \"../skia/include\" \"\$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/include/skia\" SYMBOLIC)")

get_target_property(_skia_defs Skia INTERFACE_COMPILE_DEFINITIONS)
set(_skia_flags "")
foreach (_def IN LISTS _skia_defs)
    string(APPEND _skia_flags "-D${_def}\n")
endforeach ()
file(GENERATE OUTPUT "${CMAKE_BINARY_DIR}/skia-flags.txt" CONTENT "${_skia_flags}")
install(FILES "${CMAKE_BINARY_DIR}/skia-flags.txt" DESTINATION skia)
