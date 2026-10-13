# WTF on LazyOS: Linux (the Linux ABI of docs/linux-abi-plan.md) with the
# generic run loop, the same file list as the JSCOnly port that `jsc` and
# `jsc-jit` are built from (docs W2), plus what the PlayStation port adds for
# WebKit2: UnixFileDescriptor.h and the generic work queue.
#
# Unlike PlatformPlayStation.cmake there is no playstation/ directory here:
# FileSystemPlayStation, LanguagePlayStation, OSAllocatorPlayStation and
# UniStdExtrasPlayStation are replaced by the posix and unix files, which is
# what a Linux kernel serves.

list(APPEND WTF_SOURCES
    generic/MainThreadGeneric.cpp
    generic/RunLoopGeneric.cpp
    generic/WorkQueueGeneric.cpp

    linux/CurrentProcessMemoryStatus.cpp
    linux/HighPriorityThreads.cpp
    linux/MemoryFootprintLinux.cpp

    posix/CPUTimePOSIX.cpp
    posix/FileHandlePOSIX.cpp
    posix/FileSystemPOSIX.cpp
    posix/MappedFileDataPOSIX.cpp
    posix/OSAllocatorPOSIX.cpp
    posix/ThreadingPOSIX.cpp

    text/unix/TextBreakIteratorInternalICUUnix.cpp

    unix/LanguageUnix.cpp
    unix/LoggingUnix.cpp
    unix/MemoryPressureHandlerUnix.cpp
    unix/UniStdExtrasUnix.cpp
)

list(APPEND WTF_PUBLIC_HEADERS
    linux/CurrentProcessMemoryStatus.h
    linux/HighPriorityThreads.h
    linux/ProcessMemoryFootprint.h

    posix/SocketPOSIX.h

    unix/UnixFileDescriptor.h
)

list(APPEND WTF_LIBRARIES
    Threads::Threads
)
