"""Per-machine defaults and the one-shot "boot this next" (docs/netboot-plan.md section 5).

`machines.json`, edited by the administrator for the static part and by the
registry for the rest:

    {"box": {"mac": "aa:bb:cc:dd:ee:ff", "channel": "dev", "timeout_ms": 5000,
             "next": {"id": "<build id>", "at": "<iso>", "attempts": 0} | null,
             "last_attempt": {"id": ..., "at": ...},     # POST .../booting
             "last_boot":    {"id": ..., "at": ...}}}    # POST .../booted

The file is read on every use, so editing it by hand needs no restart. A
machine that is not listed (an unknown MAC) gets the `dev` channel and a 10 s
menu; it can neither queue a `next` nor record a boot, since those name a
machine.

`next` is a one-shot that survives a failed try. `booting` (the picker, after
it verified the files, before the kexec) records an attempt and leaves `next`
pending, so a failed download, a refused verify or a kernel that never comes
up does not lose it. `booted` (the booted system confirms its id) records
`last_boot` and consumes `next` when the id is the pending one. A `next` with
`MAX_ATTEMPTS` attempts and no `booted` is dropped and recorded as
`last_boot: {"id", "failed": true, "at"}`, so a kernel that cannot boot costs
two tries, never a loop. The drop happens lazily, on the next
`GET /api/builds` or `booting` (`expire_failed`); that is the only write a
GET does.
"""

from __future__ import annotations


import jsonio
import names
import store as store_mod
from store import StoreError

DEFAULT_CHANNEL = "dev"
KNOWN_TIMEOUT_MS = 5000
UNKNOWN_TIMEOUT_MS = 10_000
#: Unconfirmed attempts after which a queued `next` is given up.
MAX_ATTEMPTS = 2


class Machines:
    def __init__(self, store: store_mod.Store):
        self.store = store
        self.path = store.root / "machines.json"
        self.lock = store.lock

    def all(self) -> dict:
        doc = jsonio.read_json(self.path, {})
        return {k: v for k, v in doc.items() if names.is_name(k) and isinstance(v, dict)} \
            if isinstance(doc, dict) else {}

    def find_by_mac(self, mac: str) -> tuple[str, dict] | None:
        wanted = names.normalize_mac(mac)
        for name, entry in sorted(self.all().items()):
            if wanted is not None and names.normalize_mac(entry.get("mac")) == wanted:
                return name, entry
        return None

    def require(self, name: str) -> dict:
        entry = self.all().get(name)
        if entry is None:
            raise StoreError(404, f"unknown machine {name!r}: add it (name, mac, channel) to machines.json")
        return entry

    def _update(self, name: str, change) -> dict:
        with self.lock:
            doc = self.all()
            if name not in doc:
                raise StoreError(404, f"unknown machine {name!r}: add it (name, mac, channel) to machines.json")
            change(doc[name])
            jsonio.write_json(self.path, doc)
            return doc[name]

    def channel_of(self, entry: dict | None) -> str:
        channel = (entry or {}).get("channel")
        return channel if names.is_name(channel) else DEFAULT_CHANNEL

    def timeout_of(self, entry: dict | None) -> int:
        if entry is None:
            return UNKNOWN_TIMEOUT_MS
        value = entry.get("timeout_ms")
        return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 \
            else KNOWN_TIMEOUT_MS

    def pending_ids(self) -> set[str]:
        """Builds some machine has queued as `next` (pruning leaves them alone)."""
        out = set()
        for entry in self.all().values():
            pending = entry.get("next")
            if isinstance(pending, dict) and isinstance(pending.get("id"), str):
                out.add(pending["id"])
        return out

    def pending_next(self, entry: dict | None) -> dict | None:
        """The machine's queued `next` if its build still exists: `{"id", "channel", "at"}`."""
        pending = (entry or {}).get("next")
        if not isinstance(pending, dict) or not names.is_build_id(pending.get("id")):
            return None
        channel = self.store.find_build(pending["id"])
        if channel is None:
            return None
        attempts = pending.get("attempts")
        return {"id": pending["id"], "channel": channel, "at": pending.get("at"),
                "attempts": attempts if isinstance(attempts, int) else 0}

    def boot_state(self, entry: dict | None) -> str:
        """`confirmed`, `attempted` (an attempt no later `booted` confirmed), `failed` or `none`."""
        entry = entry or {}
        attempt, boot = entry.get("last_attempt"), entry.get("last_boot")
        if isinstance(boot, dict) and boot.get("failed") and (
                not isinstance(attempt, dict) or str(boot.get("at")) >= str(attempt.get("at"))):
            return "failed"
        if not isinstance(attempt, dict):
            return "confirmed" if isinstance(boot, dict) else "none"
        if isinstance(boot, dict) and boot.get("id") == attempt.get("id") \
                and str(boot.get("at")) >= str(attempt.get("at")):
            return "confirmed"
        return "attempted"

    # -- writes ---------------------------------------------------------

    def set_next(self, name: str, build_id: str) -> dict:
        if not names.is_build_id(build_id):
            raise StoreError(400, f"bad build id: {str(build_id)[:80]!r}")
        self.require(name)
        channel = self.store.find_build(build_id)
        if channel is None:
            raise StoreError(404, f"no build {build_id} in the registry")
        record = {"id": build_id, "at": store_mod.iso(self.store.clock()), "attempts": 0}
        self._update(name, lambda m: m.update(next=record))
        return {"machine": name, "next": {**record, "channel": channel}}

    def expire_failed(self) -> list[str]:
        """Drop every `next` that used up its attempts without a `booted`; return the machines."""
        with self.lock:
            doc, dropped = self.all(), []
            for name, entry in doc.items():
                pending = entry.get("next")
                if isinstance(pending, dict) and isinstance(pending.get("attempts"), int)                         and pending["attempts"] >= MAX_ATTEMPTS:
                    entry["next"] = None
                    entry["last_boot"] = {"id": pending.get("id"), "failed": True,
                                          "at": store_mod.iso(self.store.clock())}
                    dropped.append(name)
            if dropped:
                jsonio.write_json(self.path, doc)
            return dropped

    def booting(self, name: str, build_id: str) -> dict:
        """The picker is about to boot `build_id`: record the attempt; `next` stays pending."""
        if not names.is_build_id(build_id):
            raise StoreError(400, f"bad build id: {str(build_id)[:80]!r}")
        self.require(name)
        channel = self.store.find_build(build_id)
        if channel is None:
            raise StoreError(404, f"no build {build_id} in the registry")
        self.expire_failed()
        now = store_mod.iso(self.store.clock())
        state = {}

        def change(machine: dict) -> None:
            machine["last_attempt"] = {"id": build_id, "at": now}
            pending = machine.get("next")
            if isinstance(pending, dict) and pending.get("id") == build_id:
                pending["attempts"] = (pending["attempts"] if isinstance(pending.get("attempts"), int)
                                       else 0) + 1
                pending["last_attempt_at"] = now
                state["attempts"] = pending["attempts"]

        self._update(name, change)
        return {"machine": name, "id": build_id, "channel": channel,
                "next_attempts": state.get("attempts")}

    def booted(self, name: str, build_id: str) -> dict:
        """The booted system confirms it runs `build_id`; that consumes a pending `next` of the same id."""
        if not names.is_build_id(build_id):
            raise StoreError(400, f"bad build id: {str(build_id)[:80]!r}")
        self.require(name)
        record = {"id": build_id, "at": store_mod.iso(self.store.clock())}
        state = {}

        def change(machine: dict) -> None:
            machine["last_boot"] = record
            pending = machine.get("next")
            if isinstance(pending, dict) and pending.get("id") == build_id:
                machine["next"] = None
                state["consumed"] = True

        self._update(name, change)
        return {"machine": name, "last_boot": record, "consumed_next": state.get("consumed", False)}

    # -- the listing ----------------------------------------------------

    def describe(self, name: str | None, entry: dict | None) -> dict:
        return {"name": name, "known": entry is not None, "mac": (entry or {}).get("mac"),
                "channel": self.channel_of(entry), "timeout_ms": self.timeout_of(entry),
                "next": self.pending_next(entry), "last_attempt": (entry or {}).get("last_attempt"),
                "last_boot": (entry or {}).get("last_boot"), "boot_state": self.boot_state(entry)}

    def listing(self, mac: str | None) -> dict:
        """The body of `GET /api/builds?mac=..`: read-only, nothing consumed or recorded."""
        self.expire_failed()
        found = self.find_by_mac(mac) if mac else None
        machine = self.describe(*(found if found else (None, None)))
        channels = []
        for channel in self.store.channel_names():
            keep, pinned = self.store.settings(channel)
            channels.append({"name": channel, "keep": keep, "pinned": pinned,
                             "builds": self.store.builds(channel)})
        mine = next((c for c in channels if c["name"] == machine["channel"]), None)
        newest = mine["builds"][0] if mine and mine["builds"] else None
        default = machine["next"] or ({"id": newest["id"], "channel": newest["channel"]} if newest else None)
        return {"registry": 1, "machine": machine, "channel": machine["channel"],
                "timeout_ms": machine["timeout_ms"], "default": default, "next": machine["next"],
                "channels": channels}
