#!/usr/bin/env python3
"""The attack harness's judge fails when it should (docs/architecture/process-sandbox.md): a
canned log of a perfect sandbox passes, and each way the sandbox can leak, or the harness can
lie, is a failure. Also: the judge's table, the probe's driver and the manifest agree.

    python tools/webkit/test_attack_judge.py
"""

from __future__ import annotations

import re
import sys
import tomllib
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import attackjudge as judge  # noqa: E402
import fhspaths  # noqa: E402

PROBE_SOURCE = ROOT / "tools" / "abi" / "fixtures" / "src" / "sandboxprobe.rs"
#: webd owns the engine processes (their labels are `app:os.lazy.webd/<name>`); LazyWeb+ keeps one helper table.
MANIFEST = ROOT / "xui-app" / "packages" / "webd" / "manifest.toml"
PLUS_MANIFEST = ROOT / "xui-app" / "packages" / "lazywebplus" / "manifest.toml"

#: What the kernel's label trace prints for each refused probe (`kernel/src/ipc/confine.rs`,
#: `ipc/policy.rs`): the judge's patterns must match these.
TRACE = {
    "read_resolv_conf": "file=/etc/resolv.conf access=stat",
    "read_ca_bundle": "file=/etc/ssl/certs/ca-certificates.crt access=stat",
    # What the kernel printed in a real --kernel run (2026-10-22): the probe writes below the children's
    # folder `.apps/os.lazy.webd/profiles` (P4), and the refusal names that folder.
    "write_profile": "file=/home/user/.apps/os.lazy.webd/profiles access=stat",
    "write_downloads": "file=/home/user/Downloads access=stat",
    "inet_socket": "net=inet",
    "inet_serve": "net=inet",
    "tcp_connect": "net=inet",
    "read_home_dir": "file=/home/user access=stat",
    "write_home": "file=/home/user/sandbox-breach.txt access=stat",
    "read_home_file": "file=/home/user/.profile access=stat",
    "read_ssh_key": "file=/home/user/.ssh/id_rsa access=stat",
    "read_system_binary": "file=/system/bin/busybox access=stat",
    "read_confd": "file=/conf/confd.db access=stat",
    "read_passwd": "file=/etc/passwd access=stat",
    "read_tty": "file=/dev/tty access=read",
    "write_system": "file=/system/bin/sandbox-breach access=stat",
    "write_tmp": "file=/tmp/sandbox-breach access=stat",
    "unix_connect": "file=/tmp/sandbox-probe.sock access=write",
    "exec_busybox": "spawn=process",
    "exec_native": "spawn=process",
    "fork": "spawn=process",
    "signal_parent": "signal=other",
    "resolve_display": "resolve=os.lazy.display.v1 op=2",
    "resolve_input": "resolve=os.lazy.input.v1 op=2",
    "resolve_clipboard": "resolve=os.lazy.clipboard.v1 op=2",
    "read_fonts": "file=/system/share/fonts access=stat",
}
#: The network role's own trace for the one probe that differs.
TRACE_NETWORK = {"read_home_dir": "file=/home/user access=read", "inet_serve": "net=serve"}


def perfect(mode: str = "kernel", drop=(), tweak=None, app: str = judge.DEFAULT_APP) -> str:
    """A log of a sandbox that holds. `drop` removes probe lines, `tweak(role, probe, line)` edits them."""
    lines: list[str] = []
    if mode == "kernel":
        lines += ["ATTACK:driver:START:webprocess:app:os.lazy.sandbox-web"]
    for role in judge.ROLES:
        label = judge.labels(mode, app)[role]
        lines.append(f"ATTACK:{role}:LABEL:{label}")
        for probe, expect in judge.expected(app)[role].items():
            if (role, probe) in drop:
                continue
            if expect.kind == "allowed":
                line = f"ATTACK:{role}:{probe}:ALLOWED:ok"
            elif expect.kind == "absent":
                line = f"ATTACK:{role}:{probe}:BLOCKED:38"
            else:
                what = (TRACE_NETWORK if role == "networkprocess" else {}).get(probe, TRACE[probe])
                lines.append(f"[   1.0] LABEL:DENY label={label} {what}")
                line = f"ATTACK:{role}:{probe}:BLOCKED:1"
            lines.append(tweak(role, probe, line) if tweak else line)
        lines.append(f"ATTACK:{role}:DONE")
    if mode == "kernel":
        lines.append("ATTACK:driver:START:networkprocess:app:os.lazy.sandbox-net")
        lines.append("ATTACK:driver:DONE")
    return "\n".join(lines) + "\n"


class JudgeTests(unittest.TestCase):
    def problems(self, log: str, mode: str = "kernel") -> list[str]:
        found, _ = judge.judge(log, mode)
        if mode == "kernel":
            found = judge.driver_problems(log) + found
        return found

    def test_a_sandbox_that_holds_passes_in_both_modes(self) -> None:
        for mode in ("kernel", "browser"):
            self.assertEqual(self.problems(perfect(mode), mode), [], mode)

    def test_webd_is_judged_under_its_own_labels_and_only_the_network_process_may_save_downloads(self) -> None:
        app = "os.lazy.webd"
        log = perfect("browser", app=app)
        found, _ = judge.judge(log, "browser", app)
        self.assertEqual(found, [])
        self.assertIn("app:os.lazy.webd/networkprocess", log)
        # Another package's labels (the browser's, once its children were its own) do not satisfy the run.
        self.assertTrue(judge.judge(log.replace("os.lazy.webd", "os.lazy.lazywebplus"), "browser", app)[0])
        self.assertTrue(judge.judge(log.replace("os.lazy.webd", "os.lazy.other"), "browser", app)[0])
        # Only the network process may write Downloads: the page process writing it is a breach.
        log = perfect("browser", app=app, tweak=lambda r, p, line: (
            line.replace("BLOCKED:1", "ALLOWED:ok") if (r, p) == ("webprocess", "write_downloads") else line))
        found, _ = judge.judge(log, "browser", app)
        self.assertTrue(any("webprocess.write_downloads" in f and "BREACH" in f for f in found), found)

    def test_the_profile_refusal_matches_at_any_level_of_the_childrens_folder(self) -> None:
        # The judge's pattern lagged P4's move to `profiles/` and failed a sandbox that held.
        pattern = judge.EXPECTED["webprocess"]["write_profile"].trace
        for what in ("file=/home/user/.apps access=write",
                     "file=/home/user/.apps/os.lazy.webd access=stat",
                     "file=/home/user/.apps/os.lazy.webd/profiles access=stat",
                     "file=/home/user/.apps/os.lazy.webd/profiles/browser/attack-probe access=write"):
            self.assertRegex(what, pattern)
        # Not some other app's folder.
        self.assertNotRegex("file=/home/user/.apps/os.lazy.mail access=stat", pattern)

    def test_every_probe_has_a_trace_the_patterns_match(self) -> None:
        blocked = {p for table in judge.EXPECTED.values() for p, e in table.items() if e.kind == "blocked"}
        self.assertEqual(blocked - set(TRACE), set())

    def test_an_attack_that_works_is_a_breach(self) -> None:
        for probe in ("read_ssh_key", "inet_socket", "fork", "exec_busybox", "resolve_display", "write_home"):
            log = perfect(tweak=lambda r, p, line, probe=probe: (
                line.replace("BLOCKED:1", "ALLOWED:ok") if p == probe and r == "webprocess" else line))
            found = self.problems(log)
            self.assertTrue(any(f"webprocess.{probe}" in f and "BREACH" in f for f in found), (probe, found))

    def test_a_failure_that_only_looks_like_a_refusal_is_caught(self) -> None:
        # ENOENT is what an unconfined process sees for a file that is not there.
        log = perfect(tweak=lambda r, p, line: line.replace("BLOCKED:1", "ALLOWED:2") if p == "read_ssh_key" else line)
        self.assertTrue(any("read_ssh_key" in f and "BREACH" in f for f in self.problems(log)))

    def test_a_refusal_without_the_kernels_trace_is_not_the_policys(self) -> None:
        log = "\n".join(line for line in perfect().splitlines() if "file=/home/user/.ssh/id_rsa" not in line)
        found = self.problems(log + "\n")
        self.assertTrue(any("read_ssh_key" in f and "LABEL:DENY" in f for f in found), found)
        # A denial of another label does not count either.
        other = perfect().replace("label=app:os.lazy.sandbox-web file=/home/user/.ssh/id_rsa",
                                  "label=app:os.lazy.other file=/home/user/.ssh/id_rsa")
        self.assertTrue(any("read_ssh_key" in f for f in self.problems(other)))

    def test_what_the_process_needs_must_still_work(self) -> None:
        log = perfect(tweak=lambda r, p, line: (
            line.replace("ALLOWED:ok", "BLOCKED:1") if (r, p) == ("webprocess", "read_fonts") else line))
        found = self.problems(log)
        self.assertTrue(any("webprocess.read_fonts" in f and "must still work" in f for f in found), found)
        log = perfect(tweak=lambda r, p, line: (
            line.replace("ALLOWED:ok", "BLOCKED:1") if (r, p) == ("networkprocess", "inet_socket") else line))
        self.assertTrue(any("networkprocess.inet_socket" in f for f in self.problems(log)))

    def test_an_allowed_probe_must_work_not_merely_not_be_refused(self) -> None:
        log = perfect(tweak=lambda r, p, line: (
            line.replace("ALLOWED:ok", "ALLOWED:2") if (r, p) == ("webprocess", "read_fonts") else line))
        found = self.problems(log)
        self.assertTrue(any("read_fonts" in f and "did not work" in f for f in found), found)
        # A connect that times out is still a socket made; a resolver file not written yet is
        # tolerated only in the real browser's image.
        log = perfect(tweak=lambda r, p, line: (
            line.replace("ALLOWED:ok", "ALLOWED:0") if p == "tcp_connect" else line))
        self.assertEqual(self.problems(log), [])
        log = perfect(tweak=lambda r, p, line: (
            line.replace("ALLOWED:ok", "ALLOWED:2") if p == "read_resolv_conf" and r == "networkprocess" else line))
        self.assertTrue(any("read_resolv_conf" in f for f in self.problems(log)))
        self.assertEqual(self.problems(log.replace("sandbox-web", "webd/webprocess")
                                       .replace("sandbox-net", "webd/networkprocess"), "browser"), [])

    def test_the_roles_differ(self) -> None:
        # The page process reads fonts and has no sockets; the network process the reverse.
        web, net = judge.EXPECTED["webprocess"], judge.EXPECTED["networkprocess"]
        self.assertEqual(web["read_fonts"].kind, "allowed")
        self.assertEqual(net["read_fonts"].kind, "blocked")
        self.assertEqual(web["inet_socket"].kind, "blocked")
        self.assertEqual(net["inet_socket"].kind, "allowed")
        self.assertEqual(web["write_profile"].kind, "blocked")
        self.assertEqual(net["write_profile"].kind, "allowed")

    def test_a_wrong_label_a_missing_probe_and_an_unfinished_run_fail(self) -> None:
        wrong = perfect().replace("ATTACK:webprocess:LABEL:app:os.lazy.sandbox-web",
                                  "ATTACK:webprocess:LABEL:app:os.lazy.other")
        self.assertTrue(any("ran under" in f for f in self.problems(wrong)))
        missing = perfect(drop={("webprocess", "fork")})
        self.assertTrue(any("webprocess.fork" in f and "0 times" in f for f in self.problems(missing)))
        unfinished = perfect().replace("ATTACK:networkprocess:DONE\n", "")
        self.assertTrue(any("networkprocess" in f and "DONE" in f for f in self.problems(unfinished)))
        self.assertTrue(any("never started" in f for f in self.problems("")))

    def test_a_probe_reported_twice_or_unknown_fails(self) -> None:
        log = perfect() + "ATTACK:webprocess:fork:BLOCKED:1\n"
        self.assertTrue(any("2 times" in f for f in self.problems(log)))
        log = perfect() + "ATTACK:webprocess:brand_new_probe:ALLOWED:ok\n"
        self.assertTrue(any("not in the judge's table" in f for f in self.problems(log)))

    def test_absent_calls_must_be_enosys(self) -> None:
        log = perfect(tweak=lambda r, p, line: line.replace("BLOCKED:38", "ALLOWED:ok") if p == "ptrace" else line)
        self.assertTrue(any("ptrace" in f for f in self.problems(log)))
        log = perfect(tweak=lambda r, p, line: line.replace("BLOCKED:38", "BLOCKED:1") if p == "ptrace" else line)
        self.assertTrue(any("ptrace" in f and "ENOSYS" in f for f in self.problems(log)))

    def test_a_dropped_trace_is_noticed(self) -> None:
        self.assertTrue(judge.trace_suppressed(perfect() + "LABEL:SUPPRESSED 12 lines\n"))
        self.assertFalse(judge.trace_suppressed(perfect()))

    def test_the_kernel_drivers_failure_is_named(self) -> None:
        log = perfect().replace("ATTACK:driver:DONE\n", "ATTACK:driver:FAIL:load app:os.lazy.sandbox-web: -1\n")
        self.assertTrue(any("driver" in f and "load" in f for f in judge.driver_problems(log)))
        self.assertTrue(any("did not finish" in f for f in judge.driver_problems(log)))


class AgreementTests(unittest.TestCase):
    """The probe's two rule lists are the manifest's process tables, and the judge's labels are
    the tables' names."""

    def table(self) -> dict:
        return tomllib.loads(MANIFEST.read_text(encoding="utf-8"))["processes"]

    def rust_list(self, name: str) -> list[str]:
        text = PROBE_SOURCE.read_text(encoding="utf-8")
        body = re.search(rf"const {name}: &\[&str\] = &\[(.*?)\];", text, re.S).group(1)
        return re.findall(r'"([^"]+)"', body)

    def test_the_drivers_rules_are_the_manifests(self) -> None:
        processes = self.table()
        self.assertEqual(self.rust_list("WEB_FILES"), processes["webprocess"]["files"])
        self.assertEqual(self.rust_list("NETWORK_FILES"), processes["networkprocess"]["files"])

    def test_the_tables_name_the_programs_and_the_network(self) -> None:
        processes = self.table()
        self.assertEqual(processes["webprocess"]["program"], fhspaths.program("WEBPROCESS"))
        self.assertEqual(processes["networkprocess"]["program"], fhspaths.program("NETWORKPROCESS"))
        self.assertEqual(processes["webprocess"]["network"], [])
        self.assertEqual(processes["networkprocess"]["network"], ["outbound"])
        # The page process holds no Messenger interface at all.
        self.assertEqual(processes["webprocess"]["interfaces"], [])
        self.assertEqual(processes["networkprocess"]["interfaces"], [])

    def test_a_child_label_holds_no_more_than_the_app(self) -> None:
        for path in (MANIFEST, PLUS_MANIFEST):
            self.check_children_within_the_app(tomllib.loads(path.read_text(encoding="utf-8")))

    def check_children_within_the_app(self, manifest: dict) -> None:
        app = manifest["permissions"]

        def covered(rule: str, rules: list[str]) -> bool:
            # The app's rule is the child's path or a directory above it, with the same verb.
            verb, path = rule.split(":", 1)
            return any(
                r.split(":", 1)[0] == verb
                and (path == r.split(":", 1)[1] or path.startswith(r.split(":", 1)[1].rstrip("/") + "/"))
                for r in rules)

        for name, table in manifest.get("processes", {}).items():
            for rule in table["files"]:
                self.assertTrue(covered(rule, app["files"]), f"{name}: {rule}")
            self.assertLessEqual(set(table["interfaces"]), set(app["interfaces"]), name)
            self.assertLessEqual(set(table["network"]), set(app["network"]), name)

    def test_webds_labels_are_app_slash_process(self) -> None:
        app = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))["app"]["system_name"]
        for role, label in judge.labels("browser").items():
            self.assertEqual(label, f"app:{app}/{role}")

    def test_the_page_process_may_not_read_what_the_network_process_may(self) -> None:
        processes = self.table()
        web = set(processes["webprocess"]["files"])
        net = set(processes["networkprocess"]["files"])
        self.assertEqual(web & net, set())
        # Nothing below the user's home, nothing in /system/bin, no root.
        for rule in web | net:
            self.assertNotIn(fhspaths.get("system::SYSTEM_BIN"), rule)
            self.assertNotEqual(rule.split(":", 1)[1], "/")
        self.assertTrue(all(not rule.startswith("write:") for rule in web))

    def test_only_the_network_process_writes_and_it_may_write_the_profile_and_downloads(self) -> None:
        plus = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
        got = plus["processes"]["networkprocess"]
        writes = [r for r in got["files"] if r.startswith("write:")]
        self.assertEqual(sorted(writes), ["write:$HOME/.apps/os.lazy.webd/profiles", "write:$HOME/Downloads"])
        self.assertEqual(judge.expected("os.lazy.webd")["networkprocess"]["write_downloads"].kind, "allowed")
        self.assertEqual(judge.expected("os.lazy.webd")["webprocess"]["write_downloads"].kind, "blocked")
        self.assertEqual(plus["app"]["system_name"], "os.lazy.webd")
        self.assertEqual(judge.APPS, ("os.lazy.webd",))

    def test_the_ad_blockers_updater_may_fetch_and_write_only_the_lists_folder(self) -> None:
        plus = tomllib.loads(PLUS_MANIFEST.read_text(encoding="utf-8"))
        table = plus["processes"]["listupdate"]
        self.assertEqual(table["program"], fhspaths.program("CURL"))
        self.assertEqual(table["interfaces"], [])
        self.assertEqual(table["network"], ["outbound"])
        writes = [r for r in table["files"] if r.startswith("write:")]
        self.assertEqual(writes, ["write:$HOME/.apps/os.lazy.lazywebplus/lists"])
        # No cookie jar, no Downloads: the profile's other folders are not its to touch.
        for rule in table["files"]:
            self.assertNotIn("Downloads", rule)
            self.assertNotIn("cookie", rule)
        self.assertIn("os.lazy.confd.v1", plus["permissions"]["interfaces"])


if __name__ == "__main__":
    unittest.main()
