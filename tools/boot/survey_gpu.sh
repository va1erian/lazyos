#!/bin/sh
# Collect what the Kaby Lake GPU plan needs (docs/kabylake-gpu-plan.md, stage
# G0 and the "to confirm" list of section 2) from the box's own Linux.
#
#     sudo sh tools/boot/survey_gpu.sh [output-dir]
#
# survey_linux_box.sh calls this with <its output-dir>/gpu, so a normal survey
# already includes it. Run it alone, with the monitor and (ideally) the HDMI
# audio in use, to refresh just the GPU facts. Commit the result as
# docs/compat/kabylake/gpu/ (README.md there says what to redact first).
#
# READ-ONLY. Nothing is written to the machine: no module is loaded, debugfs
# is not mounted for you, no MMIO write is made by this script (debugfs reads can still wake
# forcewake or make the sink transact on AUX/DPCD), the option ROM is not
# enabled. Registers are read with `intel_reg read` (the script only ever
# invokes that verb: it issues no write) or, without it, through a PROT_READ
# mmap of the BAR0 resource. A missing tool, file or privilege only leaves a
# note in its output file and in MISSING.txt.
#
# Optional tools (Debian/Ubuntu):
#     apt install pciutils intel-gpu-tools alsa-utils edid-decode python3
#
# Environment: GPU_BDF (default 0000:00:02.0), AUDIO_BDF (default 0000:00:1f.3).
#
# POSIX sh on purpose (no bashisms): it also runs from a minimal live image.

set -u

gpu=${GPU_BDF:-0000:00:02.0}
audio=${AUDIO_BDF:-0000:00:1f.3}
stamp=$(date +%Y%m%d-%H%M)
host=$(hostname 2>/dev/null || echo box)
out=${1:-./gpu-survey-$host-$stamp}
mkdir -p "$out" || exit 1
: >"$out/MISSING.txt"

if [ "$(id -u)" != 0 ]; then
    echo "note: not root; debugfs, BAR reads and lspci -vvv will be incomplete" >&2
fi

missing() {
    echo "$*" >>"$out/MISSING.txt"
}

# run <file> <command...>: stdout and stderr go to $out/<file>.
run() {
    name=$1
    shift
    {
        echo "# $*"
        if command -v "$1" >/dev/null 2>&1; then
            "$@" 2>&1
            echo "# exit $?"
        else
            echo "# not installed: $1"
            missing "$name: $1 not installed"
        fi
    } >"$out/$name"
}

# runsh <file> <sh -c text>: for pipelines and globs.
runsh() {
    name=$1
    shift
    {
        echo "# $*"
        sh -c "$*" 2>&1
        echo "# exit $?"
    } >"$out/$name"
}

# grab <source> <destination name>: a binary or text file copied verbatim.
grab() {
    if [ -r "$1" ]; then
        cat "$1" >"$out/$2" 2>/dev/null || missing "$2: read of $1 failed"
    else
        missing "$2: $1 not readable"
    fi
}

# PCI ------------------------------------------------------------------------
short() { echo "$1" | sed 's/^0000://'; }
g=$(short "$gpu")
a=$(short "$audio")

run 00-lspci-gpu.txt lspci -vvv -nn -D -s "$g"
run 00-lspci-audio.txt lspci -vvv -nn -D -s "$a"
run 00-lspci-gpu-xxxx.txt lspci -xxxx -D -s "$g"
run 00-lspci-audio-xxxx.txt lspci -xxxx -D -s "$a"
run 00-lspci-host-bridge.txt lspci -vvv -nn -D -s 00:00.0
# The three config dwords the plan names: GMCH_CTRL (0x50), BDSM (0x5C, stolen
# memory base), ASLS (0xFC, the ACPI OpRegion address that holds the VBT).
runsh 00-gpu-config-words.txt "setpci -s $g 50.W 5C.L FC.L 2>&1; echo '(GMCH_CTRL 0x50, BDSM 0x5C, ASLS 0xFC)'"
runsh 00-gpu-sysfs.txt "d=/sys/bus/pci/devices/$gpu; for f in vendor device revision subsystem_vendor subsystem_device class resource irq enable boot_vga driver_override; do printf '%s: ' \$f; cat \$d/\$f 2>&1; done; readlink \$d/driver 2>&1; ls -l \$d/ | head -60"
runsh 00-i915-params.txt 'for f in /sys/module/i915/parameters/*; do [ -r "$f" ] && printf "%s=%s\n" "$(basename $f)" "$(cat $f 2>&1)"; done'
runsh 00-i915-modinfo.txt 'modinfo i915 2>&1 | grep -i -E "^(filename|version|firmware|vermagic)" | head -60'
runsh 00-iomem-gpu.txt 'grep -i -E "i915|stolen|graphics|gtt|igfx|0000:00:02.0|0000:00:1f.3|audio" /proc/iomem 2>&1'

# Registers -------------------------------------------------------------------
# Whole named dump when intel-gpu-tools is there.
run 01-intel_reg-dump.txt intel_reg dump

# The ranges below are a superset of the Gen9 display, pipe A / EDP, DDI,
# DPLL, CDCLK, power-well, DDB/watermark and audio blocks, from memory of
# i915's regs; the offsets are TO CONFIRM and a wrong range only wastes a few
# lines. GMBUS data (0xC510C) is deliberately NOT read: a data read advances
# the transfer. The bytes come from `intel_reg read` or a PROT_READ mmap of
# resource0; neither path in this script issues a write.
mmio_ranges="
0x045000 0x045600 power wells, DC state, DMC control
0x046000 0x046200 CDCLK_CTL, LCPLL1/2, PORT_CLK_SEL
0x051000 0x051010 DFSM (fuses)
0x060000 0x060500 transcoder A: timings, PIPESRC, CRC, DDI function
0x06F000 0x06F100 EDP transcoder
0x064000 0x064400 DDI A-E buffer, AUX
0x064E00 0x064F00 DDI buffer translations
0x065000 0x065500 audio: AUD_CONFIG, MISC_CTRL, ELD control and port, DIP, ELD_CP_VLD (all pipes)
0x065800 0x065A00 audio: AUD_FREQ_CNTRL and neighbours
0x065F00 0x066000 audio: AUD_CHICKENBIT (codec wake)
0x048400 0x048430 audio: AUD_PIN_BUF_CTL (WA #1154)
0x068000 0x068200 pipe scalers
0x06C000 0x06C100 DPLL_CTRL1/2/STATUS, DPLL CFGCR
0x070000 0x070300 pipe A conf/misc, plane 1/2/3, cursor, DDB, watermarks
0x0C2000 0x0C2100 south display: SFUSE_STRAP
0x0C4000 0x0C4040 display straps
0x0C5100 0x0C510C GMBUS0..2 only
0x000D80 0x000D90 forcewake acks: render 0xD84, media 0xD88 (G3, to confirm)
0x00A180 0x00A290 forcewake requests: blitter 0xA188, media 0xA270, render 0xA278 (G3)
0x130040 0x130048 forcewake ack: blitter
"

# Each range, one dword per register, through `intel_reg read` (a read-only
# mapping of BAR0, the same one the named dump above used). The Python mmap of
# the sysfs resource below failed with EINVAL on the first box (G0, 2026-10-13)
# while intel_reg worked, so this is the preferred path.
mmio_intel_reg() {
    echo "$mmio_ranges" | while read -r lo hi rest; do
        [ -n "$lo" ] || continue
        echo "# range $lo-$hi $rest"
        regs=""
        off=$((lo))
        end=$((hi))
        while [ "$off" -lt "$end" ]; do
            regs="$regs $(printf '0x%06x' "$off")"
            off=$((off + 4))
        done
        # shellcheck disable=SC2086
        intel_reg read $regs 2>&1
    done
}

mmio_dump() {
    res=/sys/bus/pci/devices/$gpu/resource0
    {
        if command -v intel_reg >/dev/null 2>&1; then
            echo "# intel_reg read, read-only; ranges TO CONFIRM"
            mmio_intel_reg
        elif ! command -v python3 >/dev/null 2>&1; then
            echo "# read-only mmap of $res; ranges TO CONFIRM"
            echo "# python3 missing: only intel_reg output is available"
            missing "02-mmio-ranges.txt: python3 not installed"
        elif [ ! -r "$res" ]; then
            echo "# $res not readable (root needed)"
            missing "02-mmio-ranges.txt: $res not readable"
        else
            # The ranges go in through the environment: the program text comes
            # from stdin.
            MMIO_RANGES="$mmio_ranges" MMIO_RES="$res" python3 - <<'PY'
import mmap, os, struct, sys
res = os.environ["MMIO_RES"]
try:
    fd = os.open(res, os.O_RDONLY)
    m = mmap.mmap(fd, 0, prot=mmap.PROT_READ)
except Exception as e:
    print("# cannot map:", e)
    sys.exit(0)
for line in os.environ["MMIO_RANGES"].splitlines():
    parts = line.split(None, 2)
    if len(parts) < 2:
        continue
    lo, hi = int(parts[0], 16), int(parts[1], 16)
    print("# range %#x-%#x %s" % (lo, hi, parts[2] if len(parts) > 2 else ""))
    for off in range(lo, hi, 4):
        if off + 4 > len(m):
            break
        print("%#08x 0x%08x" % (off, struct.unpack_from("<I", m, off)[0]))
PY
        fi
    } >"$out/02-mmio-ranges.txt" 2>&1
}
mmio_dump

# Kernel's own decode (debugfs) --------------------------------------------------
dbg=/sys/kernel/debug/dri
if ! [ -d "$dbg" ]; then
    echo "# $dbg missing. debugfs is not mounted or i915 is not bound." >"$out/03-debugfs-note.txt"
    echo "# Mount it by hand if you want these: mount -t debugfs none /sys/kernel/debug" >>"$out/03-debugfs-note.txt"
    missing "03-*: $dbg not available"
else
    for dir in "$dbg"/[0-9]*; do
        [ -d "$dir" ] || continue
        # i915 names its directory by PCI address and links minors 1 and 128
        # to it: one copy of the files is enough, and a ':' in a file name
        # cannot be checked out on Windows (the first capture had it).
        [ -L "$dir" ] && continue
        n=$(basename "$dir" | tr ':' '_')
        for f in i915_display_info i915_dmc_info i915_gem_gtt i915_ddb_info \
            i915_power_domain_info i915_runtime_pm_status i915_shared_dplls_info \
            i915_forcewake_domains i915_engine_info i915_gem_framebuffer \
            i915_fbc_status i915_drrs_status i915_psr_status i915_guc_info \
            i915_huc_load_status i915_guc_load_status \
            i915_dp_mst_info i915_hdcp_sink_capability \
            i915_edp_psr_status i915_wa_registers i915_drpc_info \
            i915_frequency_info i915_ips_status i915_sr_status; do
            [ -r "$dir/$f" ] && grab "$dir/$f" "03-dri$n-$f.txt"
        done
        # The VBT as a blob, for tools/boot (libs/intelgfx::vbt) and
        # intel_vbt_decode; the OpRegion as a blob when this kernel has it.
        grab "$dir/i915_vbt" "03-dri$n-vbt.bin"
        [ -r "$dir/i915_opregion" ] && grab "$dir/i915_opregion" "03-dri$n-opregion.bin"
        # Whatever else i915 exposes at the top of the directory, names only.
        runsh "03-dri$n-listing.txt" "ls -l $dir | head -150"
    done
fi
run 03-intel_vbt_decode.txt sh -c 'for f in "'"$out"'"/03-dri*-vbt.bin; do [ -r "$f" ] && echo "== $f" && intel_vbt_decode "$f"; done'

# EDID and connectors -------------------------------------------------------------
for c in /sys/class/drm/card*-*; do
    [ -d "$c" ] || continue
    n=$(basename "$c")
    {
        echo "# $c"
        printf "status: "; cat "$c/status" 2>&1
        printf "enabled: "; cat "$c/enabled" 2>&1
        printf "dpms: "; cat "$c/dpms" 2>&1
        echo "modes:"; cat "$c/modes" 2>&1
    } >"$out/04-$n-connector.txt"
    if [ -r "$c/edid" ]; then
        # Raw bytes (a disconnected connector gives an empty file), a hex
        # dump to read in a diff, and a decode when edid-decode exists.
        cat "$c/edid" >"$out/04-$n-edid.bin" 2>/dev/null
        if [ -s "$out/04-$n-edid.bin" ]; then
            od -An -tx1 -v "$out/04-$n-edid.bin" >"$out/04-$n-edid.hex"
            if command -v edid-decode >/dev/null 2>&1; then
                edid-decode "$out/04-$n-edid.bin" >"$out/04-$n-edid-decode.txt" 2>&1
            else
                missing "04-$n-edid-decode.txt: edid-decode not installed"
            fi
        else
            rm -f "$out/04-$n-edid.bin"
        fi
    fi
done

# Audio -------------------------------------------------------------------------------
runsh 05-asound-cards.txt 'cat /proc/asound/cards 2>&1; echo; ls -l /proc/asound 2>&1'
run 05-aplay-cards.txt aplay -l
run 05-aplay-pcms.txt aplay -L
runsh 05-eld.txt 'for f in /proc/asound/card*/eld#*; do [ -r "$f" ] && { echo "== $f"; cat "$f"; }; done'
runsh 05-codecs.txt 'for f in /proc/asound/card*/codec#*; do [ -r "$f" ] && { echo "== $f"; cat "$f"; }; done'
runsh 05-pcm-info.txt 'for f in /proc/asound/card*/pcm*/info; do [ -r "$f" ] && { echo "== $f"; cat "$f"; }; done'
runsh 05-amixer.txt 'for c in /proc/asound/card[0-9]*; do n=${c##*card}; echo "== card $n"; amixer -c $n contents 2>&1; done'
runsh 05-hda-sysfs.txt "d=/sys/bus/pci/devices/$audio; for f in vendor device subsystem_vendor subsystem_device class; do printf '%s: ' \$f; cat \$d/\$f 2>&1; done; readlink \$d/driver 2>&1"

# Kernel log --------------------------------------------------------------------------
runsh 06-dmesg-gpu-audio.txt 'dmesg | grep -i -E "i915|snd_hda|hdmi|dmc|guc|huc|audio|eld|vbt|opregion|stolen|gtt|ggtt|aperture|DDI|lspcon|dp_aux"'
runsh 06-dmesg-i915-full.txt 'dmesg | grep -i "i915"'
runsh 06-firmware.txt 'ls -l /lib/firmware/i915 2>&1 | grep -i -E "kbl|skl|glk" ; ls -l /usr/lib/firmware/i915 2>&1 | grep -i -E "kbl|skl|glk"'

# Temperature and frequency baseline (plan section 7: idle power/thermal)
runsh 07-gt-freq.txt 'for c in /sys/class/drm/card[0-9]; do echo "== $c"; for f in gt_act_freq_mhz gt_cur_freq_mhz gt_min_freq_mhz gt_max_freq_mhz gt_boost_freq_mhz gt_RP0_freq_mhz gt_RP1_freq_mhz gt_RPn_freq_mhz power/rc6_residency_ms; do [ -r "$c/$f" ] && printf "%s: %s\n" $f "$(cat $c/$f)"; done; done'
runsh 07-thermal.txt 'for z in /sys/class/thermal/thermal_zone*; do echo "$z $(cat $z/type 2>&1) $(cat $z/temp 2>&1)"; done'

{
    echo "GPU survey by tools/boot/survey_gpu.sh on $host at $stamp"
    echo "run as uid $(id -u); gpu=$gpu audio=$audio"
    echo "MISSING.txt lists what could not be collected."
    ls -l "$out"
} >"$out/README.txt"

echo "wrote $out"
