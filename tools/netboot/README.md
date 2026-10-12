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
Python with no side-channel care, and the registry never holds the signing
seed. The one secret the registry does hold is each machine's `callback_key`
in `machines.json` (minted by `registry.py --machine-key`, used to verify
`booting` and `booted`): whoever reads the volume or its backup can forge
those two callbacks, so protect both like any credential store (the volume
is owned by uid 10001 and the container runs as that user). The key id is
the first 16 hex characters of SHA-256 of `key.pub`.

Put the printed entry into the registry's `keys.json`
(`/srv/lazyos/keys.json` in the container); the file is re-read on every
request, so no restart is needed:

```json
{"keys": [{"id": "0123456789abcdef", "pub": "<64 hex>", "name": "dev laptop"}]}
```

Until a key is registered the registry logs `no key may push yet` and refuses every upload,
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
| `--public-url URL` | `LAZYOS_REGISTRY_URL` | none: the optional iPXE scripts are served only when this is set, and use only it |
| `--healthcheck` | | `GET /healthz` of `--bind`, exit 0 or 1 |
| `--machine-key NAME` | | mint (or re-mint) the callback key of a machine in `machines.json`, print it once, exit (see Machines) |

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

The server bounds what a LAN client can hold: at most 64 connections are
served at once (one past that is closed at accept) and a connection idle for
30 s between bytes is dropped, so a request that is never finished does not
keep a thread. A reverse proxy in front may add its own limits; none is
required.

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

A machine is an entry of `machines.json` (re-read on every request, so a hand
edit takes effect at once). Create it with a signed call, then mint its
**callback key on the registry host**: a key is never sent over the wire, so
neither `add` nor any other route creates or returns one.

```bash
python tools/netboot/push.py --add-machine box --mac aa:bb:cc:dd:ee:ff --channel box
docker exec lazyos-registry python -I /app/registry.py --machine-key box     # bare: registry.py --root DIR --machine-key box
#   callback key for box (shown once): 5f2c...
#   put it in the boot OS's lazyos.cfg as  bootos.machine_key=5f2c...
```

Running the same command again re-mints the key (the old one stops working).
Do it while no `booting`/`booted` call is in flight: it rewrites
`machines.json` atomically, like the server does, but the two do not lock each
other.

```json
{"box": {"mac": "aa:bb:cc:dd:ee:ff", "channel": "box", "timeout_ms": 5000,
         "callback_key": "<64 hex>", "next": null}}
```

The key is 32 random bytes as hex, never listed by any `GET` and not in the
log. A machine without one accepts `booting`/`booted` only under the dev key. A MAC the file does not list gets the `dev` channel and a 10 s menu, and
cannot queue a `next` or record a boot. `next`:

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

`booting` and `booted` are authenticated, by either of

- `X-LazyOS-Machine: <name>:<HMAC-SHA256 hex>` over the exact body bytes, keyed
  by the machine's callback key (what the boot OS sends; it cannot hold the dev
  key), or
- `X-LazyOS-Signature` with the dev key, like the other writes (for
  `push.py --follow`).

The body is `{"action": "booting"|"booted", "nonce", "ts", "machine", "id"}`
with the same nonce list and 300 s clock skew as the other writes; `machine`
must equal the path's and `id` must be a build the registry holds. By hand:
`push.py --machine box --booting ID --machine-key HEX` (or `$LAZYOS_MACHINE_KEY`).

## 3. The API

| Method and path | Auth | What |
|---|---|---|
| `GET /healthz` | | `{"ok", "builds", "free_bytes"}` |
| `GET /api/builds?mac=..` | | **may write `machines.json`** (the lazy drop of a failed `next`, below; nothing else). Every channel with its builds (newest first; manifest parsed and verbatim as `manifest_text`, `sig`, `size`, `pinned`), this machine (`name`, `known`, `channel`, `timeout_ms`, `next`, `last_attempt`, `last_boot`, `boot_state`), `default` (the `next`, else the machine's channel's newest). |
| `GET /files/<channel>/<id>/<name>` | | the file; `Content-Length`, `Range` (206, 416), `HEAD` |
| `PUT /api/builds/<channel>/<id>/<name>` | | a file into `<id>.incoming/` (needs `Content-Length`); refused for an id that exists. `manifest.json` and `manifest.sig` must be the first two files: the pair is verified against `keys.json` on arrival (and the upload deleted if it does not verify), 403 for any other file before that, then only a name the manifest lists (400) with exactly the size it lists (413) |
| `POST /api/builds/<channel>/<id>/commit` | signed | verifies `manifest.sig` against `keys.json`, the manifest's id, and every file's size and SHA-256, then renames the directory into place; on any failure deletes the upload |
| `DELETE /api/builds/<channel>/<id>` | signed | refused (409) for a pinned build |
| `POST /api/channels/<channel>/pin\|unpin` | signed | body field `id` |
| `POST /api/channels/<channel>/prune` | signed | keeps the newest `keep` plus pinned and queued builds; optional body field `keep` (stored for the channel) |
| `POST /api/machines/<name>/next` | signed | body field `id` |
| `POST /api/machines/<name>/add` | signed | body `mac`, `channel` (default `dev`), `timeout_ms` (optional); creates the machine **without** a callback key and returns none (409 if the name or the MAC exists) |
| `POST /api/machines/<name>/booting`, `.../booted` | machine HMAC or dev key | body `{"id": ..}` plus the signed fields (see Machines) |
| `GET /boot.ipxe?mac=..`, `GET /builds/<channel>/<id>.ipxe` | | the optional iPXE menu and per-build script, served only when `--public-url` is set (otherwise 404, "iPXE scripts need --public-url (LAZYOS_REGISTRY_URL)"); every URL in them is built from it, never from the request's `Host`. iPXE boots unauthenticated code over plain HTTP by design (plan decision 1, risk 11): a lab-network option, not the signed path. A GET changes nothing, so an iPXE boot leaves a queued `next` pending; the picker is the path with the one-shot |

Names (channel, machine, file) match `[A-Za-z0-9._-]` with no leading dot, ids
match `^\d{8}-\d{6}-[0-9a-f]{8}(-dirty)?$`; anything else, `..` and absolute
names included, is refused before it can name a path.

### Signed writes

The body is a JSON object with `"action"` (the route's name: `commit`,
`delete`, `pin`, `unpin`, `prune`, `next`, `add`, `booting`, `booted`),
`"nonce"` (32 random hex characters), `"ts"` (Unix seconds) and the action's
own fields; the header `X-LazyOS-Signature: <keyid>:<128 hex>` is the Ed25519
signature over the exact body bytes. The registry refuses an unknown key id, a
bad signature, a `ts` more than 300 s from its clock and a nonce it has seen
(kept in `nonces.json`, so a restart forgets none). The fields that name the
target are **mandatory** and must equal the path's, so a captured request
cannot be pointed elsewhere: `channel` and `id` for `commit` and `delete`,
`channel` for `pin`, `unpin` and `prune`, `machine` for `next`, `add`,
`booting` and `booted`. `push.py` sends them.

The `PUT` of build files needs no signature, but an upload is opened by its
signed manifest: nothing is stored until `manifest.json` and `manifest.sig`
verify against `keys.json`, and then only the files that manifest lists, at
the sizes it lists. A host without a registered key therefore cannot make the
registry store more than a few KiB, and a build still appears only through a
signed `commit` that re-hashes everything. An unfinished upload is swept after
a day. This is meant for a trusted home network; the boot OS verifies
`manifest.sig` itself before it kexecs anything, so the registry is storage,
not a trust anchor.

## 4. Pushing

```bash
LAZYOS_REGISTRY=http://192.168.1.5:8420 python tools/netboot/push.py            # target/netboot -> dev
python tools/netboot/push.py --channel box --machine box     # and queue it for the box
python tools/netboot/push.py --machine box --id 20261012-093000-2eeee936   # roll back to an old build
python tools/netboot/push.py --list
python tools/netboot/push.py --pin ID | --unpin ID | --prune [--keep N] | --delete ID
python tools/netboot/push.py --add-machine box --mac aa:bb:cc:dd:ee:ff [--channel box]   # then mint its key on the registry host
```

The registry URL is `--registry`, `$LAZYOS_REGISTRY` or `registry.local.json`
beside this file (`{"registry": "http://HOST:8420"}`, git-ignored). `push.py`
checks what the registry will check (signature, sizes, digests) before it
uploads anything, sends `manifest.json` and `manifest.sig` first, streams the
files, then the signed commit. `--reboot` and `--follow` arrive with phase N4.
