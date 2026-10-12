"""The command lines: registry.py (flags, environment, --healthcheck) and keygen.py."""

import contextlib
import io
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.request
from pathlib import Path

import ed25519
import keygen
import keys
import registry

HERE = Path(__file__).resolve().parent


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class SplitBindTests(unittest.TestCase):
    def test_forms(self):
        self.assertEqual(registry.split_bind("0.0.0.0:8420"), ("0.0.0.0", 8420))
        self.assertEqual(registry.split_bind("8420"), ("0.0.0.0", 8420))
        self.assertEqual(registry.split_bind(":9000"), ("0.0.0.0", 9000))
        self.assertEqual(registry.split_bind("127.0.0.1:0"), ("127.0.0.1", 0))
        for bad in ("host:port", "1.2.3.4:70000", "1.2.3.4:"):
            with self.assertRaises(ValueError, msg=bad):
                registry.split_bind(bad)


class RegistryProcessTests(unittest.TestCase):
    """Starts `registry.py` as the container does: settings from the environment only."""

    def start(self, tmp: Path, **extra):
        port = free_port()
        env = {**os.environ, "LAZYOS_REGISTRY_BIND": f"127.0.0.1:{port}",
               "LAZYOS_REGISTRY_ROOT": str(tmp / "vol"), "LAZYOS_REGISTRY_KEEP": "4", **extra}
        proc = subprocess.Popen([sys.executable, "-I", str(HERE / "registry.py")], env=env,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        self.addCleanup(lambda: (proc.kill(), proc.wait(), proc.stdout.close()))
        for _ in range(100):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{port}/healthz", timeout=1).read()
                return proc, port, env
            except OSError:
                time.sleep(0.1)
        self.fail("the registry did not come up")

    def test_environment_configures_the_server(self):
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as tmp:
            tmp = Path(tmp)
            proc, port, env = self.start(tmp)
            vol = tmp / "vol"
            for name in ("channels.json", "machines.json", "keys.json"):
                self.assertTrue((vol / name).is_file(), name)
            doc = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{port}/healthz").read())
            self.assertTrue(doc["ok"])
            # the health check of the image: exit 0 while it runs
            check = subprocess.run([sys.executable, "-I", str(HERE / "registry.py"), "--healthcheck"],
                                   env=env, capture_output=True)
            self.assertEqual(check.returncode, 0)
            for _ in range(50):
                if "GET /healthz 200" in (vol / "access.log").read_text():
                    break
                time.sleep(0.05)
            self.assertIn("GET /healthz 200", (vol / "access.log").read_text())
            proc.kill()
            proc.wait()
            check = subprocess.run([sys.executable, "-I", str(HERE / "registry.py"), "--healthcheck"],
                                   env=env, capture_output=True)
            self.assertEqual(check.returncode, 1)

    def test_log_dash_goes_to_stdout_and_keys_path_follows_the_environment(self):
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as tmp:
            tmp = Path(tmp)
            keys_path = tmp / "elsewhere" / "keys.json"
            proc, port, _ = self.start(tmp, LAZYOS_REGISTRY_LOG="-", LAZYOS_REGISTRY_KEYS=str(keys_path))
            self.assertTrue(keys_path.is_file())
            self.assertFalse((tmp / "vol" / "keys.json").exists())
            self.assertFalse((tmp / "vol" / "access.log").exists())
            urllib.request.urlopen(f"http://127.0.0.1:{port}/api/builds").read()
            proc.kill()
            out = proc.stdout.read()
            self.assertIn("no key may push yet", out)
            self.assertIn("GET /api/builds 200", out)

    def test_unusable_bind_is_a_clean_error(self):
        out = subprocess.run([sys.executable, "-I", str(HERE / "registry.py"), "--bind", "nope",
                              "--root", tempfile.gettempdir() + "/unused-registry"],
                             capture_output=True, text=True)
        self.assertEqual(out.returncode, 2)
        self.assertIn("bad bind address", out.stderr)

    def test_keep_must_be_positive(self):
        out = subprocess.run([sys.executable, "-I", str(HERE / "registry.py"), "--keep", "0"],
                             capture_output=True, text=True)
        self.assertEqual(out.returncode, 2)


class KeygenTests(unittest.TestCase):
    def run_keygen(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = keygen.main(list(args))
        return code, out.getvalue(), err.getvalue()

    def test_writes_a_pair_prints_the_entry_and_refuses_to_overwrite(self):
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as tmp:
            code, out, _ = self.run_keygen("--out", tmp, "--name", "ci")
            self.assertEqual(code, 0)
            seed = keys.read_secret(Path(tmp) / "key.sec")
            pub = keys.read_public(Path(tmp) / "key.pub")
            self.assertEqual(ed25519.public_key(seed), pub)
            entry = json.loads(out.strip().splitlines()[-1])
            self.assertEqual(entry, {"id": keys.key_id(pub), "pub": pub.hex(), "name": "ci"})
            self.assertEqual(self.run_keygen("--out", tmp)[0], 1)
            self.assertEqual(keys.read_secret(Path(tmp) / "key.sec"), seed)      # untouched
            self.assertEqual(self.run_keygen("--out", tmp, "--force")[0], 0)
            self.assertNotEqual(keys.read_secret(Path(tmp) / "key.sec"), seed)


if __name__ == "__main__":
    unittest.main()
