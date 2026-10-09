"""Seeds for the `dbgwire` fuzz target (`gen_corpus.py`): the requests, the
configuration and the boot-log lines `dbgd` reads (docs/dbgd-plan.md)."""


def dbgwire_seeds():
    return {
        "log_tail": b'{"jsonrpc":"2.0","id":1,"method":"log.tail","params":{"lines":20,"source":"usbd"}}',
        "auth": b'{"jsonrpc":"2.0","id":2,"method":"auth","params":{"mac":"' + b"ab" * 32 + b'"}}',
        "notification": b'{"jsonrpc":"2.0","method":"log.unfollow"}',
        "fs_read": b'{"jsonrpc":"2.0","id":"x","method":"fs.read","params":{"path":"/transient/usbd.dump","offset":0,"len":4096}}',
        "bad_version": b'{"jsonrpc":"1.0","id":1,"method":"ping"}',
        "deep": b"[" * 12 + b"]" * 12,
        "dup_key": b'{"a":1,"a":2}',
        "unicode": rb'{"jsonrpc":"2.0","id":1,"method":"\ud83d\ude00"}',
        "cfg": b"root=UUID=0b0c8d3a-5b1e-4a53-9d77-0123456789ab\ndiag.dbg=1\ndiag.dbg.key="
               + b"00" * 16 + b"\ndiag.dbg.port=9701\ndiag.dbg.peer=10.0.2.2\n",
        "logline": b'[12.345] USBD:DIAG port=1 timeout slot=Enabled/addr1 ep0state=3 ep0dq=0x1f000 note="two words"',
        "hwline": b"HW:IRQCHIP:ioapic pins=24 dest=0 pci_lines=0x0fff",
        "empty": b"",
    }
