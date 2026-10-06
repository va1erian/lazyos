// A test app whose start-up always fails (issue #549): `form_load` throws, so
// the player exits with its fatal runtime code (2). Packaged as the user
// package `org.lazy.crashload` (`tools/lazyrad/package.py --app crashload`)
// for `tools/screenshot/examples/app_crash_notice.json`: `init` must not
// restart it, and the desktop must show one notice with this reason.

fn form_load() {
    throw "crash test: form_load always fails";
}
