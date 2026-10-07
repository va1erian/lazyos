# Network Drives

Network Drives mounts an FTP server as an ordinary folder under `/mnt`, so
every app (Files, the Terminal, the editors) reads and writes the server's
files as if they were on the disk.

* **Connect to an FTP server**: type the server's name or address, its port
  (empty is 21), a user name and password (leave both empty for an anonymous
  login) and a short name for the folder (`a-z`, `0-9`, `-`, `_`; empty is
  `ftp`). **Mount** starts the connection; the folder appears at
  `/mnt/<name>` once the server has accepted the login.
* **Mounted folders** lists every mount with its state: *Connecting...*,
  *Mounted*, or *Failed* with the reason (a wrong password, an unknown host,
  a server that does not answer). **Open in Files** (or a double-click) opens
  a mounted folder; **Unmount** disconnects it, or clears a failed one.

The files belong to you while the folder is mounted. The password is kept only
by the connection that uses it, never saved.

FTP has no way to write into the middle of a file, so changing part of a large
file sends the whole file again, and a change someone else makes on the
server may not show until the folder is mounted again. Under QEMU the host
is `10.0.2.2`.
