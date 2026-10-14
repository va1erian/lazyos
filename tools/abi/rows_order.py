"""The ABI bench rows, in the order `run.py` builds and boots them."""

ORDER = [
    "hello",
    "alloc",
    "hashmap",
    "file",
    "time",
    "thread",
    "syncstress",
    "fsstress",
    "memstress",
    "bigapp",
    "jitmem",
    "filemap",
    "procstress",
    "sigstress",
    "fpsig",
    "avx",
    "threadsig",
    "alarm",
    "epollstress",
    "msgpoll",
    "unixstress",
    "threadreap",
    "seqpollout",
    "scmrights",
    "persist",
    "statxio",
    "cwd",
    "fsops",
    "fdinherit",
    "statmiss",
    "compat",
    # rustls + the nettls provider in memory, through the AVX dispatch where
    # the CPU has it (tools/abi/fixtures/tlsfix, docs/tls-plan.md §8).
    "tlsfix",
    # A dynamically linked musl program, its interpreter and libraries
    # (`tools/abi/dynlink.py`, `LAZYOS_DYNLINK=1`; issue #535).
    "dynlink",
    "busybox",
    # Real programs (`tools/linuxapps/build.py`): one boot of the `linuxapps`
    # fixture, one row per program (see `LINUXAPPS`).
    "dash",
    "sqlite3",
    "jq",
    "rg",
]
