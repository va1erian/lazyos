# LazyOS netboot registry

The homelab half of [`docs/netboot-plan.md`](../../docs/netboot-plan.md): a
small HTTP service that keeps immutable, signed LazyOS builds, tells a
machine which one to boot, and takes pushes from the dev machine. Python
standard library only (the Ed25519 verifier is pure Python, with `cryptography`
used when it happens to be installed). `spike/` is the Q0 feasibility tooling
and is separate.

| File | What |
|---|---|
| `registry.py` | entry point and HTTP server (flags, environment, `--healthcheck`) |
| `api.py`, `store.py`, `machines.py`, `auth.py` | routes, the on-disk layout and commit, per-machine state, signed requests |
| `menu.py` | the optional iPXE text (`/boot.ipxe`, `/builds/<channel>/<id>.ipxe`) |
| `push.py`, `client.py` | upload a build, list, pin, prune, delete, set a machine's next boot |
| `keygen.py`, `keys.py`, `ed25519.py` | the signing key pair and RFC 8032 |
| `Dockerfile`, `docker-compose.yml` | the container (Cosmos Cloud import) |

Tests: `python -m unittest discover -s tools/netboot` and
`bash tools/netboot/test_container.sh` (prints `SKIP` without Docker).

## 1. Keys, once

```bash
python tools/netboot/keygen.py          # target/netboot/key.sec (secret) and key.pub
python tools/netboot/push.py --register-key     # prints the keys.json entry
```

`key.sec` is the 32-byte seed as 64 hex characters (mode 0600). It signs every
build (`LAZYOS_NETBOOT_KEY=target/netboot/key.sec`, phase N1) and every write
to the registry, so it stays on the dev machine: `ed25519.py` signs in pure
Python with no side-channel care, and the registry never holds a secret.
The key id is the first 16 hex characters of SHA-256 of `key.pub`.

Put the printed entry into the registry's `keys.json`
(`/srv/lazyos/keys.json` in the container); the file is re-read on every
request, so no restart is needed:

```json
{"keys": [{"id": "0123456789abcdef", "pub": "<64 hex>", "name": "dev laptop"}]}
```

Until a key is registered the registry logs `no key may push yet` and refuses
every commit and every signed write. Reads work.

## 2. Running it

### Bare

```bash
python tools/netboot/registry.py --root /srv/lazyos --bind 0.0.0.0:8420
```

| Flag | Environment | Default |
|---|---|---|
| `--root DIR` | `LAZYOS_REGISTRY_ROOT` | `./registry` (`/srv/lazyos` in the image) |
| `--bind HOST:PORT` | `LAZYOS_REGISTRY_BIND` | `0.0.0.0:8420` |
| `--keys FILE` | `LAZYOS_REGISTRY_KEYS` | `<root>/keys.json` |
| `--keep N` | `LAZYOS_REGISTRY_KEEP` | 10 builds per channel for `prune` |
| `--log FILE\|-` | `LAZYOS_REGISTRY_LOG` | `<root>/access.log`; `-` is stdout |
| `--public-url URL` | `LAZYOS_REGISTRY_URL` | the request's `Host` (iPXE scripts only) |
| `--healthcheck` | | `GET /healthz` of `--bind`, exit 0 or 1 |

A first run on an empty root writes `channels.json`, `machines.json` and an
empty `keys.json`.

### Docker, and the Cosmos Cloud import

```bash
docker build -t lazyos-registry tools/netboot
docker run -d --name lazyos-registry -p 8420:8420 -v lazyos-registry:/srv/lazyos lazyos-registry
```

The image is `python:3-alpine` with the `*.py` files of this directory only,
runs as the unprivileged user `lazyos` (uid 10001), listens on `8420`, keeps
everything in the volume `/srv/lazyos` and has a `HEALTHCHECK` on `/healthz`.
A named volume takes the image's ownership on first use; for a bind mount run
`chown 10001:10001 DIR` once.

Cosmos Cloud: *Import* the `docker-compose.yml` beside this file (one service,
the port, a named volume, `restart: unless-stopped`), or add a container from
the image `ghcr.io/va1erian/lazyos-registry:latest` with port 8420 and a volume
on `/srv/lazyos`. The box talks to port 8420 directly: the boot OS trusts
signatures, not TLS, so a reverse proxy in front is optional. To register
the key, put `keys.json` into the volume (Cosmos's volume browser, or
`docker run --rm -i -v lazyos-registry:/srv/lazyos --entrypoint sh lazyos-registry -c 'cat > /srv/lazyos/keys.json' < keys.json`).
Set `LAZYOS_REGISTRY_LOG=-` to see the access log in Cosmos's log view. Back up
the volume: it is everything.

### Layout of the volume

```
builds/<channel>/<id>/{kernel-x86_64,ramdisk,bootx64.efi,boot.json,manifest.json,manifest.sig}
builds/<channel>/<id>.incoming/      an upload in progress; swept after 24 h
channels.json    {"dev": {"keep": 10, "pinned": ["<id>"]}}
machines.json    see below
keys.json        public keys allowed to write
nonces.json      signed-request nonces seen in the last hour (at most 10,000)
access.log       one line per request: client, request, status, bytes, seconds, MB/s
```

Builds are immutable: an id is never overwritten and lives in one channel
(pushing the same bytes again is a no-op). `manifest.json` is stored exactly as
uploaded, since its bytes are what is signed. Ids sort by time, so a channel
lists newest first.

### Machines

`machines.json` is edited by the administrator (re-read on every request):

```json
{"box": {"mac": "aa:bb:cc:dd:ee:ff", "channel": "dev", "timeout_ms": 5000}}
```

The registry adds `next`, `last_attempt` and `last_boot`. A MAC the file does
not list gets the `dev` channel and a 10 s menu, and cannot queue a `next` or
record a boot. `next`:

- `push.py --machine box` (signed) queues a build as the machine's one-shot
  default.
- `booting {"id"}` (the picker, after it verified the files and before the
  kexec) records an attempt and leaves `next` pending, so a failed download,
  a refused verify or a kernel that never comes up does not lose it.
- `booted {"id"}` (the booted system) records `last_boot` and consumes `next`
  when the id is the pending one.
- A `next` with two attempts and no `booted` is dropped on the next
  `GET /api/builds` or `booting` and recorded as `last_boot: {"id", "failed":
  true, "at"}`: a kernel that cannot boot costs two tries, never a loop.

## 3. The API

| Method and path | Auth | What |
|---|---|---|
| `GET /healthz` | | `{"ok", "builds", "free_bytes"}` |
| `GET /api/builds?mac=..` | | every channel with its builds (newest first; manifest parsed and verbatim as `manifest_text`, `sig`, `size`, `pinned`), this machine (`name`, `known`, `channel`, `timeout_ms`, `next`, `last_attempt`, `last_boot`, `boot_state`), `default` (the `next`, else the machine's channel's newest). Read-only, except that it drops a `next` that used up its attempts |
| `GET /files/<channel>/<id>/<name>` | | the file; `Content-Length`, `Range` (206, 416), `HEAD` |
| `PUT /api/builds/<channel>/<id>/<name>` | | a file into `<id>.incoming/` (needs `Content-Length`); refused for an id that exists |
| `POST /api/builds/<channel>/<id>/commit` | signed | verifies `manifest.sig` against `keys.json`, the manifest's id, and every file's size and SHA-256, then renames the directory into place; on any failure deletes the upload |
| `DELETE /api/builds/<channel>/<id>` | signed | refused (409) for a pinned build |
| `POST /api/channels/<channel>/pin\|unpin` | signed | body field `id` |
| `POST /api/channels/<channel>/prune` | signed | keeps the newest `keep` plus pinned and queued builds; optional body field `keep` (stored for the channel) |
| `POST /api/machines/<name>/next` | signed | body field `id` |
| `POST /api/machines/<name>/booting`, `.../booted` | | body `{"id": ..}` |
| `GET /boot.ipxe?mac=..`, `GET /builds/<channel>/<id>.ipxe` | | the optional iPXE menu and per-build script. A GET changes nothing, so an iPXE boot leaves a queued `next` pending; the picker is the path with the one-shot |

Names (channel, machine, file) match `[A-Za-z0-9._-]` with no leading dot, ids
match `^\d{8}-\d{6}-[0-9a-f]{8}(-dirty)?$`; anything else, `..` and absolute
names included, is refused before it can name a path.

### Signed writes

The body is a JSON object with `"action"` (the route's name: `commit`,
`delete`, `pin`, `unpin`, `prune`, `next`), `"nonce"` (32 random hex
characters), `"ts"` (Unix seconds) and the action's own fields; the header
`X-LazyOS-Signature: <keyid>:<128 hex>` is the Ed25519 signature over the exact
body bytes. The registry refuses an unknown key id, a bad signature, a `ts` more
than 300 s from its clock and a nonce it has seen (kept in `nonces.json`, so a
restart forgets none). `push.py` also repeats the path's `channel`, `id` and
`machine` in the body; a body that names a different target than its path is
refused, so a captured request cannot be pointed elsewhere.

The `PUT` of build files is unauthenticated by design: nothing becomes a build
until a signed `commit` verifies it, and an unfinished upload is swept after a
day. This is meant for a trusted home network; the boot OS verifies
`manifest.sig` itself before it kexecs anything, so the registry is storage,
not a trust anchor.

## 4. Pushing

```bash
LAZYOS_REGISTRY=http://192.168.1.5:8420 python tools/netboot/push.py            # target/netboot -> dev
python tools/netboot/push.py --channel box --machine box     # and queue it for the box
python tools/netboot/push.py --machine box --id 20261012-093000-2eeee936   # roll back to an old build
python tools/netboot/push.py --list
python tools/netboot/push.py --pin ID | --unpin ID | --prune [--keep N] | --delete ID
```

The registry URL is `--registry`, `$LAZYOS_REGISTRY` or `registry.local.json`
beside this file (`{"registry": "http://HOST:8420"}`, git-ignored). `push.py`
checks what the registry will check (signature, sizes, digests) before it
uploads anything, streams the files, sends `manifest.json` and `manifest.sig`
last, then the signed commit. `--reboot` and `--follow` arrive with phase N4.
