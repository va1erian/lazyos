// title: QoS laboratory
// tags: listen, topics, qos
// needs: none
// expect: buffered: 8 events, last n99
// expect: conflate: 1 events, last n99
// expect: latest: 1 events, last n99
// expect: reliable: 8 events, last n99
//
// Four subscriptions to one topic, one burst of a hundred events. The
// difference between latest, buffered, conflate and reliable is the output.
// A reliable subscription redelivers its head until it is acked, so the loop
// acks what it reads.
let subs = #{
    latest:   msg::subscribe("demo/pg/burst", #{ qos: "latest" }),
    buffered: msg::subscribe("demo/pg/burst", #{ qos: "buffered", depth: 8 }),
    conflate: msg::subscribe("demo/pg/burst", #{ qos: "conflate" }),
    reliable: msg::subscribe("demo/pg/burst", #{ qos: "reliable" }),
};
for i in 0..100 { msg::publish("demo/pg/burst", `n${i}`); }
for name in subs.keys() {
    let n = 0; let last = "";
    loop {
        let e = subs[name].next(20);
        if e == () { break; }
        n += 1;
        last = e.bytes.as_string();
        if name == "reliable" { subs[name].ack(e.sequence); }
    }
    print(`${name}: ${n} events, last ${last}`);
}
for name in subs.keys() { subs[name].close(); }
