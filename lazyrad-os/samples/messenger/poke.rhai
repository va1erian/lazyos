// Drives the Messenger sample from outside: `rhai /system/share/lazyrad/messenger/poke.rhai`
// while the form runs (as root: `sys/` is root-writable). It waits for the
// form's service, changes the confd key the form watches, then calls the
// service, and prints one serial-checkable line (the desktop Terminal reports
// one line per command).

let form = ();
for attempt in 0..120 {
    try {
        form = msg::connect("os.lazy.echo.v1", "demo.lazyrad");
        break;
    } catch {
        os::sleep(250);
    }
}
if form == () {
    print("RHAI:lrpoke:no-service");
    os::exit(1);
}

let value = sys::confd::new_value();
value.kind = 3;
value.str_value = "poked";
sys::confd::set("sys/lazyrad/demo", value);
let stored = sys::confd::get("sys/lazyrad/demo").str_value;
let reply = form.echo("hi", 2);
print("RHAI:lrpoke:" + stored + ":" + reply);
