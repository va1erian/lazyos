// title: Config spy
// tags: listen, topics, confd
// needs: none
// expect: sys/ui/theme -> *
//
// Print every change under sys/ui/ while you drag a slider or flip the theme
// in Settings. The event only names the path (it never carries the value), so
// the handler reads it back. Runs until you press Stop.
sys::confd::on_changed("sys/ui/#", |e| {
    let v = sys::confd::get(e.payload.path);
    print(`${e.payload.path} -> ${if e.payload.deleted || v == () { "(deleted)" } else { v }}`);
});
msg::run();
