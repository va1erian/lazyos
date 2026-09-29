#!/usr/bin/env bash
# Best-effort: let the unprivileged CI user open /dev/kvm so QEMU can use KVM.
#
# GitHub-hosted ubuntu runners expose /dev/kvm, but as root:kvm 0660 and the
# `runner` user is not in the `kvm` group, so `-accel kvm` fails with
# "Permission denied" until this udev rule is applied (the rule GitHub's own
# docs recommend for the Android emulator). The QEMU tools default to
# `--accel auto`, which only picks KVM when a real probe start succeeds, so if
# this script cannot enable KVM (no device, no sudo, GitHub changes policy)
# the run silently falls back to TCG. It never fails the job.

set -u

if [ ! -e /dev/kvm ]; then
  echo "enable_kvm: /dev/kvm not present; QEMU will use TCG"
  exit 0
fi

if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
  if command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
    echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' \
      | sudo tee /etc/udev/rules.d/99-kvm4all.rules >/dev/null
    sudo udevadm control --reload-rules || true
    sudo udevadm trigger --name-match=kvm || true
    # udev applies the rule asynchronously; give it a moment.
    sudo udevadm settle --timeout=5 2>/dev/null || true
  fi
fi

ls -la /dev/kvm
if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
  echo "enable_kvm: /dev/kvm is accessible; --accel auto will use KVM"
else
  echo "enable_kvm: /dev/kvm is not accessible; QEMU will use TCG"
fi
exit 0
