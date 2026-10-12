#!/usr/bin/env bash
# Builds the registry image, runs it on a temporary volume with a throwaway
# key, pushes a fake payload from the host, reads it back, and checks
# /healthz, the container's own health check and that nothing runs as root.
# Prints SKIP and exits 0 where Docker is absent or its daemon is down.
#
#   bash tools/netboot/test_container.sh
set -euo pipefail

# Git Bash on Windows rewrites arguments that look like POSIX paths, which would
# turn the container path /srv/lazyos into a Windows one; keep that for docker only.
docker() { MSYS_NO_PATHCONV=1 command docker "$@"; }

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    echo "SKIP: docker is not available (no CLI or no running daemon)"
    exit 0
fi

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PY="${PYTHON:-}"
if [ -z "$PY" ]; then
    for candidate in python3 python; do
        # run it: on Windows `python3` can be a Store shim that is on PATH but is no Python
        if "$candidate" -c "import sys" >/dev/null 2>&1; then PY="$candidate"; break; fi
    done
fi
[ -n "$PY" ] || { echo "no python on PATH" >&2; exit 1; }

SUFFIX="$$-$RANDOM"
IMAGE="lazyos-registry-test:$SUFFIX"
VOLUME="lazyos-registry-test-$SUFFIX"
NAME="lazyos-registry-test-$SUFFIX"
WORK="$(mktemp -d)"

cleanup() {
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    docker volume rm -f "$VOLUME" >/dev/null 2>&1 || true
    docker rmi -f "$IMAGE" >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; docker logs "$NAME" >&2 2>&1 || true; exit 1; }

echo "== build"
CONTEXT="$HERE"
if command -v cygpath >/dev/null 2>&1; then CONTEXT="$(cygpath -m "$HERE")"; fi
docker build -q -t "$IMAGE" "$CONTEXT" >/dev/null

echo "== the image holds the server and not the tests"
docker run --rm --entrypoint sh "$IMAGE" -c 'ls /app' > "$WORK/app.txt"
grep -qx registry.py "$WORK/app.txt" || fail "registry.py is not in the image"
if grep -q '^test_' "$WORK/app.txt"; then fail "tests were copied into the image"; fi

echo "== key"
"$PY" "$HERE/keygen.py" --out "$WORK/keys" --name "container test" > "$WORK/keygen.out"
"$PY" "$HERE/push.py" --dir "$WORK/keys" --register-key | head -n 1 > "$WORK/entry.json"
"$PY" - "$WORK/entry.json" "$WORK/keys.json" <<'EOF'
import json, sys
entry = json.load(open(sys.argv[1]))
json.dump({"keys": [entry]}, open(sys.argv[2], "w"))
EOF
# register it the way an administrator would: write keys.json into the volume
docker volume create "$VOLUME" >/dev/null
docker run --rm -i -v "$VOLUME:/srv/lazyos" --entrypoint sh "$IMAGE" \
    -c 'cat > /srv/lazyos/keys.json' < "$WORK/keys.json"

echo "== run"
docker run -d --name "$NAME" -p 127.0.0.1::8420 -v "$VOLUME:/srv/lazyos" \
    --health-interval 2s --health-timeout 3s --health-start-period 1s "$IMAGE" >/dev/null
PORT="$(docker port "$NAME" 8420/tcp | head -n 1 | sed 's/.*://')"
URL="http://127.0.0.1:$PORT"

for _ in $(seq 1 60); do
    "$PY" -c "import sys, urllib.request; urllib.request.urlopen(sys.argv[1], timeout=2)" "$URL/healthz" \
        >/dev/null 2>&1 && break
    sleep 0.5
done
"$PY" -c "import sys, urllib.request; print(urllib.request.urlopen(sys.argv[1], timeout=2).read().decode())" \
    "$URL/healthz" | grep -q '"ok": true' || fail "/healthz is not green"

echo "== push a fake payload and read it back"
"$PY" - "$HERE" "$WORK" <<'EOF'
import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import fixtures, keys
seed = keys.read_secret(Path(sys.argv[2]) / "keys" / "key.sec")
build = fixtures.make_build(Path(sys.argv[2]) / "payload", seed, "20261012-093000-2eeee936")
(Path(sys.argv[2]) / "payload" / "key.sec").write_text(seed.hex() + "\n")
EOF
"$PY" "$HERE/push.py" --registry "$URL" --dir "$WORK/payload" --channel dev | tee "$WORK/push.out"
grep -q "pushed 20261012-093000-2eeee936" "$WORK/push.out" || fail "push.py did not report the id"
"$PY" - "$URL" "$WORK/payload" <<'EOF'
import hashlib, json, sys, urllib.request
from pathlib import Path
url, payload = sys.argv[1], Path(sys.argv[2])
listing = json.load(urllib.request.urlopen(url + "/api/builds"))
ids = [b["id"] for c in listing["channels"] for b in c["builds"]]
assert ids == ["20261012-093000-2eeee936"], ids
for name in ("kernel-x86_64", "ramdisk", "bootx64.efi", "boot.json"):
    got = urllib.request.urlopen(f"{url}/files/dev/20261012-093000-2eeee936/{name}").read()
    assert hashlib.sha256(got).digest() == hashlib.sha256((payload / name).read_bytes()).digest(), name
print("read back: ok")
EOF

echo "== a tampered payload is refused (by push.py, before upload; test_registry.py covers the server) and no second build appears"
rm -rf "$WORK/bad" && cp -r "$WORK/payload" "$WORK/bad"
"$PY" - "$HERE" "$WORK" <<'EOF'
import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import fixtures, keys
seed = keys.read_secret(Path(sys.argv[2]) / "keys" / "key.sec")
fixtures.make_build(Path(sys.argv[2]) / "bad", seed, "20261012-094000-2eeee936", payload_seed=7)
data = bytearray((Path(sys.argv[2]) / "bad" / "ramdisk").read_bytes())
data[10] ^= 1
(Path(sys.argv[2]) / "bad" / "ramdisk").write_bytes(bytes(data))
(Path(sys.argv[2]) / "bad" / "key.sec").write_text(seed.hex() + "\n")
EOF
if "$PY" "$HERE/push.py" --registry "$URL" --dir "$WORK/bad" >/dev/null 2>&1; then
    fail "a tampered build was accepted"
fi
"$PY" -c "import sys, urllib.request; print(urllib.request.urlopen(sys.argv[1], timeout=5).read().decode())" \
    "$URL/api/builds" > "$WORK/after.json"
if grep -q 20261012-094000 "$WORK/after.json"; then fail "the tampered build is listed"; fi

echo "== a machine is added over HTTP (no key), its key is minted inside the container"
"$PY" "$HERE/push.py" --registry "$URL" --dir "$WORK/payload" --add-machine box --mac aa:bb:cc:dd:ee:ff \
    | tee "$WORK/add.out"
if grep -q "callback key (" "$WORK/add.out"; then fail "add printed a key"; fi
docker exec "$NAME" python -I /app/registry.py --machine-key box | tee "$WORK/mint.out"
grep -Eq "bootos.machine_key=[0-9a-f]{64}" "$WORK/mint.out" || fail "the minted key is missing"

echo "== not root, healthy, and the data survives a restart"
UID_LINE="$(docker exec "$NAME" sh -c 'grep "^Uid:" /proc/1/status')"
echo "$UID_LINE"
case "$UID_LINE" in
    *"	0	"*|*" 0 "*) fail "the registry runs as root: $UID_LINE" ;;
esac
[ "$(docker exec "$NAME" id -u)" != "0" ] || fail "id -u is 0"
for _ in $(seq 1 30); do
    [ "$(docker inspect --format '{{.State.Health.Status}}' "$NAME")" = "healthy" ] && break
    sleep 1
done
[ "$(docker inspect --format '{{.State.Health.Status}}' "$NAME")" = "healthy" ] || fail "the container never became healthy"
docker restart "$NAME" >/dev/null
PORT="$(docker port "$NAME" 8420/tcp | head -n 1 | sed 's/.*://')"
for _ in $(seq 1 60); do
    "$PY" -c "import sys, urllib.request; urllib.request.urlopen(sys.argv[1], timeout=2)" "http://127.0.0.1:$PORT/healthz" \
        >/dev/null 2>&1 && break
    sleep 0.5
done
"$PY" -c "import sys, urllib.request; print(urllib.request.urlopen(sys.argv[1], timeout=5).read().decode())" \
    "http://127.0.0.1:$PORT/api/builds" | grep -q 20261012-093000-2eeee936 || fail "the build did not survive a restart"

echo "PASS: container test"
