#!/usr/bin/env python3
"""Compare two versions of the Messenger IDL and report non-append-only changes.

The interface id a client and a server check at bind time is the FNV-1a hash
of the `.vN` *name* (`midlc_model.Interface.id`): it does not cover methods,
fields or types, so it cannot tell a package manager whether a newer provider
is compatible with an older client. This script is that test (docs/update-plan.md
decision 18): within one `.vN`, only appending is allowed (docs/midl.md,
"Compatibility rules").

    python tools/pkg/idl_compat.py OLD_REF [NEW_REF]      # git refs; NEW_REF defaults to the work tree
    python tools/pkg/idl_compat.py --history [--since REV] # every commit that touched idl/, against its parent

Exit status is 1 when a breaking change is found.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "midlc"))

from midlc_lexer import lex  # noqa: E402
from midlc_parser import Parser  # noqa: E402


def git(*args: str) -> str:
    done = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, encoding="utf-8")
    if done.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {done.stderr.strip()}")
    return done.stdout


def midl_paths(ref: str | None) -> list[str]:
    if ref is None:
        return sorted(p.relative_to(ROOT).as_posix() for p in (ROOT / "idl").glob("*.midl"))
    out = git("ls-tree", "-r", "--name-only", ref, "--", "idl")
    return [p for p in out.splitlines() if p.endswith(".midl") and "/conformance/" not in p]


def read(ref: str | None, path: str) -> str:
    if ref is None:
        return (ROOT / path).read_text(encoding="utf-8")
    return git("show", f"{ref}:{path}")


def schema_of_text(text: str) -> dict[str, dict]:
    """interface name -> its methods, structs, enums, topics and rings as plain data."""
    out: dict[str, dict] = {}
    for itf in Parser(lex(text)).parse_interfaces():
        def fields(params):
            return {p.id: (p.name, str(p.ty), p.ty.objects) for p in params}

        out[itf.name] = {
            "methods": {
                m.name: {
                    "id": m.method_id,
                    "oneway": m.oneway,
                    "params": fields(m.params),
                    "returns": fields(m.returns),
                    # The kernel refuses a request whose object list is not
                    # exactly the declared one, so any change to it breaks.
                    "objects": [(o.kind, o.interface, o.dotted) for o in m.objects],
                }
                for m in itf.methods
            },
            "structs": {s.name: fields(s.fields) for s in itf.structs},
            "enums": {e.name: list(e.variants) for e in itf.enums},
            "topics": {
                # Wildcard names are documentation; their kinds are the wire.
                t.name: (t.payload, t.qos, t.retained, tuple(p.kind for p in t.params))
                for t in itf.topics
            },
            "rings": {r.name: (r.layout, r.producer, r.doorbell, r.advance) for r in itf.rings},
        }
    return out


def schema(ref: str | None) -> dict[str, dict]:
    """The schema of every `idl/*.midl` at `ref` (the work tree for None)."""
    out: dict[str, dict] = {}
    for path in midl_paths(ref):
        try:
            out.update(schema_of_text(read(ref, path)))
        except Exception as error:  # an older tree may not parse with today's midlc
            out[f"<unparsed {path}>"] = {"error": str(error)}
    return out


def diff_fields(where: str, old: dict, new: dict) -> list[str]:
    """Breaking differences between two field tables (id -> (name, type, objects)).

    Appending is compatible, except a field that carries a kernel object:
    that changes the request's object list. A pure rename (same id and type)
    is wire-compatible and not reported; a *swap* is, because each name now
    means the other field.
    """
    problems = []
    new_ids = {name: fid for fid, (name, _, _) in new.items()}
    for fid, (name, ty, _) in old.items():
        if fid not in new:
            if name in new_ids:
                problems.append(f"{where}: field {name} moved from id {fid} to id {new_ids[name]}")
            else:
                problems.append(f"{where}: field {name} (id {fid}) removed")
        elif new[fid][1] != ty:
            problems.append(f"{where}: field {name} (id {fid}) retyped {ty} -> {new[fid][1]}")
        elif new[fid][0] != name and name in new_ids:
            problems.append(f"{where}: field {name} (id {fid}) swapped with {new[fid][0]}: the names now mean the other field")
    for fid, (name, ty, objects) in new.items():
        if fid not in old and objects:
            problems.append(f"{where}: appended field {name} ({ty}) carries a kernel object")
    return problems


def compare(old: dict[str, dict], new: dict[str, dict]) -> list[str]:
    """The non-append-only changes from `old` to `new`."""
    problems: list[str] = []
    for name, o in old.items():
        if "error" in o:
            continue
        n = new.get(name)
        if n is None:
            problems.append(f"{name}: interface removed (a new .vN is the only legal way)")
            continue
        if "error" in n:
            problems.append(f"{name}: no longer parses: {n['error']}")
            continue
        for mname, om in o["methods"].items():
            nm = n["methods"].get(mname)
            if nm is None:
                problems.append(f"{name}.{mname}: method removed")
                continue
            if nm["id"] != om["id"]:
                problems.append(f"{name}.{mname}: method id {om['id']} -> {nm['id']}")
            if nm["oneway"] != om["oneway"]:
                problems.append(f"{name}.{mname}: oneway changed")
            if nm["objects"] != om["objects"]:
                problems.append(f"{name}.{mname}: the request's object list changed {om['objects']} -> {nm['objects']}")
            problems += diff_fields(f"{name}.{mname} args", om["params"], nm["params"])
            problems += diff_fields(f"{name}.{mname} reply", om["returns"], nm["returns"])
        for sname, os_ in o["structs"].items():
            ns = n["structs"].get(sname)
            if ns is None:
                problems.append(f"{name}: struct {sname} removed")
            else:
                problems += diff_fields(f"{name} struct {sname}", os_, ns)
        for ename, ov in o["enums"].items():
            nv = n["enums"].get(ename)
            if nv is None:
                problems.append(f"{name}: enum {ename} removed")
            elif nv[: len(ov)] != ov:
                problems.append(f"{name}: enum {ename} variants {ov} -> {nv} (not an append)")
        for tname, ot in o["topics"].items():
            if n["topics"].get(tname) != ot:
                problems.append(f"{name}: topic {tname} changed {ot} -> {n['topics'].get(tname)}")
        for rname, orng in o["rings"].items():
            if n["rings"].get(rname) != orng:
                problems.append(f"{name}: ring {rname} changed {orng} -> {n['rings'].get(rname)}")
    return problems


def history(since: str | None) -> int:
    rng = [f"{since}..HEAD"] if since else ["HEAD"]
    commits = git("log", "--format=%H %s", *rng, "--", "idl/*.midl").splitlines()
    breaking = unreadable = 0
    for line in commits:
        sha, _, subject = line.partition(" ")
        parent = f"{sha}^"
        try:
            old, new = schema(parent), schema(sha)
        except RuntimeError:
            continue  # the first commit has no parent
        if any(k.startswith("<unparsed") for k in (*old, *new)):
            unreadable += 1  # an older syntax today's midlc refuses: not comparable
            continue
        problems = compare(old, new)
        if problems:
            breaking += 1
            print(f"{sha[:9]} {subject[:70]}")
            for problem in problems[:6]:
                print(f"    {problem}")
    print(
        f"\n{len(commits)} commits touched idl/*.midl: {unreadable} not comparable (older syntax), "
        f"{breaking} contained a non-append-only change"
    )
    return 1 if breaking else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("old", nargs="?")
    ap.add_argument("new", nargs="?")
    ap.add_argument("--history", action="store_true")
    ap.add_argument("--since")
    args = ap.parse_args()
    if args.history:
        return history(args.since)
    if not args.old:
        ap.error("OLD_REF or --history is required")
    problems = compare(schema(args.old), schema(args.new))
    for problem in problems:
        print(problem)
    print(f"{len(problems)} non-append-only change(s)")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary and problems:
        with open(summary, "a", encoding="utf-8") as out:
            out.write(f"### IDL: {len(problems)} non-append-only change(s) against {args.old}\n\n")
            out.writelines(f"- {problem}\n" for problem in problems)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
