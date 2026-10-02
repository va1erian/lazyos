// Drives the Messenger sample from outside: `rhai /LAZYRAD/messenger/poke.rhai`
// while the form runs (as root: `sys/` is root-writable). It waits for the
// form's service, changes the confd key the form watches, then calls the
// service, and prints serial-checkable results.

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
    print("RHAI:lrserve:none");
    os::exit(1);
}

let value = sys::confd::new_value();
value.kind = 3;
value.str_value = "poked";
sys::confd::set("sys/lazyrad/demo", value);
print("RHAI:lrset:" + sys::confd::get("sys/lazyrad/demo").str_value);

print("RHAI:lrserve:" + form.echo("hi", 2));
