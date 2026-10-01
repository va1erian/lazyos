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

## Topics

```rhai
let s = msg::subscribe("system/confd/changed/sys/#");   // returns a Subscription
let e = s.next(2000);              // wait up to 2 s; () when nothing arrived
print(e.topic + " " + e.payload.path);
msg::publish("demo/rhai/x", "hi");                      // returns how many subscribers it reached
```

- `msg::subscribe(filter[, #{ qos: "buffered", depth: 16 }])` takes `+` (one
  segment) and a trailing `#` (the rest). The QoS (`latest`, `buffered`,
  `conflate`, `reliable`) defaults to the topic's declaration in the IDL, or
  `buffered` for an undeclared topic. `depth` (1 to 64) applies to `buffered`.
- `sub.next()` waits up to `msg::timeout()`, and `sub.next(ms)` waits up to
  `ms` (`0` waits forever). An event is a map with `topic`, `publisher`,
  `sequence`, `retained`, `payload` and `bytes`. On a reliable subscription,
  `sub.ack(event.sequence)` retires events, and `sub.close()` unsubscribes.
- **Payloads.** A topic declared in an IDL file (`topic "..." : Type`) carries
  that type, so `payload` is a map (or an enum's variant name) and
  `msg::publish` encodes your map the same way. Any other topic carries bytes:
  publish a string or blob, and read `payload` as a blob. `bytes` is always
  the raw payload.
- `msg::publish(topic, value, retained)` overrides the declared retained flag.
- On the broker, payloads travel in the same one-field wrapper parcel the
  native services use (`user/src/central.rs`). That lets a script read
  `confd`'s change events, and lets native subscribers read a script's events.

## The event loop

Rhai runs one thing at a time, so a script that reacts to the fabric
registers handlers and then hands control to `msg::run`:

```rhai
msg::on("demo/#", |e| print(e.topic));                     // subscribe + handler
msg::on("jobs/#", #{ qos: "reliable" }, |e| process(e));   // acked after the handler returns
msg::run();          // until msg::stop() is called from a handler
msg::run(10000);     // or for at most 10 s; returns how many events and calls were handled
```

An error thrown by a topic handler ends `msg::run` with that error.

## Writing a service in Rhai

```rhai
msg::serve("demo.rhai", "os.lazy.echo.v1", #{
    Echo: |text, count| { let r = ""; for i in 0..count { r += text } r },
    ping: || true,
});
msg::run();
```

- `msg::serve(name, interface, handlers)` registers `name` with the kernel.
  `msg::serve(interface, handlers)` uses the default name (`os.lazy.echo`).
  Handler keys are IDL method names in either spelling. A key the interface
  does not have is an error, so typos are caught.
- A handler receives the decoded arguments in IDL order. Its return value is
  the reply: the value itself for one return value, or a map for several.
- A handler that throws answers with a structured error, which the caller sees
  as a catchable error. `throw "text"` sends `EIO` with that text.
  `throw #{ code: 13, message: "denied" }` sends a chosen errno.
- A call to a method without a handler is answered with `ENOSYS`. One-way
  methods run their handler and send no reply.
- Names under `os.lazy.` are reserved for system services. An unlabelled
  process may register other names, such as `demo.rhai`.

## In LazyRAD form scripts

The LazyRAD player on LazyOS (`lrplay`, `lazyrad-os/`) registers `msg` as a
LazyRAD script extension (`lazyrad_runtime::extensions`), so every form script
has the module. All the player's forms share one connection to the fabric.

```rhai
fn form_load() {
    let confd = msg::connect("os.lazy.confd.v1");
    info_label.text = "confd: " + confd.info().store_dir;
}
```

The player prints `LRPLAY:MSG:PASS` on serial once `msg` is installed. The
guest check is `tools/screenshot/examples/lazyrad_msg.json`. It writes a small
project to `/tmp/p` from the Terminal and runs it. The form sets a confd key
over `msg`, and a separate `rhai` script reads the key back
(`RHAI:lrmsg:ok`).

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
  desktop and runs two sessions in the Terminal.
  `tools/screenshot/examples/rhai_msg.json` covers: list services, `confd`
  `Info`, a `Set`/`Get` round trip, the topics broker, a refused call and a
  missing service. `rhai_msg_loop.json` covers: a topic round trip through the
  broker, a `confd` change event reaching a script, and a service written in
  Rhai, run in the background, answering another script.
- Generated table: `python tools/midlc/midlc.py --check --schema
  libs/rhai-lazy/src/msg/idl.rs idl/*.midl` (CI runs it).
