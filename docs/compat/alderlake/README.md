# Alder Lake box (CHUWI LarkBox X, i3-1220P): collected facts

A CHUWI LarkBox X mini PC (board `LarkBox X`, AMI BIOS `DB19C0.19` of
2025-10-29, EC 1.11), UEFI, Secure Boot disabled. It replaces the retired AMD
NUC (`MAGICNUC AS1`; [`../hardware.md`](../hardware.md), "Retired machines").

This directory is `tools/boot/survey_linux_box.sh` run on its Kubuntu 26.10
(kernel 7.3.0-9), plus `gpu/` from `tools/boot/survey_gpu.sh`. Both were
redacted with `tools/boot/redact_survey.py`; the HDMI EDID's monitor serial
was redacted by hand (`gpu/04-card1-HDMI-A-1-edid.*`, per
[`../kabylake/gpu/README.md`](../kabylake/gpu/README.md)).

The machine is genuine: CPUID family 6 model 154 (Alder Lake-P) matches DMI's
"12th Gen Intel(R) Core(TM) i3-1220P", and the two Ramaxel DDR4-3200 DIMMs
give 32 GiB (MemTotal 32590500 kB; the e820 map tops out at 0x8afbfffff).

| Function | Id | LazyOS |
|---|---|---|
| SATA (PCH AHCI), G534N 256G SSD | `8086:51d3` | `ahci` in-kernel (`libs/ahci`) |
| NVMe (YEESTOR GMT1000 / PNY CS1030 250G) | `1d89:ef25` | `nvme` in-kernel (`libs/nvme`); 512-byte LBA, so N1-compatible |
| 2x Ethernet, RTL8168h XID 541 | `10ec:8168` rev 15 | `netdrv` + `libs/rtl8168` (same chip as the Kaby Lake box) |
| 2x xHCI (PCH, Thunderbolt 4) | `8086:51ed`, `8086:461e` | `usbd` by class |
| HDA, Conexant SN6140 (analog) | `8086:51c8` | `sndd` by class (`libs/hda`, generic codec walk); **to confirm** |
| HDA, Intel Alder Lake-P HDMI (codec 2) | (same controller) | out: HDMI audio needs the GPU driver |
| Wi-Fi 6 AX101 (CNVi) | `8086:51f0` | **none**: `wifid` drives the RTL8822CE only; needs `iwlwifi` and firmware |
| UHD Graphics, Alder Lake-UP3 GT1 | `8086:46b3` | UEFI GOP framebuffer only |
| MEI/HECI, LPSS I2C/UART/SPI, SMBus, TPM2 | `8086:51e0`, ... | not needed |

## Platform facts worth knowing

- **Interrupts are MSI**: Linux binds the two NICs to IRQ 142/144, AHCI to 141
  and HDA to 169. This is the box the I/O APIC + MSI work (issue #616) is for.
- **ACPI**: FACP S0/S3/S4/S5, PM-Timer at I/O 0x1808, `HPET` present but
  "dysfunctional in PC10". `MCFG` and `DMAR` (VT-d) are both present, so ECAM
  and an IOMMU are reachable without firmware work.
- **Display**: one sink, HDMI-A-1 at 2560x1440 (IVM PL2730Q, EDID in `gpu/`);
  DP-1, DP-2, HDMI-A-2 and HDMI-A-3 are disconnected. The **GOP** mode is
  2560x1440 (read by LazyOS, `gpu/lazyos/`) — Linux drove i915, so the survey shows i915's state, not the
  firmware's.
- The i915 log carries a DMC firmware warning (`assert_dmc_loaded`), and
  iwlwifi loaded `so-a0-hr-b0-89.ucode`. Both are Linux-side notes.

## Still to check on the box

- ~~The `HW:GOP:`/`HW:FB:` lines, and that the stick boots at all.~~ It boots;
  the GOP leaves 2560x1440 BGRX (`gpu/lazyos/01-dbgd-capture.md`). The
  firmware's display path is read too: HDMI on DDI B from DPLL1 at 59.93 Hz.
- Which HDA path `sndd` picks, and whether the SN6140 walks to the headphone
  pin (0x16) or the speaker (0x17).
- MSI on the real NICs, AHCI and HDA rather than polling.
- Memory above 4 GiB (almost all of it is), and the NICs' DMA under a 32 GiB
  memory map.
