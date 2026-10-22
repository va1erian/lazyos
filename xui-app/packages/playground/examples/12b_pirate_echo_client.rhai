// title: A service written in Rhai (run 2: call)
// tags: build, connect
// needs: second run
// after: 12_pirate_echo_server
// expect: AHOY! ARR. AHOY! ARR.
//
// Run 2 calls the service run 1 serves, by the name it registered.
print(msg::connect("os.lazy.echo.v1", "demo.pirate").echo("ahoy", 2));
