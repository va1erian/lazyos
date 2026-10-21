# Messenger Playground

Browse the Messenger interfaces and topics of a running LazyOS, build a call
from a form or a generated snippet, and run it as a Rhai script.

This is a **developer tool**: it runs with your own authority, like the
Terminal. The limits bind it, its scripts and the programs it starts alike. A
call through the system's services (Messenger) can do anything you can do
except use passwords, keys, accounts and the label policy loader. The network
is denied unless the package lists it: sockets, name lookups, ping and
file-server mounts are refused, for its own programs as well as for scripts
(the kernel refuses a socket a tool without the network permission opens).
Files, and starting other programs, stay your authority: they run with your
permissions. Read a script before you run it, especially one you did not write.

Only images built with `LAZYOS_DEVTOOLS=1` ship it (`python tools/run_demo.py
--devtools`). See `docs/messenger-playground-plan.md` and
`docs/devtool-packages-plan.md`.
