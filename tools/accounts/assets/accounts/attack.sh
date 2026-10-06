# One attack per call: sh attack.sh <name>
# Prints exactly one line, ACCT:ATTACK:<name>:BLOCKED|SUCCEEDED:<detail>, which
# the Terminal reports on serial as TERM:OUT. Attacks that would brick the
# image are probes: they open for writing without changing a byte, or touch a
# canary file; anything created is removed again. See tools/accounts/README.md.
SHARE=/system/share/accounts
name=$1

# res <rc> <error text>: the verdict of one attempt.
res() {
    if [ "$1" = 0 ]; then
        echo "ACCT:ATTACK:$name:SUCCEEDED:ok"
        return
    fi
    case "$2" in
    *denied*) e=EACCES ;;
    *permitted*) e=EPERM ;;
    *"No such"*) e=ENOENT ;;
    *"Read-only"*) e=EROFS ;;
    *) e=err ;;
    esac
    echo "ACCT:ATTACK:$name:BLOCKED:$e"
}

case "$name" in
uid)
    u=$(id -u)
    if [ "$u" = 0 ]; then echo "ACCT:ATTACK:uid:SUCCEEDED:root"; else echo "ACCT:ATTACK:uid:BLOCKED:uid=$u"; fi
    ;;
rm_system)
    # -f: a write-protected file must not make rm ask (and eat the next
    # command typed into the Terminal as its answer).
    out=$(rm -f "$SHARE/canary" 2>&1)
    res $? "$out"
    ;;
overwrite_init)
    # Opens for writing, appends nothing. The redirection runs in a subshell
    # whose stderr is captured: a refused open is reported by the shell doing
    # the redirection, which would otherwise print it to the Terminal.
    out=$( (: >> /system/bin/init) 2>&1)
    res $? "$out"
    ;;
write_conf)
    out=$( (echo x > /conf/acct-probe) 2>&1)
    rc=$?
    rm -f /conf/acct-probe 2>/dev/null
    res $rc "$out"
    ;;
read_home_admin)
    out=$(ls /home/admin 2>&1)
    res $? "$out"
    ;;
signal_service)
    pid=$(rhai $SHARE/svc_pid.rhai 2>/dev/null)
    if [ -z "$pid" ] || [ "$pid" = 0 ]; then
        echo "ACCT:ATTACK:signal_service:ERROR:nopid"
    else
        # SIGCONT: harmless to a running service.
        out=$(kill -CONT "$pid" 2>&1)
        res $? "$out"
    fi
    ;;
fork_bomb)
    # Bounded: up to 300 sleepers, then all killed. More than 150 forks as one
    # user means no per-user task limit. The counters live in the home: the
    # session user may not write /tmp's root.
    d=${HOME:-/tmp}
    rm -f "$d/acct.n" "$d/acct.pids"
    D="$d" sh -c 'n=0; while [ $n -lt 300 ]; do sleep 100 & echo $! >> "$D/acct.pids"; n=$((n+1)); echo $n > "$D/acct.n"; done' 2>/dev/null
    n=$(cat "$d/acct.n" 2>/dev/null)
    kill $(cat "$d/acct.pids" 2>/dev/null) 2>/dev/null
    if [ "${n:-0}" -gt 150 ]; then
        echo "ACCT:ATTACK:fork_bomb:SUCCEEDED:forks=$n"
    else
        echo "ACCT:ATTACK:fork_bomb:BLOCKED:forks=${n:-0}"
    fi
    rm -f "$d/acct.n" "$d/acct.pids"
    ;;
disk_fill)
    # Bounded: 32 MiB into the home. A quota (U3) would refuse it.
    f=${HOME:-/tmp}/acct-fill
    out=$(dd if=/dev/zero of="$f" bs=1M count=32 2>&1)
    rc=$?
    rm -f "$f"
    res $rc "$out"
    ;;
autostart_pkg)
    # Install a user package that opens at login (org.acct.autoprobe, built
    # by run.py). Installing is allowed; the attack is what init does with it
    # at the next login, which run.py judges from the verify boot: it must run
    # as the session's user, never as root (accounts plan section 2). Here
    # only the install is reported, as ACCT:INSTALL.
    if pkgctl install $SHARE/autoprobe.lzp > /dev/null 2>&1; then
        echo "ACCT:INSTALL:autostart_pkg:OK"
    else
        echo "ACCT:INSTALL:autostart_pkg:FAIL"
    fi
    ;;
core_replace)
    # Replace a core app with a higher-versioned package of the same name
    # (os.lazy.counter 99.0.0, the Counter's own program): only an admin
    # should (U3). Left installed, so the later boots run with it.
    out=$(pkgctl install $SHARE/corereplace.lzp 2>&1)
    res $? "$out"
    ;;
shell_role)
    # Claim xuid's shell role from the session while LazyShell holds it
    # (shellprobe subscribes as the shell; refused, it exits at once). A probe
    # still running after a few seconds holds the role: kill it.
    [ -x /system/bin/shellprobe ] || { echo "ACCT:ATTACK:shell_role:ERROR:noprobe"; exit 0; }
    shellprobe > /dev/null 2>&1 &
    p=$!
    sleep 4
    if kill -0 "$p" 2>/dev/null; then
        kill "$p" 2>/dev/null
        echo "ACCT:ATTACK:shell_role:SUCCEEDED:subscribed"
    else
        echo "ACCT:ATTACK:shell_role:BLOCKED:EACCES"
    fi
    ;;
*)
    echo "ACCT:ATTACK:$name:ERROR:unknown"
    ;;
esac
