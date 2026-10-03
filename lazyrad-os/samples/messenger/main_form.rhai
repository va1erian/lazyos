// Messenger from a LazyRAD form (docs/lazyrad-messenger-plan.md).
//
// Every `sys::*` function is generated from idl/*.midl; see
// libs/rhai-lazy/api/README.md for the full list. Handlers registered with
// `on_*` and `msg::serve` run on this window while it is open.

fn form_load() {
    let info = sys::confd::info();
    store_label.text = `confd: ${info.store_dir} (persistent: ${info.persistent})`;

    let theme = sys::confd::get("sys/ui/theme");
    theme_label.text = if theme == () { "theme: (unset)" } else { `theme: ${theme.str_value}` };

    // `system/confd/changed/sys/lazyrad/#`: every change under sys/lazyrad.
    sys::confd::on_changed("sys/lazyrad/#", |event| {
        let change = event.payload;
        let now = sys::confd::get(change.path);
        let text = if change.deleted || now == () { "deleted" } else { now.str_value };
        event_label.text = `changed ${change.path} = ${text}`;
    });

    serve_echo();
}

// Another program can call this form: msg::connect("os.lazy.echo.v1",
// "demo.lazyrad").echo("hi", 2) answers "hihi". Serving a name is a
// development feature: run as an installed app, the kernel refuses it, and an
// error escaping form_load would end the app.
fn serve_echo() {
    form.state.calls = 0;
    try {
        msg::serve("demo.lazyrad", "os.lazy.echo.v1", #{
            Echo: |text, count| {
                form.state.calls += 1;
                served_label.text = `served Echo("${text}", ${count}) (call ${form.state.calls})`;
                let reply = "";
                for i in 0..count {
                    reply += text;
                }
                reply
            },
            Ping: || true,
        });
    } catch (error) {
        served_label.text = `not serving demo.lazyrad: ${error}`;
    }
}

fn set_button_click() {
    let value = sys::confd::new_value();
    value.kind = 3;
    value.str_value = `clicked at ${now()}`;
    try {
        sys::confd::set("sys/lazyrad/demo", value);
        status_label.text = "set sys/lazyrad/demo";
    } catch (error) {
        // sys/ is writable by root only.
        status_label.text = `${error}`;
    }
}
