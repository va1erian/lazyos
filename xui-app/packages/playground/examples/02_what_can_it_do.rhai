// title: What can this interface do?
// tags: explore, describe
// needs: none
// expect: os.lazy.confd.v1 (service os.lazy.confd)
// expect: ----
// expect: os.lazy.timed.v1 (service os.lazy.timed)
// expect: ----
//
// msg::describe for every interface that is live right now. A service is
// registered under the interface name without ".vN" or, for some, with it, so
// both spellings are tried.
let live = msg::services();
for name in msg::interfaces() {
    let base = name.sub_string(0, name.len() - 3);   // drop ".v1"
    if live.contains(base) || live.contains(name) {
        print(msg::describe(name));
        print("----");
    }
}
