#!/usr/bin/env bash
# Prepare a Debian/Ubuntu (Kubuntu) machine to run the LazyOS workflows on a
# self-hosted GitHub Actions runner. Run it once as the runner's user (it uses
# sudo). Safe to re-run.
set -euo pipefail

if [ "$(id -u)" -eq 0 ]; then
    echo "run as the runner's user, not root" >&2
    exit 1
fi

# Packages the workflows `apt-get install` themselves, plus build basics
# that GitHub's ubuntu-latest image ships preinstalled.
sudo apt-get update
sudo apt-get install -y \
    build-essential pkg-config curl wget git unzip zip jq ca-certificates \
    python3 python3-pip python3-venv python3-tk \
    qemu-system-x86 qemu-utils ovmf musl-tools e2fsprogs dosfstools mtools \
    clang lld llvm cmake ninja-build libssl-dev xvfb

# The workflows run `sudo apt-get ...` unattended, so the runner user needs
# passwordless sudo for exactly that command and nothing else. (apt-get can
# still be coaxed into running code as root, so only run trusted workflows
# on this machine.) The file is validated with visudo before it is installed.
SUDOERS=/etc/sudoers.d/github-runner-apt
if ! sudo test -f "$SUDOERS"; then
    TMP="$(mktemp)"
    echo "$USER ALL=(root) NOPASSWD: /usr/bin/apt-get" > "$TMP"
    sudo visudo -cf "$TMP"
    sudo install -m 440 -o root -g root "$TMP" "$SUDOERS"
    rm -f "$TMP"
fi

# QEMU needs /dev/kvm for speed; add the user to the kvm group.
sudo usermod -aG kvm "$USER" || true

# Rust: rust-toolchain.toml pins the nightly, rustup fetches it on first use.
if ! command -v rustup >/dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"

# setup-python caches its Pythons here; make sure the user can write to it.
sudo mkdir -p /opt/hostedtoolcache
sudo chown -R "$USER" /opt/hostedtoolcache

# Make cargo visible to the runner service (it reads <runner dir>/.path).
for dir in "$HOME"/actions-runner*/; do
    [ -d "$dir" ] || continue
    echo "$HOME/.cargo/bin:$PATH" > "${dir}.path"
done

echo
echo "Done. Log out and back in (kvm group), then restart the runner service:"
echo "  cd ~/actions-runner && sudo ./svc.sh stop && sudo ./svc.sh start"
