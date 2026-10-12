# PAL on LazyOS: unchanged from PlatformPlayStation.cmake (OpenSSL digests,
# the generic clock, the no-op sound and the kill ring).

list(APPEND PAL_SOURCES
    crypto/openssl/CryptoDigestOpenSSL.cpp

    system/ClockGeneric.cpp
    system/Sound.cpp

    text/KillRing.cpp
)

list(APPEND PAL_LIBRARIES OpenSSL::Crypto)
