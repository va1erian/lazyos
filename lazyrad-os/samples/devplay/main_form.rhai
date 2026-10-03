// The development-run sample (issue #529, docs/lazyrad-package-plan.md
// section 3). A packaged IDE plays it under `dev:<system_name>`: the
// `sys::confd` call below is allowed only because Make LazyOS App's derivation
// grants `os.lazy.confd.v1` to the package, and the kernel judges the player
// by exactly that. The lines it prints reach the IDE's run console (and
// `lazyrad --play-dev` copies them to serial as `LRIDE:PLAY:OUT:`).

fn form_load() {
    let theme = sys::confd::get("sys/ui/theme");
    let value = if theme == () { "unset" } else { theme.str_value };
    theme_label.text = `theme: ${value}`;
    print(`DEVPLAY:CONFD:PASS theme=${value}`);
    print("DEVPLAY:DONE");
    app.quit();
}
