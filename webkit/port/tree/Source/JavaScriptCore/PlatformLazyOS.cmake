# JavaScriptCore on LazyOS: the JSCOnly configuration (docs W2). The
# PlayStation file includes the socket remote inspector (ENABLE_REMOTE_INSPECTOR
# is off here), shrinks the executable memory pool to 64 MiB for the console's
# address space (LazyOS keeps the 1 GiB x86-64 default that jsc-jit ran with)
# and links its MemoryExtra library into the LLInt generators (there is none).

if (ENABLE_REMOTE_INSPECTOR)
    include(inspector/remote/Socket.cmake)
endif ()
