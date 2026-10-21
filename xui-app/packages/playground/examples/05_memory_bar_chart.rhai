// title: Memory as a bar chart
// tags: listen, topics, stats
// needs: none
// expect: heap [##########..............................] 1024 KiB
//
// sysmond publishes MemoryStats (retained); draw the kernel heap's use as a
// bar of forty cells for thirty seconds.
sys::sysmond::on_stats_memory(|e| {
    let m = e.payload;
    let total = if m.heap_total == 0 { 1 } else { m.heap_total };
    let pct = m.heap_used * 40 / total;
    let bar = "";
    for i in 0..40 { bar += if i < pct { "#" } else { "." } }
    print(`heap [${bar}] ${m.heap_used / 1024} KiB`);
});
msg::run(30000);
