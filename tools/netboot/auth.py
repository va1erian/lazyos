"""Signed write requests and the nonce list (docs/netboot-plan.md section 4, decision 4).

A signed request carries a JSON body with `"action"` (the route's name),
`"nonce"` (32 hex, random), `"ts"` (Unix seconds) and the action's own
fields, and the header

    X-LazyOS-Signature: <keyid>:<128 hex>

holding the Ed25519 signature over the exact body bytes. `verify_signed`
refuses an unknown key, a bad signature, a clock more than `SKEW` seconds off
and a nonce it has already seen. The nonce list is kept in a file, so a
restart does not forget what was used.
"""

from __future__ import annotations

import json
import os
import threading
import time
from pathlib import Path

import ed25519
import jsonio
import keys
import names

SIGNATURE_HEADER = "X-LazyOS-Signature"
#: How far a request's `ts` may be from the registry's clock, in seconds.
SKEW = 300
#: A nonce is remembered for this long and at most this many are kept.
NONCE_MEMORY_S = 3600
NONCE_MAX = 10_000
#: Signed bodies are small; this refuses a body that is not one.
MAX_BODY = 64 * 1024


class AuthError(Exception):
    """A request refused; `status` is the HTTP code (401, or 400 for a malformed body)."""

    def __init__(self, message: str, status: int = 401):
        super().__init__(message)
        self.status = status


class NonceList:
    """The nonces seen recently: `add` is False for one already present."""

    def __init__(self, path: Path | None, clock=time.time,
                 memory_s: int = NONCE_MEMORY_S, cap: int = NONCE_MAX):
        self._path, self._clock = path, clock
        self._memory_s, self._cap = memory_s, cap
        self._lock = threading.Lock()
        self._seen: dict[str, float] = {}  # nonce -> when it was seen, oldest first
        if path is not None:
            raw = jsonio.read_json(path, [])
            for item in raw if isinstance(raw, list) else []:
                if (isinstance(item, list) and len(item) == 2 and isinstance(item[0], str)
                        and isinstance(item[1], (int, float)) and not isinstance(item[1], bool)):
                    self._seen[item[0]] = float(item[1])
            self._expire(clock())

    def _expire(self, now: float) -> None:
        for nonce in [n for n, at in self._seen.items() if now - at > self._memory_s]:
            del self._seen[nonce]
        while len(self._seen) > self._cap:
            del self._seen[next(iter(self._seen))]

    def __len__(self) -> int:
        return len(self._seen)

    def add(self, nonce: str) -> bool:
        with self._lock:
            now = self._clock()
            self._expire(now)
            if nonce in self._seen:
                return False
            self._seen[nonce] = now
            self._expire(now)
            if self._path is not None:
                jsonio.write_json(self._path, [[n, at] for n, at in self._seen.items()])
            return True


def parse_header(header: str | None) -> tuple[str, bytes]:
    """`<keyid>:<128 hex>` to `(keyid, signature)`."""
    key, sep, sig_hex = (header or "").strip().partition(":")
    sig = keys.parse_signature(sig_hex) if sep else None
    if not key or sig is None:
        raise AuthError(f"missing or malformed {SIGNATURE_HEADER} (want <keyid>:<128 hex>)")
    return key, sig


def verify_signed(body: bytes, header: str | None, action: str, known_keys: dict[str, bytes],
                  nonces: NonceList, now: float) -> tuple[dict, str]:
    """Check a signed request; return `(fields, key id)` or raise `AuthError`.

    The signature is checked before anything in the body is trusted, and the
    nonce is recorded only after every other check passed, so an attacker
    cannot fill the list with nonces of requests that were refused.
    """
    key, sig = parse_header(header)
    pub = known_keys.get(key)
    if pub is None:
        raise AuthError(f"unknown key id {key!r} (is it in the registry's keys.json?)")
    if not ed25519.verify(pub, body, sig):
        raise AuthError("bad signature")
    try:
        fields = json.loads(body)
    except ValueError:
        raise AuthError("the body is not JSON", 400) from None
    if not isinstance(fields, dict):
        raise AuthError("the body is not a JSON object", 400)
    if fields.get("action") != action:
        raise AuthError(f"the signed action is {fields.get('action')!r}, this route is {action!r}", 400)
    nonce, ts = fields.get("nonce"), fields.get("ts")
    if not isinstance(nonce, str) or not names.NONCE.match(nonce):
        raise AuthError("nonce must be 32 lower-case hex characters", 400)
    if not isinstance(ts, int) or isinstance(ts, bool):
        raise AuthError("ts must be an integer (Unix seconds)", 400)
    if abs(now - ts) > SKEW:
        raise AuthError(f"ts is {int(now - ts)} s from the registry's clock (limit {SKEW} s)")
    if not nonces.add(nonce):
        raise AuthError("nonce already used (replayed request)")
    return fields, key


def sign_body(seed: bytes, action: str, fields: dict | None = None, now: float | None = None,
              nonce: str | None = None) -> tuple[bytes, str]:
    """A signed request: `(body bytes, X-LazyOS-Signature value)` (what `push.py` sends)."""
    doc = {"action": action, "nonce": nonce or os.urandom(16).hex(),
           "ts": int(time.time() if now is None else now)}
    doc.update(fields or {})
    body = json.dumps(doc, separators=(",", ":"), sort_keys=True).encode()
    return body, f"{keys.key_id(ed25519.public_key(seed))}:{ed25519.sign(seed, body).hex()}"
