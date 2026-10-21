// title: Open-with oracle, then remote control
// tags: do, mimed, apps
// needs: none
// expect: /system/share/samples/lazyos-sample.pdf is application/pdf; verbs: ["open"]; opens with os.lazy.pdf
//
// Ask mimed what would open a file, then open it for real: the PDF viewer
// appears on your desktop.
let path = "/system/share/samples/lazyos-sample.pdf";
let mime = sys::mimed::guess(path);
print(`${path} is ${mime}; verbs: ${sys::mimed::verbs(mime)}; opens with ${sys::mimed::lookup(mime, "open")}`);
sys::mimed::open(path, "open");          // the PDF viewer appears
