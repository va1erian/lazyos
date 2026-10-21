# Messenger Playground

Browse the Messenger interfaces and topics of a running LazyOS, build a call
from a form or a generated snippet, and run it as a Rhai script.

This is a **developer tool**: it runs with your own authority, like the
Terminal. The limits bind its calls through the system's services (Messenger):
a call from it, or from a script it runs, can do anything you can do except use
passwords, keys, accounts and the label policy loader, and open sockets through
the network service unless the package lists them. Its own programs run with
your permissions, so those limits do not confine native code. Read a script
before you run it, especially one you did not write.

Only images built with `LAZYOS_DEVTOOLS=1` ship it (`python tools/run_demo.py
--devtools`). See `docs/messenger-playground-plan.md` and
`docs/devtool-packages-plan.md`.
