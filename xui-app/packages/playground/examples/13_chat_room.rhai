// title: Chat room in a topic
// tags: build, topics, retained
// needs: second run
// expect: alice joined
// expect: bob: hello
//
// Two runs (or two Playground windows) are two users. A retained topic means
// a late joiner sees the last line. Change me to "bob" in the other run and
// type more lines with the Publish form. Runs until you press Stop.
let me = "alice";                          // change to "bob" in the other run
msg::on("demo/pg/chat/lobby", |e| print(e.payload.as_string()));
msg::publish("demo/pg/chat/lobby", `${me} joined`, true);
msg::run();
