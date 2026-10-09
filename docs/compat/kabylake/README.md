# Kaby Lake box (i3-7100U): collected facts

Output saved from the box's Linux lives here (kabylake-box-plan step B0, and
R0 of [rtl8168-driver-plan.md](../../rtl8168-driver-plan.md)). Nothing has been
collected yet. On the box, as root, with the cable plugged in (the PHY firmware
patch loads at link-up):

```bash
sudo python3 tools/net/rtl8168/rtl8168_probe.py survey --iface enp2s0 --bdf 0000:02:00.0
lspci -nn > docs/compat/kabylake/lspci-nn.txt; lsblk > docs/compat/kabylake/lsblk.txt
```

`survey` writes `report.txt` (registers decoded and the plan's assumptions
checked, the PHY's MII registers), `ethtool-*.txt`, `dmesg-r8169.txt`,
`lspci.txt` and `bar2.bin` here. Commit them, then settle each *to confirm* in
the plan from them.
