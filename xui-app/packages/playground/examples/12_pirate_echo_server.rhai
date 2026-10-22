// title: A service written in Rhai (run 1: serve)
// tags: build, serve
// needs: second run
// expect: serving demo.pirate
//
// Run 1 serves the echo interface under a name of your own (names under
// os.lazy. are reserved). Leave it running and call it from run 2
// (12b_pirate_echo_client), or from the Terminal with rhai -e.
msg::serve("demo.pirate", "os.lazy.echo.v1", #{
    Echo: |text, count| { let r = ""; for i in 0..count { r += text.to_upper() + "! ARR. " } r },
    Ping: || true,
});
print("serving demo.pirate"); msg::run();
