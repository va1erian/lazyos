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
add_executable(webkit-smoke "${LAZYOS_PORT_DIR}/smoke/main.cpp" "${LAZYOS_PORT_DIR}/smoke/monitor.cpp" "${LAZYOS_PORT_DIR}/smoke/cache.cpp" "${LAZYOS_PORT_DIR}/smoke/spawn.cpp")
# spawn.cpp is the launcher's embedder here (lzw_launcher_event) and probes what a program started at a launch inherits (issue #889).
# monitor.cpp tests the IPC socket monitor's own header (SocketMonitorLazyOS.h, issue #867).
target_include_directories(webkit-smoke PRIVATE "${CMAKE_BINARY_DIR}/WebKit/Headers" "${WEBKIT_DIR}/Platform/IPC/unix")
target_include_directories(webkit-smoke SYSTEM PRIVATE ${FONTCONFIG_INCLUDE_DIR} ${Fontconfig_INCLUDE_DIRS})
target_compile_definitions(webkit-smoke PRIVATE ${_lazyos_program_definitions})
target_link_libraries(webkit-smoke PRIVATE WebKit)

# The engine: views, their clients and their paint, with no window and no C ABI of its own (webkit-views and
# webd-app compile it; lazyos_ui.h holds the constants it shares with webd).
set(_lazyos_engine_sources
    "${LAZYOS_PORT_DIR}/app/engine.cpp"
    "${LAZYOS_PORT_DIR}/app/clients.cpp"
    "${LAZYOS_PORT_DIR}/app/policy.cpp"
    "${LAZYOS_PORT_DIR}/app/uiclient.cpp"
    "${LAZYOS_PORT_DIR}/app/notify.cpp"
    "${LAZYOS_PORT_DIR}/app/children.cpp"
    "${LAZYOS_PORT_DIR}/app/blocking.cpp"
    "${LAZYOS_PORT_DIR}/app/paint.cpp"
    "${LAZYOS_PORT_DIR}/app/light.cpp"
    "${LAZYOS_PORT_DIR}/app/log.cpp"
    "${LAZYOS_PORT_DIR}/app/profile.cpp"
    "${LAZYOS_PORT_DIR}/app/fullview.cpp"
)

# Two views at once with no window (webkit/port/views/, docs/webd-plan.md P1): the engine's second
# owner, which is also the test of its views. Built on request (`port.py --only webkit-views`).
add_executable(webkit-views "${LAZYOS_PORT_DIR}/views/main.cpp" "${LAZYOS_PORT_DIR}/views/timers.cpp" ${_lazyos_engine_sources})
# The `timers` mode tests WTF's RunLoop itself (patch 0037), with WTF's own headers and definitions.
set_source_files_properties("${LAZYOS_PORT_DIR}/views/timers.cpp" PROPERTIES
    INCLUDE_DIRECTORIES "${CMAKE_BINARY_DIR}/WTF/Headers;${CMAKE_BINARY_DIR}/bmalloc/Headers"
    COMPILE_DEFINITIONS "WTF_PLATFORM_LAZYOS=1;BUILDING_WITH_CMAKE=1;HAVE_CONFIG_H=1;PAS_BMALLOC=1;STATICALLY_LINKED_WITH_bmalloc;STATICALLY_LINKED_WITH_WTF")
target_include_directories(webkit-views PRIVATE "${CMAKE_BINARY_DIR}/WebKit/Headers" "${LAZYOS_PORT_DIR}/app")
target_compile_definitions(webkit-views PRIVATE ${_lazyos_program_definitions})
target_link_libraries(webkit-views PRIVATE WebKit)

# webd's engine half (webkit/port/webd/, docs/webd-plan.md P2): the C ABI of webd_abi.h over the engine's light
# and full views, and the headless program that plays the service to test it (`port.py --only webd-abi-test`,
# `--run-webd`).
set(_lazyos_webd_sources
    "${LAZYOS_PORT_DIR}/webd/abi.cpp"
    "${LAZYOS_PORT_DIR}/webd/abi_full.cpp"
    "${LAZYOS_PORT_DIR}/webd/sink_full.cpp"
)
add_executable(webd-abi-test
    "${LAZYOS_PORT_DIR}/webd/test/main.cpp"
    "${LAZYOS_PORT_DIR}/webd/test/support.cpp"
    "${LAZYOS_PORT_DIR}/webd/test/support_full.cpp"
    "${LAZYOS_PORT_DIR}/webd/test/server.cpp"
    "${LAZYOS_PORT_DIR}/webd/test/modes_basic.cpp"
    "${LAZYOS_PORT_DIR}/webd/test/modes_net.cpp"
    "${LAZYOS_PORT_DIR}/webd/test/modes_procs.cpp"
    "${LAZYOS_PORT_DIR}/webd/test/modes_full.cpp"
    "${LAZYOS_PORT_DIR}/webd/test/modes_page.cpp"
    "${LAZYOS_PORT_DIR}/webd/test/modes_paint.cpp"
    "${LAZYOS_PORT_DIR}/webd/test/modes_nonet.cpp"
    "${LAZYOS_PORT_DIR}/webd/test/modes_store.cpp"
    ${_lazyos_webd_sources}
    ${_lazyos_engine_sources})
target_include_directories(webd-abi-test PRIVATE "${CMAKE_BINARY_DIR}/WebKit/Headers" "${LAZYOS_PORT_DIR}/app" "${LAZYOS_PORT_DIR}/webd" "${LAZYOS_PORT_DIR}/webd/test")
target_compile_definitions(webd-abi-test PRIVATE ${_lazyos_program_definitions})
target_link_libraries(webd-abi-test PRIVATE WebKit)

# webd itself (webkit/port/webd/main.cpp, docs/webd.md): the engine, its C ABI and the service, a Rust static
# library (LAZYOS_WEBD_LIBRARY, built on the host by tools/webkit/webd.py; xui-app/webd). Without the library
# its target does not exist, so the other programs build as before. `port.py --only webd-app`.
if (LAZYOS_WEBD_LIBRARY)
    add_executable(webd-app "${LAZYOS_PORT_DIR}/webd/main.cpp" ${_lazyos_webd_sources} ${_lazyos_engine_sources})
    target_include_directories(webd-app PRIVATE "${CMAKE_BINARY_DIR}/WebKit/Headers" "${LAZYOS_PORT_DIR}/app" "${LAZYOS_PORT_DIR}/webd")
    target_compile_definitions(webd-app PRIVATE ${_lazyos_program_definitions})
    target_link_libraries(webd-app PRIVATE WebKit "${LAZYOS_WEBD_LIBRARY}")
endif ()
