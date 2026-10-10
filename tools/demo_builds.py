"""The optional build steps `run_demo.py` runs before `cargo build`.

Each runs one tool script quietly and reports a failure in one line; the
explicitly requested ones (`--lazyrad`, `--doom`, `--quake`, `--emusic`, `--modplayer`,
`--pictures`, `--linuxapps`, `--tls`, `--lazyweb`, `--mail`, `--devices`) return
False so the run stops instead of booting an image without what was asked
for.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def run_tool(what: str, script: str, args: tuple[str, ...] = (), announce: bool = True) -> bool:
    """Run `tools/<script>` with `args` from the repo root; True on success."""
    if announce:
        print(f"building {what} (tools/{script})…", flush=True)
    command = [sys.executable, str(ROOT / "tools" / script), *args]
    return subprocess.run(command, cwd=ROOT, stdout=subprocess.DEVNULL).returncode == 0


def required(what: str, script: str, args: tuple[str, ...] = ()) -> bool:
    """[`run_tool`] for a step the user asked for: a failure is an error."""
    if run_tool(what, script, args):
        return True
    print(f"error: {what} did not build (run `python tools/{script}`)", file=sys.stderr)
    return False


def build_rhai() -> None:
    """Rebuild `target/rhai/rhai.elf` so the image never embeds a stale or
    missing `rhai` (issue #319). Optional: a host without the musl target
    still boots, just without the command, which `build.py` explains."""
    if not run_tool("rhai", "rhai/build.py"):
        print("warning: rhai did not build; the image will have no `rhai` command",
              file=sys.stderr)


def build_lazyrad() -> bool:
    """The LazyRAD IDE and player (`--lazyrad`)."""
    return required("LazyRAD", "lazyrad/build.py")


def build_pictures() -> bool:
    """The Picture Viewer (`--pictures`): the LazyRAD player it runs on.
    `tools/lazyrad/build.py` repackages the core packages after it, and the
    desktop build packages them again before the image, so `os.lazy.pictures`
    (the player, `lazyrad-os/samples/pictures` and the sample pictures) is
    there either way."""
    return required("the Picture Viewer (the LazyRAD player)", "lazyrad/build.py")


def build_doom() -> bool:
    """The Doom package (`tools/doom/build.py`: engine, Freedoom, then
    `target/pkg/doom.lzp`); a missing toolchain or download stops the run."""
    return required("Doom", "doom/build.py", ("--require",))


def build_quake() -> bool:
    """The Quake package (`tools/quake/build.py`: the quake-srp engine and
    its LazyOS bridge, id's shareware pak, then `target/pkg/quake.lzp`); a
    missing toolchain or download stops the run."""
    return required("Quake", "quake/build.py", ("--require",))


def build_emusic() -> bool:
    """The emusic package (`tools/emusic/build.py`: the program, then
    `target/pkg/emusic.lzp`); `EMUSIC_SRC` names a local emusic clone. A
    missing toolchain or download stops the run."""
    return required("emusic", "emusic/build.py", ("--require",))


def build_modplayer() -> bool:
    """The LazyRAD MOD player package (`target/pkg/MODPLAY.LZP` from
    `lazyrad-os/samples/modplayer`), after LazyRAD itself."""
    return required("the MOD player package", "lazyrad/package.py", ("--no-build", "--require"))


def build_linuxapps() -> bool:
    """The Linux command-line programs (dash, lua, sqlite3, jq, rg from pinned
    sources); a missing toolchain or download stops the run."""
    return required("the Linux programs", "linuxapps/build.py", ("--require",))


def build_tls() -> bool:
    """`fetch`, also run as `curl` and `wget` (rustls over pure-Rust RustCrypto,
    built by `tools/nettls/build.py`); a failed build stops the run."""
    return required("the HTTPS tools", "nettls/build.py", ("--require",))


#: LazyWeb's browser binary (`tools/xui/build.py` builds it with the other apps).
LAZYWEB_ELF = ROOT / "target" / "xui" / "xui-lazyweb.elf"


def build_lazyweb() -> bool:
    """LazyWeb (`--lazyweb`): the xui apps when its binary is missing. It is
    pure Rust (Blitz), so `tools/xui/build.py` builds it with the rest."""
    if LAZYWEB_ELF.is_file() or (build_xui_apps() and LAZYWEB_ELF.is_file()):
        return True
    print(f"error: {LAZYWEB_ELF} was not built "
          "(run `python tools/xui/build.py` and read its errors)", file=sys.stderr)
    return False


def build_xui_apps() -> bool:
    """The desktop's xui apps, which include the Devices app (`--devices`)."""
    return required("the xui apps", "xui/build.py")


def build_mail() -> bool:
    """The xui apps plus Mail (esMail with its IMAP/SMTP core and SQLite,
    built with zig; docs/mail.md) and their core packages."""
    return required("Mail", "xui/build.py", ("--mail",))
