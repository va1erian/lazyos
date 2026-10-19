#!/bin/sh
# Drive a keyd probe app (tools/keyd/run.py); every call prints one line,
# KEYD:APP:<id>:<verb>:PASS|FAIL:<detail>:
#   sh probe.sh install <id>   copy the package home and `pkgctl install` it
#   sh probe.sh remove <id>    `pkgctl remove org.keyd.<id>`
#   sh probe.sh run <id>       launch it and print the line it wrote
# The probe (appprobe.rhai) writes $HOME/.apps/org.keyd.<id>/out.txt once; the
# old file is removed first so a result from an earlier boot is never read.
verb="$1"
id="$2"
name="org.keyd.$id"
case "$verb" in
install)
    cp "/system/share/keyd/$id.lzp" "$HOME/$id.lzp" &&
        pkgctl install "$HOME/$id.lzp" >/dev/null &&
        echo "KEYD:APP:$id:install:PASS:ok" ||
        echo "KEYD:APP:$id:install:FAIL:pkgctl"
    ;;
remove)
    pkgctl remove "$name" >/dev/null &&
        echo "KEYD:APP:$id:remove:PASS:ok" ||
        echo "KEYD:APP:$id:remove:FAIL:pkgctl"
    ;;
run)
    dir="$HOME/.apps/$name"
    mkdir -p "$dir"
    rm -f "$dir/out.txt"
    rhai -e "sys::init::launch(\"$name\", \"\", 0)" >/dev/null
    n=0
    while [ ! -s "$dir/out.txt" ] && [ "$n" -lt 120 ]; do
        sleep 1
        n=$((n + 1))
    done
    cat "$dir/out.txt" 2>/dev/null || echo "KEYD:APP:$id:run:FAIL:no result"
    ;;
esac
