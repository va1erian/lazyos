// title: Read a retained value without subscribing for good
// tags: explore, topics, retained
// needs: none
// expect: #{"detail": "all services up", "name": "summary", "status": "ok", "tick": 42}
//
// system/health/summary is retained, so a one-event subscription returns the
// current state at once. This is what the Peek button does.
let s = msg::subscribe("system/health/summary");
let e = s.next(1000);
print(if e == () { "no health record yet" } else { e.payload });
s.close();
