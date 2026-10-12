"""Ed25519 (RFC 8032, pure signatures: no prehash, empty context).

    import ed25519
    pub = ed25519.public_key(seed)       # seed: the 32-byte secret
    sig = ed25519.sign(seed, message)    # 64 bytes
    ed25519.verify(pub, message, sig)    # True or False, never raises

The registry only *verifies*; `keygen.py` and `push.py` sign with the same
module. Two implementations sit behind the three functions: the
`cryptography` package when it imports (fast, OpenSSL), else the pure-Python
reference arithmetic of RFC 8032 section 6 below, so the container needs no
compiler and no wheel. `PURE` forces the second path (the tests run both and
check that they agree).

The pure path is **not constant time** (Python integers branch on their
value): fine for verifying public data, so keep the secret seed on a trusted
dev machine and never sign on a shared or remote one. The registry holds no
secret.
"""

from __future__ import annotations

import hashlib
import os

try:  # which branch runs depends on the machine
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric import ed25519 as _fast
    FAST_AVAILABLE = True
except Exception:  # ImportError, or a broken OpenSSL binding
    FAST_AVAILABLE = False

#: Force the pure-Python path (the tests flip it to compare the two).
PURE = False

SEED_LEN = 32
PUBLIC_LEN = 32
SIG_LEN = 64

_P = 2 ** 255 - 19
_Q = 2 ** 252 + 27742317777372353535851937790883648493


def _sha512(data: bytes) -> bytes:
    return hashlib.sha512(data).digest()


def _inv(x: int) -> int:
    return pow(x, _P - 2, _P)


_D = -121665 * _inv(121666) % _P
_SQRT_M1 = pow(2, (_P - 1) // 4, _P)

# Points are extended coordinates (X, Y, Z, T) with x = X/Z, y = Y/Z, xy = T/Z.


def _add(a, b):
    a1 = (a[1] - a[0]) * (b[1] - b[0]) % _P
    b1 = (a[1] + a[0]) * (b[1] + b[0]) % _P
    c1 = 2 * a[3] * b[3] * _D % _P
    d1 = 2 * a[2] * b[2] % _P
    e, f, g, h = b1 - a1, d1 - c1, d1 + c1, b1 + a1
    return (e * f % _P, g * h % _P, f * g % _P, e * h % _P)


def _mul(s: int, point):
    result = (0, 1, 1, 0)  # the neutral element
    while s > 0:
        if s & 1:
            result = _add(result, point)
        point = _add(point, point)
        s >>= 1
    return result


def _equal(a, b) -> bool:
    return ((a[0] * b[2] - b[0] * a[2]) % _P == 0
            and (a[1] * b[2] - b[1] * a[2]) % _P == 0)


def _recover_x(y: int, sign: int):
    if y >= _P:
        return None
    x2 = (y * y - 1) * _inv(_D * y * y + 1) % _P
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (_P + 3) // 8, _P)
    if (x * x - x2) % _P != 0:
        x = x * _SQRT_M1 % _P
    if (x * x - x2) % _P != 0:
        return None
    if (x & 1) != sign:
        x = _P - x
    return x


_GY = 4 * _inv(5) % _P
_GX = _recover_x(_GY, 0)
_G = (_GX, _GY, 1, _GX * _GY % _P)


def _compress(point) -> bytes:
    zinv = _inv(point[2])
    x, y = point[0] * zinv % _P, point[1] * zinv % _P
    return int.to_bytes(y | ((x & 1) << 255), 32, "little")


def _decompress(data: bytes):
    if len(data) != 32:
        return None
    y = int.from_bytes(data, "little")
    sign = y >> 255
    y &= (1 << 255) - 1
    x = _recover_x(y, sign)
    if x is None:
        return None
    return (x, y, 1, x * y % _P)


def _expand(seed: bytes) -> tuple[int, bytes]:
    h = _sha512(seed)
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8
    a |= 1 << 254
    return a, h[32:]


def _pure_public(seed: bytes) -> bytes:
    a, _ = _expand(seed)
    return _compress(_mul(a, _G))


def _pure_sign(seed: bytes, message: bytes) -> bytes:
    a, prefix = _expand(seed)
    pub = _compress(_mul(a, _G))
    r = int.from_bytes(_sha512(prefix + message), "little") % _Q
    big_r = _compress(_mul(r, _G))
    h = int.from_bytes(_sha512(big_r + pub + message), "little") % _Q
    s = (r + h * a) % _Q
    return big_r + int.to_bytes(s, 32, "little")


def _pure_verify(pub: bytes, message: bytes, sig: bytes) -> bool:
    a_point = _decompress(pub)
    if a_point is None:
        return False
    r_point = _decompress(sig[:32])
    if r_point is None:
        return False
    s = int.from_bytes(sig[32:], "little")
    if s >= _Q:
        return False
    h = int.from_bytes(_sha512(sig[:32] + pub + message), "little") % _Q
    return _equal(_mul(s, _G), _add(r_point, _mul(h, a_point)))


def _use_fast() -> bool:
    return FAST_AVAILABLE and not PURE


def generate_seed() -> bytes:
    """A fresh random 32-byte secret seed."""
    return os.urandom(SEED_LEN)


def public_key(seed: bytes) -> bytes:
    """The 32-byte public key of a 32-byte seed."""
    if len(seed) != SEED_LEN:
        raise ValueError("an Ed25519 seed is 32 bytes")
    if _use_fast():
        key = _fast.Ed25519PrivateKey.from_private_bytes(seed)
        return key.public_key().public_bytes(serialization.Encoding.Raw,
                                             serialization.PublicFormat.Raw)
    return _pure_public(seed)


def sign(seed: bytes, message: bytes) -> bytes:
    """The 64-byte signature of `message` (deterministic)."""
    if len(seed) != SEED_LEN:
        raise ValueError("an Ed25519 seed is 32 bytes")
    if _use_fast():
        return _fast.Ed25519PrivateKey.from_private_bytes(seed).sign(message)
    return _pure_sign(seed, message)


def verify(pub: bytes, message: bytes, sig: bytes) -> bool:
    """True when `sig` is `pub`'s signature of `message`; False for anything else.

    A wrong length, a point off the curve and a non-canonical `s` are all
    just False.
    """
    if len(pub) != PUBLIC_LEN or len(sig) != SIG_LEN:
        return False
    if _use_fast():
        try:
            _fast.Ed25519PublicKey.from_public_bytes(pub).verify(sig, message)
            return True
        except (InvalidSignature, ValueError):
            return False
    return _pure_verify(pub, message, sig)
