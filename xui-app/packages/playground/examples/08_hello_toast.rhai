// title: Hello, toast
// tags: do, notify
// needs: none
// expect: posted toast 1
//
// One call, one notification; then a progress toast that replaces itself (the
// call returns the toast id and replaces_id reuses it). The toast shows who
// sent it, as the system identifies it. This changes only what is on screen.
let id = sys::notify::notify("Info", "Hello from the Playground", "Messenger works.", (), [], (), (), ());
for p in [10, 40, 70, 100] {
    sleep(500);
    id = sys::notify::notify("Info", "Working", `${p}%`, (), [], p, id, ());
}
print(`posted toast ${id}`);
