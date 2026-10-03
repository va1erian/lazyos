# Net Tools

Net Tools shows LazyOS's network stack at work.

* **Ping** sends four echo requests to a host, one per second, and shows each
  reply. Type an address (`10.0.2.2`, the host under QEMU) or a name.
* **Look up a name** asks the DNS server for a host name's addresses.
* **Fetch a web page** downloads an `http://` URL (there is no TLS yet, so not
  `https://`) and shows the status line, the size and the first lines.
* **Web server** starts with the app and answers on port 8080 with a small page
  about this machine. Under QEMU, `python tools/run_demo.py --net` forwards the
  host's port 8080 to it: open `http://localhost:8080` in a browser on the
  host. Every visit is listed. **Stop** and **Start** switch it off and on.

See `docs/networking-host-access.md` for the other ways in.
