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
    out=$(rm "$SHARE/canary" 2>&1)
    res $? "$out"
    ;;
overwrite_init)
    # Opens for writing, appends nothing.
    out=$(: >> /system/bin/init 2>&1)
    res $? "$out"
    ;;
write_conf)
    out=$(echo x > /conf/acct-probe 2>&1)
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
    f=${HOME:-/tmp}/acct-fill
    out=$(dd if=/dev/zero of="$f" bs=1M count=32 2>&1)
    rc=$?
    rm -f "$f"
    res $rc "$out"
    ;;
*)
    echo "ACCT:ATTACK:$name:ERROR:unknown"
    ;;
esac
