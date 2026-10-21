# WebKit (WebKit2) on LazyOS. Derived from Source/WebKit/PlatformPlayStation.cmake
# of the WebKit commit the webkitgtk-2.54.1 tag points to.
#
# Same as the PlayStation port: the Unix IPC connection (SOCK_SEQPACKET,
# SCM_RIGHTS), the C API with WKView and WKRunLoop (the non-WPE-backend path),
# the PlayStation page client and web view, the classic drawing area that
# paints dirty rectangles into a ShareableBitmap and sends them to the UI
# process, and the Skia backing store that receives them.
#
# Different, and why:
#  * ProcessLauncherLazyOS.cpp (UIProcess/Launcher/ here, laid over the tree from
#    webkit/port/tree/) replaces the PlayStation
#    launcher, which calls a closed library: it forks and execs the WebProcess or
#    NetworkProcess with the socketpair's client end open, and passes
#    "<process identifier> <fd>" as the generic unix AuxiliaryProcessMain wants.
#  * The generic unix entry points (WebProcess/EntryPoint/unix, NetworkProcess/
#    EntryPoint/unix) replace the PlayStation ones, which dlopen the libraries of
#    the console's module system; everything here is one static executable.
#  * No GPU process, no media (RemoteMediaPlayerProxy, VideoLayerRemote), no
#    libwpe view backend or renderer, no gamepad, no WebDriver, no DisplayLink.

include(Headers.cmake)
include(Platform/Curl.cmake)

# The image's paths of the programs, from libs/fhs (tools/webkit/port_cmake.py passes them):
# the launcher's defaults and the smoke program's.
foreach (_lazyos_path LAZYOS_WEBPROCESS_PATH LAZYOS_NETWORKPROCESS_PATH)
    if (NOT ${_lazyos_path})
        message(FATAL_ERROR "${_lazyos_path} is not set (tools/webkit/port_cmake.py passes it from libs/fhs)")
    endif ()
endforeach ()
set(_lazyos_program_definitions
    "LAZYOS_WEBPROCESS_PATH=\"${LAZYOS_WEBPROCESS_PATH}\""
    "LAZYOS_NETWORKPROCESS_PATH=\"${LAZYOS_NETWORKPROCESS_PATH}\""
)
set_source_files_properties("${WEBKIT_DIR}/UIProcess/Launcher/ProcessLauncherLazyOS.cpp"
    PROPERTIES COMPILE_DEFINITIONS "${_lazyos_program_definitions}")

WEBKIT_ADD_TARGET_CXX_FLAGS(WebKit -Wno-unused-lambda-capture)

list(APPEND WebProcess_SOURCES
    WebProcess/EntryPoint/unix/WebProcessMain.cpp
)
list(APPEND WebProcess_PRIVATE_LIBRARIES
    OpenSSL::Crypto
)

list(APPEND NetworkProcess_SOURCES
    NetworkProcess/EntryPoint/unix/NetworkProcessMain.cpp
)
list(APPEND NetworkProcess_PRIVATE_LIBRARIES
    OpenSSL::Crypto
)

list(APPEND WebKit_SOURCES
    NetworkProcess/NetworkDataTaskDataURL.cpp

    NetworkProcess/Classifier/WebResourceLoadStatisticsStore.cpp

    Platform/IPC/unix/ArgumentCodersUnix.cpp
    Platform/IPC/unix/ConnectionUnix.cpp
    Platform/IPC/unix/IPCSemaphoreUnix.cpp
    Platform/IPC/unix/IPCUtilitiesUnix.cpp

    Platform/classifier/ResourceLoadStatisticsClassifier.cpp

    Platform/unix/LoggingUnix.cpp
    Platform/unix/ModuleUnix.cpp

    Shared/API/c/playstation/WKEventPlayStation.cpp

    Shared/libwpe/NativeWebKeyboardEventLibWPE.cpp
    Shared/libwpe/NativeWebMouseEventLibWPE.cpp
    Shared/libwpe/NativeWebTouchEventLibWPE.cpp
    Shared/libwpe/NativeWebWheelEventLibWPE.cpp
    Shared/libwpe/WebEventFactory.cpp

    Shared/unix/AuxiliaryProcessMain.cpp

    UIProcess/DefaultUndoController.cpp
    UIProcess/LegacySessionStateCodingNone.cpp
    UIProcess/WebGrammarDetail.cpp
    UIProcess/WebMemoryPressureHandler.cpp
    UIProcess/WebViewportAttributes.cpp

    UIProcess/API/C/WKUserScriptRef.cpp
    UIProcess/API/C/WKViewportAttributes.cpp

    UIProcess/API/C/playstation/WKContextConfigurationPlayStation.cpp
    UIProcess/API/C/playstation/WKPagePrivatePlayStation.cpp
    UIProcess/API/C/playstation/WKRunLoop.cpp
    UIProcess/API/C/playstation/WKView.cpp

    UIProcess/CoordinatedGraphics/DrawingAreaProxyCoordinatedGraphics.cpp

    UIProcess/WebsiteData/playstation/WebsiteDataStorePlayStation.cpp

    UIProcess/linux/MemoryPressureMonitor.cpp

    UIProcess/API/C/lazyos/WPELoaderLazyOS.cpp

    UIProcess/libwpe/WebPasteboardProxyLibWPE.cpp

    UIProcess/playstation/PageClientImpl.cpp
    UIProcess/playstation/PlayStationWebView.cpp
    UIProcess/playstation/WebPageProxyPlayStation.cpp
    UIProcess/playstation/WebPreferencesPlayStation.cpp
    UIProcess/playstation/WebProcessPoolPlayStation.cpp

    WebProcess/InjectedBundle/lazyos/InjectedBundleLazyOS.cpp

    WebProcess/WebPage/CoordinatedGraphics/DrawingAreaCoordinatedGraphics.cpp

    WebProcess/WebPage/playstation/WebPagePlayStation.cpp

    WebProcess/playstation/WebProcessMainPlayStation.cpp
    WebProcess/playstation/WebProcessPlayStation.cpp

    UIProcess/Launcher/ProcessLauncherLazyOS.cpp
)

list(APPEND WebKit_PRIVATE_INCLUDE_DIRECTORIES
    "${WEBKIT_DIR}/Platform/IPC/unix"
    "${WEBKIT_DIR}/Platform/classifier"
    "${WEBKIT_DIR}/Platform/generic"
    "${WEBKIT_DIR}/Shared/libwpe"
    "${WEBKIT_DIR}/UIProcess/API/C/playstation"
    "${WEBKIT_DIR}/UIProcess/API/playstation"
    "${WEBKIT_DIR}/UIProcess/CoordinatedGraphics"
    "${WEBKIT_DIR}/UIProcess/linux"
    "${WEBKIT_DIR}/UIProcess/playstation"
    "${WEBKIT_DIR}/WebProcess/WebPage/CoordinatedGraphics"
)

include(Platform/Skia.cmake)

list(APPEND WebKit_SOURCES
    UIProcess/skia/BackingStoreSkia.cpp
)

# USE_COORDINATED_GRAPHICS: the PlayStation drawing area still names these
# classes (LayerTreeHost for the composited mode a page never enters here).
list(APPEND WebKit_SOURCES
    WebProcess/WebPage/CoordinatedGraphics/AcceleratedSurfacePlayStation.cpp
    WebProcess/WebPage/CoordinatedGraphics/CompositingRunLoop.cpp
    WebProcess/WebPage/CoordinatedGraphics/CoordinatedSceneState.cpp
    WebProcess/WebPage/CoordinatedGraphics/LayerTreeHostPlayStation.cpp
    WebProcess/WebPage/CoordinatedGraphics/ThreadedCompositorPlayStation.cpp
    WebProcess/WebPage/CoordinatedGraphics/ThreadedDisplayRefreshMonitorPlayStation.cpp
)

list(APPEND WebKit_PUBLIC_FRAMEWORK_HEADERS
    Shared/API/c/playstation/WKBasePlayStation.h
    Shared/API/c/playstation/WKEventPlayStation.h

    UIProcess/API/C/playstation/WKContextConfigurationPlayStation.h
    UIProcess/API/C/playstation/WKPagePrivatePlayStation.h
    UIProcess/API/C/playstation/WKRunLoop.h
    UIProcess/API/C/playstation/WKView.h
    UIProcess/API/C/playstation/WKViewClient.h
)

if (LAZYOS_STATIC_LIBRARIES)
    list(APPEND WebKit_LIBRARIES ${LAZYOS_STATIC_LIBRARIES})
endif ()

# The headless UI process: a program that drives WebKit through the C API
# (webkit/port/smoke/, docs W4). Linked like the auxiliary processes.
add_executable(webkit-smoke "${LAZYOS_PORT_DIR}/smoke/main.cpp" "${LAZYOS_PORT_DIR}/smoke/monitor.cpp" "${LAZYOS_PORT_DIR}/smoke/cache.cpp")
# monitor.cpp tests the IPC socket monitor's own header (SocketMonitorLazyOS.h, issue #867).
target_include_directories(webkit-smoke PRIVATE "${CMAKE_BINARY_DIR}/WebKit/Headers" "${WEBKIT_DIR}/Platform/IPC/unix")
target_include_directories(webkit-smoke SYSTEM PRIVATE ${FONTCONFIG_INCLUDE_DIR} ${Fontconfig_INCLUDE_DIRS})
target_compile_definitions(webkit-smoke PRIVATE ${_lazyos_program_definitions})
target_link_libraries(webkit-smoke PRIVATE WebKit)

# The desktop UI process (webkit/port/app/, docs/lazywebplus.md): WebKit's run loop in C++, the
# window in a Rust static library. lazywebplus-app links LazyWeb+'s window (LAZYOS_PLUS_LIBRARY,
# built by tools/webkit/app.py). Without the library its target does not exist, so the W4
# programs build as before.
# The engine: views, their clients and their paint, with no window and no C ABI of its own.
set(_lazyos_engine_sources
    "${LAZYOS_PORT_DIR}/app/engine.cpp"
    "${LAZYOS_PORT_DIR}/app/clients.cpp"
    "${LAZYOS_PORT_DIR}/app/policy.cpp"
    "${LAZYOS_PORT_DIR}/app/uiclient.cpp"
    "${LAZYOS_PORT_DIR}/app/notify.cpp"
    "${LAZYOS_PORT_DIR}/app/children.cpp"
    "${LAZYOS_PORT_DIR}/app/blocking.cpp"
    "${LAZYOS_PORT_DIR}/app/paint.cpp"
    "${LAZYOS_PORT_DIR}/app/log.cpp"
)
# ... and the C ABI of lazyos_ui.h over it, for a program with a window of that kind.
set(_lazyos_app_sources
    "${LAZYOS_PORT_DIR}/app/main.cpp"
    "${LAZYOS_PORT_DIR}/app/legacy.cpp"
    ${_lazyos_engine_sources}
)

function (lazyos_add_app name library)
    add_executable(${name} ${_lazyos_app_sources})
    target_include_directories(${name} PRIVATE "${CMAKE_BINARY_DIR}/WebKit/Headers" "${LAZYOS_PORT_DIR}/app")
    target_compile_definitions(${name} PRIVATE ${_lazyos_program_definitions})
    target_link_libraries(${name} PRIVATE WebKit "${library}")
endfunction ()

if (LAZYOS_PLUS_LIBRARY)
    lazyos_add_app(lazywebplus-app "${LAZYOS_PLUS_LIBRARY}")
endif ()

# Two views at once with no window (webkit/port/views/, docs/webd-plan.md P1): the engine's second
# owner, which is also the test of its views. Built on request (`port.py --only webkit-views`).
add_executable(webkit-views "${LAZYOS_PORT_DIR}/views/main.cpp" ${_lazyos_engine_sources})
target_include_directories(webkit-views PRIVATE "${CMAKE_BINARY_DIR}/WebKit/Headers" "${LAZYOS_PORT_DIR}/app")
target_compile_definitions(webkit-views PRIVATE ${_lazyos_program_definitions})
target_link_libraries(webkit-views PRIVATE WebKit)
