# The Kaby Lake box: evidence folder

Everything measured on the i3-7100U mini PC ([`../../kabylake-box-plan.md`](../../kabylake-box-plan.md)).
Facts from the box's own Linux are stated with their source; anything else is
to be confirmed. Files land here as they are collected:

| File or folder | What | Filled by |
|---|---|---|
| `survey/` | The Linux survey: `lspci`, `dmesg`, IOMMU groups, HDA codec, framebuffer, ethtool | `tools/boot/survey_linux_box.sh` (step 1 of [`B0.md`](B0.md)) |
| `boot-1/` | First stick boot: photos of each screen, `lazyos-boot.log`, `devctl-*.txt` | the person at the box ([`B0.md`](B0.md) steps 5 to 7) |
| `boot-N/` | Later boots, one folder each, same names | same |

The conclusions go to [`../hardware.md`](../hardware.md) (one row) and to the
open questions of the box plan section 4.
