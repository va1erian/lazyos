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
    # pkgd refusing to replace a core app for a non-administrator (U2).
    *"needs an administrator"*) e=EPERM ;;
    *"No such"*) e=ENOENT ;;
    *"Read-only"*) e=EROFS ;;
    *[Qq]uota*) e=EDQUOT ;;
    *"No space"*) e=ENOSPC ;;
    *)
        # Not a recognised refusal: never let it pass as proof of isolation.
        echo "ACCT:ATTACK:$name:ERROR:unrecognised"
        return
        ;;
    esac
    echo "ACCT:ATTACK:$name:BLOCKED:$e"
}

# probe <file> <snippet>: run the shell snippet (the file is its "$1") to
# create <file>, which must not exist yet: the snippet runs under noclobber
# (`sh -C`), so a success proves creation and nothing pre-existing is ever
# overwritten. Then remove the file only if this call created it (also when
# the snippet failed after creating it). 2>&1 outside the inner shell
# captures its redirection errors too.
probe() {
    f=$1
    existed=0
    [ -e "$f" ] && existed=1
    out=$(sh -C -c "$2" sh "$f" 2>&1)
    rc=$?
    # 2>/dev/null: an unreadable directory makes rm complain on the Terminal,
    # which would replace this attack's marker as the reported output.
    [ "$existed" = 0 ] && rm -f "$f" 2>/dev/null
    res $rc "$out"
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
    # Opens for writing, appends nothing.
    out=$( (: >> /system/bin/init) 2>&1 )
    res $? "$out"
    ;;
write_conf)
    f=/conf/acct-probe.$$
    probe "$f" 'echo x > "$1"'
    ;;
read_conf_store)
    # confd's raw store holds sys/** and every user's keys: root's alone
    # (/conf 0700, the store 0600). The content never reaches the Terminal.
    out=$(cat /conf/store 2>&1 > /dev/null)
    res $? "$out"
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
    # user means no per-user task limit.
    rm -f /tmp/acct.n /tmp/acct.pids
    sh -c 'n=0; while [ $n -lt 300 ]; do sleep 100 & echo $! >> /tmp/acct.pids; n=$((n+1)); echo $n > /tmp/acct.n; done' 2>/dev/null
    n=$(cat /tmp/acct.n 2>/dev/null)
    kill $(cat /tmp/acct.pids 2>/dev/null) 2>/dev/null
    if [ "${n:-0}" -gt 150 ]; then
        echo "ACCT:ATTACK:fork_bomb:SUCCEEDED:forks=$n"
    else
        echo "ACCT:ATTACK:fork_bomb:BLOCKED:forks=${n:-0}"
    fi
    rm -f /tmp/acct.n /tmp/acct.pids
    ;;
disk_fill)
    # Bounded: 32 MiB into the home. A quota (U3) would refuse it.
    # The file is created first (noclobber) so dd never writes over one that
    # already existed.
    f=${HOME:-/tmp}/acct-fill.$$
    probe "$f" ': > "$1" && dd if=/dev/zero of="$1" bs=1M count=32'
    ;;
autostart_pkg)
    # Install a user package that opens at login (org.acct.autoprobe, built by
    # probe_packages.py). Installing is allowed; the attack is what init does
    # with it at the next login, which run.py judges from the verify boot
    # (`autostart_root`): it must run as the session's user, never as root.
    # Only the install is reported here, as ACCT:INSTALL.
    if pkgctl install $SHARE/autoprobe.lzp > /dev/null 2>&1; then
        echo "ACCT:INSTALL:autostart_pkg:OK"
    else
        echo "ACCT:INSTALL:autostart_pkg:FAIL"
    fi
    ;;
core_replace)
    # Replace a core app with a higher-versioned package of the same name
    # (os.lazy.counter 99.0.0, the Counter's own program): only an admin,
    # through elevd's pkg.update-core, may (U2). Left installed when it
    # succeeds, so the later boots run with it.
    out=$(pkgctl install $SHARE/corereplace.lzp 2>&1)
    res $? "$out"
    ;;
admin_lockout)
    # Review of #659 (H5): a session flooding Authenticate("admin", ...)
    # must not lock admin out of the trusted prompt. The flood runs in the
    # background; 20 s later elevd shows the prompt, the harness types
    # admin's right password, and the request must be granted.
    rhai $SHARE/auth_hammer.rhai > /dev/null 2>&1 &
    hammer=$!
    sleep 20
    rhai $SHARE/admin_lockout.rhai
    kill $hammer 2>/dev/null
    wait
    ;;
prompt_over)
    # The trusted prompt (U2): elevd asks for an administrator, and a window
    # opens 5 s later while the prompt is up. The harness screenshots both
    # and cancels the prompt; prompt_judge.py judges. This prints the
    # request's outcome (ACCT:PROMPT:over:...).
    rhai $SHARE/elev_wait.rhai over &
    sleep 5
    rhai $SHARE/open_window.rhai > /dev/null
    wait
    ;;
prompt_keys)
    # The Terminal keeps the focus while elevd's prompt is up: the harness
    # types into the prompt, presses Enter and Escape, and prompt_judge.py
    # checks the Terminal never got a key (TERM:CMD:inject). Prints
    # ACCT:PROMPT:keys:<outcome>.
    rhai $SHARE/elev_wait.rhai keys
    ;;
input_flood)
    # The same while three programs keep inputd's shared endpoint full
    # (review of #659, H3): the prompt must still own the keyboard, or refuse
    # to open. One line: ACCT:PROMPT:flood:<outcome>:full=<refused sends>.
    rm -f /tmp/acct.flood
    for n in 1 2 3; do
        rhai --max-ops 0 $SHARE/input_flood.rhai 45 >> /tmp/acct.flood 2>&1 &
    done
    sleep 3
    out=$(rhai $SHARE/elev_wait.rhai flood)
    wait
    full=$(sed -n 's/.*:full=\([0-9]*\).*/\1/p' /tmp/acct.flood | awk '{s+=$1} END {print s+0}')
    rm -f /tmp/acct.flood
    echo "$out:full=${full:-0}"
    ;;
prompt_flood)
    # Prompt after prompt (review of #659, H4): prompt_flood.rhai prints the
    # ACCT:ATTACK line itself.
    rhai $SHARE/prompt_flood.rhai
    ;;
*)
    echo "ACCT:ATTACK:$name:ERROR:unknown"
    ;;
esac
