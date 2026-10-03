// Starts the development-run test package on this project, the way the Start
// menu starts an installed app (`init.Launch`), so it runs under its own
// `app:org.lazy.test.lrdev` label: `rhai /system/share/lazyrad/devplay/launch.rhai`
// (tools/screenshot/examples/lazyrad_devplay.json). Not part of the project:
// the .lrp does not reference it, so Make LazyOS App never packages it.

let init = msg::connect("os.lazy.init.v1");
let started = init.launch("org.lazy.test.lrdev", "/system/share/lazyrad/devplay", 0);
print(`DEVPLAY:LAUNCH:PASS pid=${started.pid}`);
