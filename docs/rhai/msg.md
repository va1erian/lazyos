# `msg`: Messenger from Rhai

The `rhai` command can call every Messenger service on LazyOS. Nothing about a
service is written by hand: `midlc --schema` compiles `idl/*.midl` into a table
of interfaces (`libs/rhai-lazy/src/msg/idl.rs`), and the `msg` module encodes
calls and decodes replies from it. A new or changed `.midl` file becomes
scriptable as soon as the table is regenerated.

```bash
rhai -e 'msg::connect("os.lazy.confd.v1").info()'
#{"persistent": true, "store_dir": "/conf"}
```

The module exists only when `rhai` runs on LazyOS (it checks the kernel name),
so a host build of the command has no `msg`.

## Generated modules (`sys::*`)

`midlc --rhai-api` also writes one Rhai module per interface, so a script can
use named, documented functions instead of strings. `sys::<alias>` is the
interface name without `os.lazy.` and `.vN`:

```rhai
sys::confd::get("sys/ui/theme");                // = msg::connect("os.lazy.confd.v1").get(...)
let v = sys::confd::new_value();                // a struct with every field at its zero value
v.kind = 3; v.str_value = "hi";
sys::confd::set("sys/test/x", v);
sys::confd::on_changed("sys/ui/#", |e| print(e.payload.path));   // a typed topic helper
sys::messenger_topics::QOS_RELIABLE;            // enum variants are constants
```

Every function is one call into `msg`, so everything below (values, errors,
timeouts) applies unchanged. The reference, generated with the modules, is
[`libs/rhai-lazy/api/README.md`](../../libs/rhai-lazy/api/README.md); each
module's `.rhai` source sits next to it. Methods whose request carries a
channel, buffer or ring are not generated (a script cannot create those), and
neither are the kernel ACL scopes. Regenerate after an IDL change with
`python tools/midlc/midlc.py --rhai-api libs/rhai-lazy/api idl/*.midl`.

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

An error thrown by a topic handler ends `msg::run` with that error. The
event still counts as delivered: a reliable event is acked first, because
redelivering it would only fail again.

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

The LazyRAD player on LazyOS (`lrplay`, `lazyrad-os/`) gives every form script
`msg` and `sys::*` (`docs/lazyrad-messenger-plan.md`). All the player's forms
share one connection to the fabric, and the generated modules are compiled
once per process.

```rhai
fn form_load() {
    theme_label.text = sys::confd::get("sys/ui/theme").str_value;
    sys::confd::on_changed("sys/ui/#", |e| theme_label.text = e.payload.path);
    msg::serve("demo.lazyrad", "os.lazy.echo.v1", #{ Ping: || true });
}
```

- **Events need no loop.** `on_*` and `msg::on` handlers, and calls to a service
  the form serves, run on the form's window: while the form has something
  registered, its window polls the fabric every 50 ms without blocking and
  runs what is ready in the form's script. `msg::run()` is an error in a form.
- **Errors** in a handler are shown like an event handler's error (a message
  box) and the program keeps running. A handler that fails on every event is
  shown once; the repeats are counted on stderr.
- **Closing a form** unsubscribes its topics and withdraws the names it served.
- **Blocking calls** still block the window: a call waits up to
  `msg::timeout()` (5 s) for a stuck service, so keep slow work out of click
  handlers or lower the timeout.
- **Installed apps.** Make LazyOS App declares the interfaces and topics the
  scripts use (`sys::<alias>::...` and literal `msg::connect`, `msg::on`,
  `msg::subscribe`, `msg::publish` arguments; `rhai_lazy::msg::permissions`),
  so the app's kernel rules allow exactly those. A name built at run time
  cannot be seen; the consent screen lists what was found. A topic helper's
  literal arguments narrow its rule (`sys::confd::on_changed("sys/ui/#", ..)`
  declares `subscribe:system/confd/changed/sys/ui/#`); one built at run time
  leaves the bare pattern, which the kernel's per-segment check refuses past
  its wildcard. Serving a name is a development feature: an installed app's
  manifest cannot grant it, so wrap `msg::serve` in `try`/`catch` (as the
  sample does). An error escaping `form_load` ends the app, and `init`
  restarts it.

The player prints `LRPLAY:MSG:PASS` on serial once `msg` and `sys` are
installed, and `LRPLAY:MSGEVENT:PASS` after the first Messenger handler ran in
a form. The sample is `lazyrad-os/samples/messenger` (`/system/share/lazyrad/messenger` in
a `--lazyrad` image) and the guest check is `python tools/rhai/run.py
--lazyrad` (`tools/screenshot/examples/lazyrad_msg.json`): the form starts,
`poke.rhai` changes the confd key it watches and calls the service it serves.

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
- Generated table and modules: `python tools/midlc/midlc.py --check --schema
  libs/rhai-lazy/src/msg/idl.rs --rhai-api libs/rhai-lazy/api idl/*.midl`
  (CI runs it), and `python tools/midlc/test_midlc_rhai.py`.
- LazyRAD: `python tools/rhai/run.py --lazyrad` (above); the host side is
  `cd lazyrad-os && cargo test` and LazyRAD's `crates/lazyrad-runtime/tests/events.rs`.
