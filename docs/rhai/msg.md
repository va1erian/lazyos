# `msg`: Messenger from Rhai

The `rhai` command can call every Messenger service on LazyOS. Nothing about a
service is written by hand: `midlc --schema` compiles `idl/*.midl` into a table
of interfaces (`libs/rhai-lazy/src/msg/idl.rs`), and the `msg` module encodes
calls and decodes replies from it. A new or changed `.midl` file becomes
scriptable as soon as the table is regenerated.

```bash
rhai -e 'msg::connect("os.lazy.confd.v1").info()'
#{"persistent": true, "store_dir": "/data/confd"}
```

The module exists only when `rhai` runs on LazyOS (it checks the kernel name),
so a host build of the command has no `msg`.

## Discovering services

| Call | Returns |
|---|---|
| `msg::interfaces()` | every interface name compiled from `idl/` |
| `msg::services()` | every name registered with the kernel right now, sorted |
| `msg::describe("os.lazy.echo.v1")` | the interface's doc, method signatures and topics, as text |
| `svc.methods` | the method names of a connected service |
| `svc.interface`, `svc.service` | the interface and the registered name it resolved to |

## Calling

```rhai
let confd = msg::connect("os.lazy.confd.v1");          // or connect(interface, service_name)
confd.info().store_dir;                                 // method sugar: snake_case name
confd.set("sys/test/x", #{ kind: 3, str_value: "hi" }); // positional arguments
confd.get(#{ path: "sys/test/x" });                     // one map = named arguments
confd.invoke("Get", ["sys/test/x"]);                    // generic: IDL name + array
confd.invoke("Info");                                   // generic, no arguments
```

- `msg::connect(interface)` resolves the service once and keeps the endpoint
  for the life of the process. It tries the interface name without its `.vN`
  suffix (`os.lazy.confd`), then the full name (`os.lazy.display.v1`). If the
  name is something else (`os.lazy.accountsd`), pass it:
  `msg::connect("os.lazy.accounts.v1", "os.lazy.accountsd")`.
- A reply with one value returns that value, a reply with several returns a
  map, and an empty reply returns `()`.
- One-way methods (`oneway` in the IDL) are sent, not called, and return `()`
  once queued. `svc.invoke_oneway("Notify", args)` refuses a method that has a
  reply, so a reply is never dropped by accident.
- A method whose snake_case name is one of `invoke`, `invoke_oneway`, `call`,
  `interface`, `service`, `methods` or `type_of` is reachable only through
  `invoke`.
- `msg::set_timeout(ms)` sets how long a call waits for its reply (default
  5000; `0` waits forever), and `msg::timeout()` reads it.

## Values

| IDL | Rhai |
|---|---|
| `Bool` | `bool` |
| `I32`, `U32`, `I64` | `int` (checked against the type's range when sent) |
| `U64` | `int`. The 64-bit pattern is kept, so ids above `i64::MAX` read back negative and round-trip unchanged |
| `F64` | `float` (an `int` is accepted when sending) |
| `String` | `string` |
| `Bytes` | `blob` (a `string` is accepted as its UTF-8 bytes) |
| `Array<T>` | `array` |
| `Option<T>` | `()` for none, otherwise the value |
| struct | object map. Missing fields take their zero value and unknown keys are an error, so typos are caught |
| enum | the variant name as a string (an `int` index is accepted when sending) |
| `Handle`, `Buffer` | received as `int` or a map; scripts cannot send them |

## Errors

Every failure is an ordinary Rhai error that `try`/`catch` can handle, phrased
with the interface and method:

```rhai
try {
    msg::connect("os.lazy.confd.v1").get("nope/x");
} catch (e) {
    print(e);   // os.lazy.confd.v1.Get: that is not a valid confd path (<ERRNO>, code <n>)
}
```

- **Refused by the service:** the service's own text plus the errno name
  (`EACCES` for a permission refusal). Scripts run with their process's
  credentials. There is no ambient authority, so a script can do exactly what
  its process could do through the compiled clients.
- **Fabric failures:** no such service, timed out, the service went away. A
  service that restarts is resolved again on the next call.
- **Bad arguments:** wrong count, wrong type, out of range, an unknown field or
  enum variant. These are reported before anything is sent.

## Testing

- Host: `cargo test --manifest-path libs/rhai-lazy/Cargo.toml` runs the module
  against an in-memory fabric whose services use the compiled
  `messenger-generated` codecs, so the schema codec is cross-checked against
  the compiled one.
- Guest: `python tools/rhai/run.py --desktop` (or `--msg-only`) boots the
  desktop and runs `tools/screenshot/examples/rhai_msg.json` in the Terminal:
  list services, `confd` `Info`, a `Set`/`Get` round trip, the topics broker,
  a refused call and a missing service.
- Generated table: `python tools/midlc/midlc.py --check --schema
  libs/rhai-lazy/src/msg/idl.rs idl/*.midl` (CI runs it).
