"""Headless process execution: run a plan, stream output, kill process trees.

The runner is deliberately UI-agnostic: it pushes ``(kind, ...)`` messages onto
a :class:`queue.Queue` and the Tkinter event loop drains it. Output is merged
(stdout + stderr) for a single ordered stream.
"""

from __future__ import annotations

import os
import queue
import signal
import subprocess
import sys
import threading
import time


def kill_tree(proc: subprocess.Popen | None) -> None:
    """Kill a process and its children, cross-platform."""
    if proc is None or proc.poll() is not None:
        return
    try:
        if os.name == "nt":
            subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                           capture_output=True)
        else:
            os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
    except Exception:
        try:
            proc.kill()
        except Exception:
            pass


def open_path(path: str) -> None:
    """Reveal a directory in the platform file manager."""
    os.makedirs(path, exist_ok=True)
    if os.name == "nt":
        os.startfile(path)  # noqa: S606
    elif sys.platform == "darwin":
        subprocess.Popen(["open", path])
    else:
        subprocess.Popen(["xdg-open", path])


class Runner:
    """Run steps on a worker thread; messages arrive on :attr:`q`."""

    def __init__(self) -> None:
        """Create an idle runner with an empty message queue."""
        self.q: queue.Queue = queue.Queue()
        self.proc: subprocess.Popen | None = None
        self.stop_requested = False
        self.busy = False

    def start(self, steps: list[dict], env: dict[str, str], cwd: str) -> None:
        """Run ``steps`` sequentially on a daemon thread, extra ``env`` applied."""
        self.stop_requested = False
        self.busy = True
        threading.Thread(target=self._work, args=(steps, env, cwd), daemon=True).start()

    def stop(self) -> None:
        """Request cancellation and kill the current process tree."""
        self.stop_requested = True
        kill_tree(self.proc)

    def _work(self, steps: list[dict], env: dict[str, str], cwd: str) -> None:
        """Worker body: launch each step, stream its output, stop on failure."""
        full = os.environ.copy()
        # The GUI owns the LAZYOS_* build switches: drop any inherited values so
        # an unchecked box cannot be overridden by the parent environment.
        for key in [k for k in full if k.startswith("LAZYOS_")]:
            del full[key]
        full.update(env)
        kwargs: dict = {}
        if os.name == "nt":
            kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP
        else:
            kwargs["start_new_session"] = True

        for step in steps:
            if self.stop_requested:
                break
            self.q.put(("step", step["label"], " ".join(step["argv"])))
            try:
                proc = subprocess.Popen(step["argv"], cwd=cwd, env=full,
                                        stdout=subprocess.PIPE,
                                        stderr=subprocess.STDOUT,
                                        text=True, bufsize=1, **kwargs)
            except OSError as exc:
                self.q.put(("out", f"failed to launch: {exc}\n"))
                self.q.put(("done", 1))
                self.busy = False
                return
            self.proc = proc
            # A stop may have arrived between Popen returning and self.proc
            # being assigned; kill now, then keep draining until EOF so the
            # pipe cannot fill and proc.wait() cannot hang.
            if self.stop_requested:
                kill_tree(proc)
            assert proc.stdout is not None
            # Host wall-clock stamp per line (seconds since the step began): the
            # guest's own tick counter drifts when QEMU delivers timer IRQs late.
            began = time.monotonic()
            for line in proc.stdout:
                if not self.stop_requested:
                    self.q.put(("out", f"[+{time.monotonic() - began:8.3f}s] {line}"))
            proc.wait()
            self.proc = None
            code = proc.returncode
            self.q.put(("exit", code))
            if self.stop_requested or code != 0:
                self.q.put(("done", code if not self.stop_requested else 1))
                self.busy = False
                return

        self.q.put(("done", 0))
        self.busy = False
