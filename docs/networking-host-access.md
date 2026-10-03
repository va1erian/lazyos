# Networking under QEMU: reaching the guest from the host

How to boot LazyOS with a working network, what the network looks like from
both sides, and how to connect **host → guest** and **guest → host**. The stack
itself is described in [`architecture/networking.md`](architecture/networking.md);
the test harness in [`tools/net/README.md`](../tools/net/README.md).

## Quick start

```bash
python tools/run_demo.py --desktop --net
```

or, in the launcher (`python tools/lazyos_gui.py`), tick **Networking** on the
Simple tab and press Start. Then, on the LazyOS desktop:

1. open **Net Tools** from the start menu (Internet group). Its web server
   starts with it, on guest port 8080;
2. on the host, open **<http://localhost:8080>** in a browser. You get a page
   served by LazyOS ("Hello from LazyOS", the guest's address, uptime and a
   request counter), and the visit shows up in Net Tools' log.

`--net` builds the image with the network stack (`LAZYOS_NETD=1`: the
virtio-net driver `netdrv`, the stack service `netd`, and `ping`, `nslookup`,
`nc`, `ftp` in the shell) and gives QEMU a virtio-net card on its user-mode
network with host port 8080 forwarded to the guest. On the desktop it also adds
two apps: **Network** (status and settings) and **Net Tools** (ping, name
lookups, a web fetch and the web server). Without `--desktop` you get the
console with the same shell tools.

## The network QEMU gives the guest

QEMU's user-mode network ("slirp") is a private, NATed network that exists
only inside the QEMU process. It needs no administrator rights and no host
configuration:

```
  host (your machine)                    QEMU process                       LazyOS guest
  ───────────────────                    ────────────                       ────────────
  127.0.0.1:8080  ── hostfwd ──────────► 10.0.2.15:8080 ─────────────────► Net Tools web server
  127.0.0.1:PORT  ◄─ 10.0.2.2:PORT ───── virtual gateway / host alias ◄─── nc 10.0.2.2 PORT
  host resolver   ◄─ 10.0.2.3:53 ─────── virtual DNS ◄──────────────────── nslookup, Resolve
  the internet    ◄─ NAT ─────────────── outbound TCP/UDP ◄─────────────── fetch http://example.com
```

| Address | What it is |
|---|---|
| `10.0.2.15/24` | the guest, handed out by QEMU's DHCP server |
| `10.0.2.2` | the gateway, and an alias for the **host's loopback** (`127.0.0.1`) |
| `10.0.2.3` | the DNS server (QEMU forwards to the host's resolver) |

* **Guest → anywhere** works out of the box (TCP, UDP, DNS): QEMU NATs it
  through the host's own connections. `ping 10.0.2.2` is always answered;
  pinging the internet depends on the host (QEMU needs unprivileged ICMP,
  which Windows hosts usually lack).
* **Host → guest** only works through **port forwards** (below). The guest has
  no address the host can route to.

## Host → guest: port forwards

A forward is `[tcp:|udp:][HOSTADDR:]HOSTPORT:GUESTPORT`. `run_demo.py --net`
forwards `tcp:127.0.0.1:8080:8080` unless you give your own:

```bash
python tools/run_demo.py --desktop --net --net-forward 8080:8080 --net-forward 2323:2323
python tools/run_demo.py --net --net-forward udp:5555:5555     # a UDP port
python tools/run_demo.py --net --net-forward 9090:8080         # host 9090 -> Net Tools (8080 busy on the host)
python tools/run_demo.py --net --net-forward none              # nothing forwarded
```

Giving any `--net-forward` replaces the default, so repeat `8080:8080` if you
still want the Net Tools page. The host address defaults to `127.0.0.1`: a
forwarded port is reachable **from your machine only**. To let other machines
on your LAN in, name it explicitly (`--net-forward 0.0.0.0:8080:8080`); the
guest has no firewall, so do this only on a network you trust. `run_demo.py`
checks the host ports before starting QEMU and stops with a message if one is
taken. When it starts, it prints the forwards and the URL to open.

**Example: a raw TCP connection into the guest.** Boot with
`--net-forward 2323:2323`, then in the LazyOS Terminal (or the console):

```sh
nc -l -w 60 2323 hello from lazyos
```

`nc -l` waits for one connection, sends the text, and prints what the other
side sends until it is idle for 60 s. On the host:

```bash
python -c "import socket; s = socket.create_connection(('localhost', 2323)); print(s.recv(100)); s.sendall(b'hi from the host\n')"
```

## Guest → host

Anything listening on the host's loopback is reachable from the guest at
`10.0.2.2`. For example, serve a directory on the host:

```bash
python -m http.server 8000 --bind 127.0.0.1
```

and fetch it from the guest: in Net Tools type `http://10.0.2.2:8000/` and press
Fetch. For a raw TCP exchange from the shell, listen on the host:

```bash
python -c "import socket; c, _ = socket.create_server(('127.0.0.1', 7000)).accept(); print(c.recv(100)); c.sendall(b'hello from the host\n')"
```

and connect from the guest (`nc` sends the text, then prints the reply):

```sh
nc 10.0.2.2 7000 hi from lazyos
```

A forward and `10.0.2.2` combine: from the guest, `http://10.0.2.2:8080/` goes
out to the host's forwarded port and straight back in to Net Tools' own server
(the scripted session `net_apps.json` checks exactly that). The same goes for the `ftp` client (`ftp 10.0.2.2:2121 user=NAME pass=SECRET`)
and any other server on the host. Name lookups (`nslookup example.com`, Net
Tools' Look up) go through `10.0.2.3`; there is no TLS on LazyOS yet, so web
fetches are `http://` only.

## Configuring the guest's address

DHCP is the default and is what QEMU's network expects. The **Network** app
switches between *Automatic (DHCP)* and *Manual* (address with prefix,
gateway, DNS server); it writes `confd`'s `sys/net/eth0/{mode,address,gateway,dns}`
and `netd` picks the change up within a few seconds and restarts itself to
apply it (`init` brings it straight back; open connections drop). From a
shell, the same keys work with `confctl`:

```sh
confctl set sys/net/eth0/address str 10.0.2.15/24
confctl set sys/net/eth0/gateway str 10.0.2.2
confctl set sys/net/eth0/dns str 10.0.2.3
confctl set sys/net/eth0/mode str static      # last: netd must not see `static` before the address
confctl set sys/net/eth0/mode str dhcp        # back to automatic
```

Under QEMU's user network a manual address must stay `10.0.2.15/24` for the
port forwards to keep working: QEMU delivers forwarded connections to that
address only. A static setup `netd` cannot use (a gateway outside the subnet,
an unusable address) makes it fall back to DHCP; the Network app refuses such
a setup with the reason instead of saving it.

## The launcher and the other tools

* **Launcher GUI** (`python tools/lazyos_gui.py`): the Simple tab's
  *Networking* box is `run_demo.py --net` with the default forward. The
  Advanced tab's *Networking* group has the switch (`LAZYOS_NETD`), a *Port
  forwards* field (space-separated forwards; empty means the default, `none`
  means none) and *Isolate the guest*. They apply to the interactive demo, the
  headless screenshots and the scripted sessions alike.
* **Screenshot tools** take the same options as `run_demo.py`
  (`--net`, `--net-forward`, `--net-restrict`, `--net-pcap`), for an image built
  with `LAZYOS_NETD=1`:

  ```bash
  LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_NETD_ARGS=demo=0 cargo build
  python tools/screenshot/qemu_session.py --image target/lazyos.img --net \
      --out shots/net_apps --script tools/screenshot/examples/net_apps.json
  ```

* `--net-restrict` isolates the guest (QEMU `restrict=on`): it cannot reach the
  host or the internet, but forwarded ports still reach it.
* `--net-pcap PATH` records every frame the card sends and receives; open the
  file with Wireshark, or judge it with `tools/net/analyze_pcap.py`.
* `LAZYOS_NETD_ARGS=demo=0` is what `--net` and the launcher build with: `netd`
  runs alone. Without it `netd demo=1` runs the evidence clients the network
  harness (`python tools/net/run.py --netd`) judges, which expect that
  harness's host servers.

## Other QEMU network back ends

The scripts wire only the user-mode network. For a bridged or TAP setup, build
the stack yourself and hand QEMU your own card instead of `--net`:

```bash
LAZYOS_NETD=1 LAZYOS_NETD_ARGS=demo=0 python tools/run_demo.py --desktop \
    -- -netdev tap,id=n0,ifname=tap0,script=no,downscript=no -device virtio-net-pci,netdev=n0
```

`run_demo.py` sets nothing network-related without `--net`, so the environment
variables carry the build switch. The guest then asks your LAN's DHCP server
for an address, and the host reaches it directly, no forwards needed.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `host port(s) tcp/127.0.0.1:8080 already in use` | Something on the host holds the port: `--net-forward 9090:8080`, then open `http://localhost:9090` |
| The browser waits, then fails | Net Tools is not open, or its server was stopped (its window says which). The page is served by the app |
| Net Tools says "no address yet" | DHCP takes a second or two after boot; the headline updates by itself |
| "the network stack is not running" | The image was built without the stack: boot with `--net` (or tick Networking) |
| `https://` refused | No TLS on LazyOS yet; use `http://` |
| A connection to a closed host port hangs instead of failing | QEMU's user network on Windows does not answer it with a reset; the client times out |
