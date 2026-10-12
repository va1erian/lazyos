#!/usr/bin/env python3
"""Push a netboot build to the registry (docs/netboot-plan.md section 6).

    python tools/netboot/push.py                        # target/netboot -> channel dev
    python tools/netboot/push.py --channel box --machine box     # and make it the box's next boot
    python tools/netboot/push.py --machine box --id 20261012-093000-2eeee936   # roll back: next = an old build
    python tools/netboot/push.py --list | --pin ID | --unpin ID | --prune [--keep N] | --delete ID
    python tools/netboot/push.py --register-key         # print the keys.json entry for key.pub
    python tools/netboot/push.py --add-machine box --mac aa:bb:cc:dd:ee:ff [--channel box]
    # then, on the registry host (keys are minted there, never sent):
    #   docker exec lazyos-registry python -I /app/registry.py --machine-key box
    python tools/netboot/push.py --machine box --booting ID | --booted ID [--machine-key HEX]   # what the boot OS posts

The registry is `--registry`, `$LAZYOS_REGISTRY` or the `registry` field of
`tools/netboot/registry.local.json` (git-ignored). Before uploading it checks
what the registry would: `manifest.sig` verifies against the key, and every
file's size and SHA-256 match `manifest.json`. Writes other than the upload
itself are signed with `key.sec` (see `auth.py`); the secret never leaves this
machine. The `--reboot` and `--follow` steps of the plan arrive with phase N4.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))  # `python -I` drops the script directory

import client as client_mod  # noqa: E402
import ed25519  # noqa: E402
import keys  # noqa: E402
import names  # noqa: E402

DEFAULT_DIR = HERE.parent.parent / "target" / "netboot"
LOCAL_CONFIG = HERE / "registry.local.json"
CHUNK = 1 << 20


class PushError(Exception):
    pass


def mib(n: float) -> str:
    return f"{n / (1 << 20):.1f} MiB"


def registry_url(arg: str | None) -> str:
    if arg:
        return arg
    if os.environ.get("LAZYOS_REGISTRY"):
        return os.environ["LAZYOS_REGISTRY"]
    try:
        url = json.loads(LOCAL_CONFIG.read_text()).get("registry")
        if isinstance(url, str) and url:
            return url
    except (OSError, ValueError, AttributeError):
        pass
    raise PushError("no registry: pass --registry URL, set LAZYOS_REGISTRY, or put "
                    f'{{"registry": "http://HOST:8420"}} in {LOCAL_CONFIG}')


def load_seed(directory: Path, key_arg: str | None) -> bytes:
    path = Path(key_arg) if key_arg else directory / "key.sec"
    try:
        return keys.read_secret(path)
    except ValueError as e:
        raise PushError(f"{e} (python tools/netboot/keygen.py makes one)") from e


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(CHUNK):
            digest.update(chunk)
    return digest.hexdigest()


def check_build(directory: Path, pub: bytes) -> dict:
    """Verify a `target/netboot/` directory as the registry's commit would; return its facts."""
    try:
        raw = (directory / "manifest.json").read_bytes()
    except OSError as e:
        raise PushError(f"{directory}: no manifest.json ({e.strerror}); build with LAZYOS_NETBOOT=1") from e
    try:
        manifest = json.loads(raw)
        files = manifest["files"]
        build_id = manifest["id"]
        well_formed = isinstance(build_id, str) and isinstance(files, dict) and all(
            isinstance(e, dict) and isinstance(e.get("size"), int) and not isinstance(e.get("size"), bool)
            and e["size"] >= 0 and isinstance(e.get("sha256"), str) and bool(names.HEX64.match(e["sha256"]))
            for e in files.values())
    except (ValueError, KeyError, TypeError):
        well_formed = False
    if not well_formed:
        raise PushError('manifest.json is not a netboot manifest (needs a string "id" and a "files" object '
                        'of {"size": int, "sha256": 64 hex} entries)')
    if not names.is_build_id(build_id):
        raise PushError(f"manifest id {build_id!r} is not <yyyymmdd-HHMMSS>-<rev8>[-dirty]")
    try:
        sig = keys.parse_signature((directory / "manifest.sig").read_bytes())
    except OSError:
        raise PushError(f"{directory} has no manifest.sig: the build was not signed "
                        "(set LAZYOS_NETBOOT_KEY=<key.sec>); the registry would refuse it") from None
    if sig is None:
        raise PushError("manifest.sig is not 128 hex characters and a newline")
    if not ed25519.verify(pub, raw, sig):
        raise PushError("manifest.sig does not verify against this key (signed by another key, "
                        "or manifest.json changed after signing)")
    total = 0
    for name, entry in sorted(files.items()):
        path = directory / name
        if not names.is_name(name) or not path.is_file():
            raise PushError(f"{name}: listed in the manifest but not in {directory}")
        if path.stat().st_size != entry["size"] or sha256_file(path) != entry["sha256"]:
            raise PushError(f"{name}: does not match manifest.json (rebuild, do not edit the files)")
        total += entry["size"]
    return {"id": build_id, "raw": raw, "files": sorted(files), "bytes": total}


def push_build(cli: client_mod.Client, directory: Path, pub: bytes, channel: str, say=print) -> dict:
    info = check_build(directory, pub)
    build_id = info["id"]
    existing = next((b for c in cli.listing()["channels"] for b in c["builds"] if b["id"] == build_id), None)
    if existing is not None:
        if existing["channel"] == channel and existing["manifest_text"].encode() == info["raw"]:
            say(f"{build_id} is already in channel {channel}; nothing to upload")
            return {**info, "uploaded": False}
        raise PushError(f"{build_id} already exists (channel {existing['channel']}) with other contents; "
                        "ids are never overwritten: rebuild to get a new id")
    started = time.monotonic()
    # the signed manifest opens the upload: the registry takes no file before it
    for name in ["manifest.json", "manifest.sig", *info["files"]]:
        say(f"  PUT {name} ({mib((directory / name).stat().st_size)})")
        cli.put_file(channel, build_id, name, directory / name)
    seconds = max(time.monotonic() - started, 1e-6)
    cli.commit(channel, build_id)
    say(f"pushed {build_id} to {channel} on {cli.url} "
        f"({mib(info['bytes'])} in {seconds:.1f} s, {info['bytes'] / seconds / 1e6:.1f} MB/s)")
    return {**info, "uploaded": True}


def show_list(cli: client_mod.Client, say=print) -> None:
    doc = cli.listing()
    if not doc["channels"]:
        say("the registry holds no builds")
    for channel in doc["channels"]:
        say(f"{channel['name']} (keep {channel['keep']})")
        for b in channel["builds"]:
            say(f"  {b['id']}  {mib(b['size']):>10}{'  pinned' if b['pinned'] else ''}")


def register_key_line(directory: Path, key_arg: str | None, name: str | None) -> str:
    pub_path = directory / "key.pub"
    try:
        pub = keys.read_public(pub_path)
    except ValueError:
        pub = ed25519.public_key(load_seed(directory, key_arg))
    return json.dumps(keys.keys_entry(pub, name))


def build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--registry", help="registry URL (default $LAZYOS_REGISTRY, then registry.local.json)")
    ap.add_argument("--dir", default=str(DEFAULT_DIR), help="the build directory (default %(default)s)")
    ap.add_argument("--key", help="key.sec to sign with (default <dir>/key.sec)")
    ap.add_argument("--channel", default="dev", help="channel to push to or manage (default %(default)s)")
    ap.add_argument("--machine", help="make the build the one-shot next boot of this machine")
    ap.add_argument("--id", help="with --machine and no push: an id already in the registry (roll back)")
    ap.add_argument("--list", action="store_true", help="list the registry's builds")
    ap.add_argument("--pin", metavar="ID")
    ap.add_argument("--unpin", metavar="ID")
    ap.add_argument("--prune", action="store_true", help="delete all but the newest --keep unpinned builds")
    ap.add_argument("--keep", type=int, help="with --prune: builds to keep (stored for the channel)")
    ap.add_argument("--delete", metavar="ID")
    ap.add_argument("--add-machine", metavar="NAME", help="create a machine entry; prints its callback key once")
    ap.add_argument("--mac", help="with --add-machine: the machine's MAC address")
    ap.add_argument("--timeout-ms", type=int, help="with --add-machine: the menu timeout")
    ap.add_argument("--booting", metavar="ID", help="with --machine: record a boot attempt, as the picker does")
    ap.add_argument("--booted", metavar="ID", help="with --machine: confirm a boot, as the booted system does")
    ap.add_argument("--machine-key", default=os.environ.get("LAZYOS_MACHINE_KEY"),
                    help="with --booting/--booted: sign with this callback key (default $LAZYOS_MACHINE_KEY); "
                         "without it the dev key signs")
    ap.add_argument("--register-key", action="store_true", help="print the keys.json entry for key.pub")
    ap.add_argument("--name", help="with --register-key: the entry's name (default this host)")
    return ap


def run(args, say=print) -> int:
    directory = Path(args.dir)
    if args.register_key:
        say(register_key_line(directory, args.key, args.name))
        say('add it to the "keys" list of the registry\'s keys.json')
        return 0
    cli = client_mod.Client(registry_url(args.registry))
    if args.list:
        show_list(cli, say)
        return 0
    if (args.booting or args.booted) and args.machine_key and args.machine:
        action, build_id = ("booting", args.booting) if args.booting else ("booted", args.booted)
        result = cli.callback(args.machine, action, build_id, args.machine_key)
        say(f"{args.machine}: {action} {build_id} recorded")
        return 0
    cli.seed = load_seed(directory, args.key)
    pub = ed25519.public_key(cli.seed)
    if args.add_machine:
        if not args.mac:
            raise PushError("--add-machine needs --mac")
        result = cli.add_machine(args.add_machine, args.mac, args.channel, args.timeout_ms)
        say(f"added machine {args.add_machine} ({result['mac']}, channel {result['channel']}); it has no callback key yet")
        say("mint one on the registry host (a key is never sent over the wire), e.g.:")
        say(f"  docker exec lazyos-registry python -I /app/registry.py --machine-key {args.add_machine}")
        say("then put it in the boot OS's lazyos.cfg as  bootos.machine_key=<key>")
        return 0
    if args.booting or args.booted:
        if not args.machine:
            raise PushError("--booting/--booted need --machine NAME")
        action, build_id = ("booting", args.booting) if args.booting else ("booted", args.booted)
        cli.callback(args.machine, action, build_id)
        say(f"{args.machine}: {action} {build_id} recorded")
        return 0
    if args.pin or args.unpin:
        build = args.pin or args.unpin
        cli.pin(args.channel, build, bool(args.pin))
        say(f"{'pinned' if args.pin else 'unpinned'} {build} in {args.channel}")
        return 0
    if args.prune:
        result = cli.prune(args.channel, args.keep)
        say(f"pruned {args.channel}: deleted {len(result['deleted'])}, kept {len(result['kept'])}")
        return 0
    if args.delete:
        cli.delete(args.channel, args.delete)
        say(f"deleted {args.delete} from {args.channel}")
        return 0
    if args.id:
        if not args.machine:
            raise PushError("--id needs --machine (it picks an existing build for that machine's next boot)")
        build_id = args.id
    else:
        build_id = push_build(cli, directory, pub, args.channel, say)["id"]
    if args.machine:
        cli.set_next(args.machine, build_id)
        say(f"next boot of {args.machine}: {build_id}")
    return 0


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return run(args)
    except (PushError, client_mod.RegistryError, ValueError) as e:
        print(f"push: {getattr(e, 'message', e)}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
