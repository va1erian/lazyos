#!/usr/bin/env python3
"""A small FTP server over a real directory, for `tools/fuse/ftp_run.py`.

Standard library only (no pyftpdlib): passive mode, binary type, one user,
and the commands `ftpfuse` uses (`MLSD`/`LIST`, `RETR`, `STOR`, `APPE`,
`DELE`, `MKD`, `RMD`, `RNFR`/`RNTO`, `SIZE`, `NOOP`, ...). Every path is
resolved under the served directory, which is what the harness judges
afterwards: the files the guest wrote are the server's own files.

    python tools/fuse/ftpserver.py DIR --port 2121 --user lazy --password os
    python tools/fuse/ftpserver.py DIR --no-mlsd     # answer MLSD with 500
    python tools/fuse/ftpserver.py DIR --no-overwrite  # RNTO onto a file: 553

The passive address it announces is QEMU's gateway (10.0.2.2), which the guest
reaches; `ftpfuse` connects to the control peer's address anyway.
"""

from __future__ import annotations

import argparse
import os
import socket
import stat
import threading
import time
from pathlib import Path

GATEWAY = (10, 0, 2, 2)


class FtpServer:
    """Serve `root` on 127.0.0.1:`port` until `close()`."""

    def __init__(self, root: Path, port: int = 0, user: str = "lazy", password: str = "os",
                 mlsd: bool = True, overwrite: bool = True) -> None:
        self.root = root.resolve()
        self.user, self.password, self.mlsd = user, password, mlsd
        # Servers differ on whether RNTO may replace an existing file.
        self.overwrite = overwrite
        self.commands: list[tuple[str, str]] = []
        self._lock = threading.Lock()
        self._listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._listener.bind(("127.0.0.1", port))
        self._listener.listen(8)
        self.port = self._listener.getsockname()[1]
        self._stop = threading.Event()
        threading.Thread(target=self._accept, daemon=True).start()

    def close(self) -> None:
        self._stop.set()
        try:
            self._listener.close()
        except OSError:
            pass

    def verbs(self) -> list[str]:
        with self._lock:
            return [verb for verb, _ in self.commands]

    def _accept(self) -> None:
        while not self._stop.is_set():
            try:
                conn, _ = self._listener.accept()
            except OSError:
                return
            threading.Thread(target=Session(self, conn).run, daemon=True).start()

    def resolve(self, cwd: str, arg: str) -> Path | None:
        """`arg` (absolute or relative to `cwd`) under the root, or None."""
        virtual = arg if arg.startswith("/") else f"{cwd.rstrip('/')}/{arg}"
        path = (self.root / virtual.lstrip("/")).resolve()
        return path if path == self.root or self.root in path.parents else None


def _mlsd_line(path: Path) -> str:
    info = path.stat()
    kind = "dir" if stat.S_ISDIR(info.st_mode) else "file"
    modify = time.strftime("%Y%m%d%H%M%S", time.gmtime(info.st_mtime))
    return f"type={kind};size={info.st_size};modify={modify}; {path.name}\r\n"


def _list_line(path: Path) -> str:
    info = path.stat()
    kind = "d" if stat.S_ISDIR(info.st_mode) else "-"
    when = time.strftime("%b %d %H:%M", time.gmtime(info.st_mtime))
    return f"{kind}rw-r--r-- 1 lazy lazy {info.st_size:10d} {when} {path.name}\r\n"


class Session:
    def __init__(self, server: FtpServer, conn: socket.socket) -> None:
        self.server, self.conn = server, conn
        self.cwd = "/"
        self.passive: socket.socket | None = None
        self.rename_from: Path | None = None
        self.user_ok = self.logged_in = False

    def say(self, text: str) -> None:
        self.conn.sendall(text.encode("utf-8") + b"\r\n")

    def run(self) -> None:
        self.conn.settimeout(600)
        reader = self.conn.makefile("rb")
        try:
            self.say("220 lazyos fuse test ftp server")
            while True:
                raw = reader.readline(4096)
                if not raw:
                    break
                line = raw.decode("utf-8", "replace").rstrip("\r\n")
                verb, _, arg = line.partition(" ")
                verb = verb.upper()
                with self.server._lock:
                    self.server.commands.append((verb, "****" if verb == "PASS" else arg))
                if not self.dispatch(verb, arg):
                    break
        except (OSError, ValueError):
            pass
        finally:
            if self.passive is not None:
                self.passive.close()
            self.conn.close()

    def dispatch(self, verb: str, arg: str) -> bool:
        if verb == "USER":
            self.user_ok = arg == self.server.user
            self.say("331 password please")
        elif verb == "PASS":
            self.logged_in = self.user_ok and arg == self.server.password
            self.say("230 welcome" if self.logged_in else "530 login incorrect")
        elif verb == "QUIT":
            self.say("221 bye")
            return False
        elif not self.logged_in:
            self.say("530 log in first")
        elif verb in ("SYST", "TYPE", "NOOP", "PWD", "FEAT", "OPTS"):
            self.say({"SYST": "215 UNIX Type: L8", "TYPE": "200 type set", "NOOP": "200 ok",
                      "PWD": f'257 "{self.cwd}"', "FEAT": "211 no features",
                      "OPTS": "200 ok"}[verb])
        elif verb == "PASV":
            self.open_passive()
        elif verb in ("LIST", "NLST", "MLSD", "RETR", "STOR", "APPE"):
            self.transfer(verb, arg)
        else:
            self.path_command(verb, arg)
        return True

    def path_command(self, verb: str, arg: str) -> None:
        path = self.server.resolve(self.cwd, arg)
        if verb not in ("CWD", "SIZE", "DELE", "MKD", "RMD", "RNFR", "RNTO"):
            self.say("502 not implemented")
        elif path is None:
            self.say("550 outside the served tree")
        elif verb == "CWD":
            if path.is_dir():
                self.cwd = "/" + path.relative_to(self.server.root).as_posix().strip(".")
                self.say("250 directory changed")
            else:
                self.say("550 no such directory")
        elif verb == "SIZE":
            self.say(f"213 {path.stat().st_size}" if path.is_file() else "550 no such file")
        elif verb == "DELE":
            self.attempt(lambda: path.unlink(), "250 deleted")
        elif verb == "MKD":
            self.attempt(lambda: path.mkdir(), f'257 "{arg}" created')
        elif verb == "RMD":
            self.attempt(lambda: path.rmdir(), "250 removed")
        elif verb == "RNFR":
            self.rename_from = path if path.exists() else None
            self.say("350 ready for RNTO" if self.rename_from else "550 no such file")
        else:
            source, self.rename_from = self.rename_from, None
            if source is None:
                self.say("503 RNFR first")
            elif path.exists() and not self.server.overwrite:
                self.say("553 destination exists")
            else:
                self.attempt(lambda: os.replace(source, path), "250 renamed")

    def attempt(self, action, ok: str) -> None:
        try:
            action()
            self.say(ok)
        except OSError as error:
            self.say(f"550 {error.strerror or error}")

    def open_passive(self) -> None:
        if self.passive is not None:
            self.passive.close()
        self.passive = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.passive.bind(("127.0.0.1", 0))
        self.passive.listen(1)
        self.passive.settimeout(30)
        port = self.passive.getsockname()[1]
        host = ",".join(str(o) for o in GATEWAY)
        self.say(f"227 Entering Passive Mode ({host},{port >> 8},{port & 255}).")

    def transfer(self, verb: str, arg: str) -> None:
        if verb == "MLSD" and not self.server.mlsd:
            self.say("500 MLSD not understood")
            return
        if self.passive is None:
            self.say("425 use PASV first")
            return
        listener, self.passive = self.passive, None
        path = self.server.resolve(self.cwd, arg or ".")
        reading = verb in ("LIST", "NLST", "MLSD", "RETR")
        problem = None
        if path is None:
            problem = "550 outside the served tree"
        elif verb == "RETR" and not path.is_file():
            problem = "550 no such file"
        elif verb in ("LIST", "NLST", "MLSD") and not path.is_dir():
            problem = "550 no such directory"
        elif not reading and not path.parent.is_dir():
            problem = "553 no such directory"
        if problem:
            listener.close()
            self.say(problem)
            return
        self.say("150 opening data connection")
        try:
            data, _ = listener.accept()
        except OSError:
            self.say("425 no data connection")
            return
        finally:
            listener.close()
        data.settimeout(60)
        try:
            if verb == "RETR":
                data.sendall(path.read_bytes())
            elif reading:
                children = sorted(path.iterdir(), key=lambda p: p.name)
                if verb == "NLST":
                    body = "".join(f"{p.name}\r\n" for p in children)
                else:
                    line = _mlsd_line if verb == "MLSD" else _list_line
                    body = "".join(line(p) for p in children)
                data.sendall(body.encode("utf-8"))
            else:
                with open(path, "ab" if verb == "APPE" else "wb") as out:
                    while chunk := data.recv(65536):
                        out.write(chunk)
            data.shutdown(socket.SHUT_WR)
        except OSError as error:
            data.close()
            self.say(f"426 transfer aborted: {error}")
            return
        data.close()
        self.say("226 transfer complete")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("root", type=Path)
    parser.add_argument("--port", type=int, default=2121)
    parser.add_argument("--user", default="lazy")
    parser.add_argument("--password", default="os")
    parser.add_argument("--no-mlsd", action="store_true", help="refuse MLSD (clients fall back to LIST)")
    parser.add_argument("--no-overwrite", action="store_true", help="refuse RNTO onto an existing file")
    args = parser.parse_args()
    server = FtpServer(args.root, args.port, args.user, args.password, mlsd=not args.no_mlsd,
                          overwrite=not args.no_overwrite)
    print(f"serving {server.root} on 127.0.0.1:{server.port}", flush=True)
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        server.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
