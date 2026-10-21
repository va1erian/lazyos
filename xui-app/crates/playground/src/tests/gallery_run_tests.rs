//! Each Gallery example run against the in-memory fabric and judged by its
//! own `// expect:` lines (see `gallery_tests` for the manifest).

use rhai::{Dynamic, Map};
use rhai_lazy::Outcome;

use crate::gallery::{self, check_expectations, Example};
use crate::testkit::bus::s;
use crate::testkit::services::{add_pirate, gallery_world, publish_bytes, publish_declared, World};
use crate::testkit::{Lab, Ran};

fn example(id: &str) -> Example {
    gallery::find(id).unwrap_or_else(|| panic!("no example {id}"))
}

fn entries(pairs: Vec<(&str, Dynamic)>) -> Dynamic {
    Dynamic::from_map(
        pairs
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect::<Map>(),
    )
}

/// Run `id` over the Gallery world. `ticks` is how many idle waits the
/// fabric allows after the scenario is spent.
fn run(id: &str, ticks: u32, scenario: impl FnOnce(&World)) -> (Ran, World, Example) {
    let ex = example(id);
    let world = gallery_world();
    scenario(&world);
    world.bus.set_idle_budget(ticks);
    let lab = Lab::over(world.bus.clone());
    lab.compile(ex.source)
        .unwrap_or_else(|e| panic!("{id} does not compile: {e}"));
    let ran = lab.run(ex.source);
    (ran, world, ex)
}

/// The run ended because the scenario was spent, as a script that waits
/// until Stop does here.
fn stopped_by_harness(ran: &Ran) -> bool {
    matches!(&ran.outcome, Outcome::Failed(m) if m.contains("harness: the script is idle"))
}

fn judged(ran: &Ran, ex: &Example) {
    if ex.runs_until_stopped() {
        assert!(stopped_by_harness(ran), "{}: {:?}", ex.id, ran.outcome);
    } else {
        ran.ok();
    }
    check_expectations(&ex.expect, &ran.lines).unwrap_or_else(|missing| {
        panic!(
            "{}: expected a line matching `{missing}`\noutput:\n{}",
            ex.id,
            ran.lines.join("\n")
        )
    });
}

#[test]
fn example_01_lists_the_registry_sorted() {
    let (ran, _, ex) = run("01_whos_on_the_fabric", 3, |_| {});
    judged(&ran, &ex);
    let names: Vec<_> = ran
        .lines
        .iter()
        .map(|l| l.split("  ").next().unwrap().to_string())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert!(names.len() > 8);
}

#[test]
fn example_02_describes_the_live_interfaces_only() {
    let (ran, world, ex) = run("02_what_can_it_do", 3, |_| {});
    judged(&ran, &ex);
    let heads = ran
        .lines
        .iter()
        .filter(|l| l.contains(" (service "))
        .count();
    let live = world.bus.registry_rows().len();
    assert!(
        heads >= 8 && heads <= live,
        "{heads} described, {live} registered"
    );
    assert!(
        !ran.lines.iter().any(|l| l.starts_with("os.lazy.echo.v1")),
        "echo is not served"
    );
}

#[test]
fn example_03_reads_the_retained_summary() {
    let (ran, _, ex) = run("03_read_a_retained_value", 3, |_| {});
    judged(&ran, &ex);
    assert_eq!(ran.lines.len(), 1);
}

#[test]
fn example_03_says_so_when_nothing_is_retained() {
    let ex = example("03_read_a_retained_value");
    let bus = crate::testkit::FakeBus::new();
    bus.set_idle_budget(5);
    let ran = Lab::over(bus).run(ex.source);
    ran.ok();
    assert_eq!(ran.lines, ["no health record yet"]);
}

#[test]
fn example_04_prints_the_retained_tick() {
    let (ran, _, ex) = run("04_heartbeat", 200, |_| {});
    judged(&ran, &ex);
}

#[test]
fn example_04_prints_a_new_tick_when_the_zone_changes() {
    let (ran, _, ex) = run("04_heartbeat", 200, |w| {
        w.bus.on_idle(|bus| {
            publish_declared(
                bus,
                "time/tick",
                12,
                &entries(vec![
                    ("unix", Dynamic::from_int(1_700_000_060)),
                    ("offset", Dynamic::from_int(0)),
                    ("zone_name", s("UTC")),
                ]),
            );
        });
    });
    judged(&ran, &ex);
    assert_eq!(
        ran.lines,
        ["tick 1700000000 (Europe/Paris)", "tick 1700000060 (UTC)"]
    );
}

#[test]
fn example_05_draws_the_heap_bar() {
    let (ran, _, ex) = run("05_memory_bar_chart", 400, |_| {});
    judged(&ran, &ex);
    assert_eq!(ran.lines.len(), 1);
}

#[test]
fn example_05_survives_an_empty_heap_total() {
    let ex = example("05_memory_bar_chart");
    let world = gallery_world();
    world
        .bus
        .broker
        .borrow_mut()
        .publish("noise", vec![], false, 1);
    publish_declared(
        &world.bus,
        "system/stats/memory",
        13,
        &entries(vec![
            ("heap_used", Dynamic::from_int(0)),
            ("heap_total", Dynamic::from_int(0)),
        ]),
    );
    world.bus.set_idle_budget(400);
    let ran = Lab::over(world.bus.clone()).run(ex.source);
    ran.ok();
    assert_eq!(
        ran.lines,
        ["heap [........................................] 0 KiB"]
    );
}

#[test]
fn example_06_reports_a_change_it_reads_back() {
    let (ran, _, ex) = run("06_config_spy", 3, |w| {
        w.bus.on_idle(|bus| {
            publish_declared(
                bus,
                "system/confd/changed/sys/ui/theme",
                9,
                &entries(vec![
                    ("path", s("sys/ui/theme")),
                    ("deleted", Dynamic::from_bool(false)),
                ]),
            );
        });
        w.bus.on_idle(|bus| {
            publish_declared(
                bus,
                "system/confd/changed/sys/ui/gone",
                9,
                &entries(vec![
                    ("path", s("sys/ui/gone")),
                    ("deleted", Dynamic::from_bool(true)),
                ]),
            );
        });
    });
    judged(&ran, &ex);
    assert_eq!(ran.lines.len(), 2);
    assert!(
        ran.lines[0].starts_with("sys/ui/theme -> #{"),
        "{:?}",
        ran.lines
    );
    assert!(ran.lines[0].contains("\"dark\""));
    assert_eq!(ran.lines[1], "sys/ui/gone -> (deleted)");
}

#[test]
fn example_07_shows_what_each_qos_keeps() {
    let (ran, _, ex) = run("07_qos_laboratory", 200, |_| {});
    judged(&ran, &ex);
    assert_eq!(
        ran.lines,
        [
            "buffered: 8 events, last n99",
            "conflate: 1 events, last n99",
            "latest: 1 events, last n99",
            "reliable: 8 events, last n99",
        ]
    );
}

#[test]
fn example_08_posts_a_toast_and_updates_it_in_place() {
    let (ran, world, ex) = run("08_hello_toast", 3, |_| {});
    judged(&ran, &ex);
    let toasts = world.logged("notify:");
    assert_eq!(toasts.len(), 5);
    assert_eq!(
        toasts[0],
        "notify:Info:Hello from the Playground:Messenger works.:progress=-1:id=1"
    );
    let progress: Vec<_> = toasts[1..]
        .iter()
        .map(|t| t.split("progress=").nth(1).unwrap().to_string())
        .collect();
    assert_eq!(progress, ["10:id=1", "40:id=1", "70:id=1", "100:id=1"]);
    assert!(toasts[4].contains(":Working:100%:"));
}

#[test]
fn example_09_asks_the_oracle_then_opens_the_file() {
    let (ran, world, ex) = run("09_open_with_oracle", 3, |_| {});
    judged(&ran, &ex);
    assert_eq!(
        world.logged("mimed:"),
        ["mimed:open /system/share/samples/lazyos-sample.pdf open"]
    );
}

#[test]
fn example_10_ramps_the_volume_and_puts_it_back() {
    let (ran, world, ex) = run("10_volume_wave", 3, |_| {});
    judged(&ran, &ex);
    let sets = world.logged("mixer:");
    assert_eq!(sets.len(), 22);
    assert_eq!(sets[0], "mixer:set_master 0");
    assert_eq!(sets[10], "mixer:set_master 65536");
    assert_eq!(sets[20], "mixer:set_master 0");
    assert_eq!(sets[21], "mixer:set_master 32768", "put back");
}

#[test]
fn example_11_turns_supervision_events_into_toasts() {
    let (ran, world, ex) = run("11_service_watchdog", 3, |w| {
        w.bus.on_idle(|bus| {
            publish_declared(
                bus,
                "system/events/service/inputd",
                1,
                &entries(vec![
                    ("state", s("restarting")),
                    ("pid", Dynamic::from_int(0)),
                    ("restarts", Dynamic::from_int(1)),
                    ("status", Dynamic::from_int(0)),
                ]),
            );
        });
    });
    judged(&ran, &ex);
    assert_eq!(
        world.logged("notify:"),
        ["notify:Warn:service system/events/service/inputd:restarting (restarts: 1):progress=-1:id=1"]
    );
}

#[test]
fn example_11b_asks_elevd_to_restart_a_service() {
    let (ran, world, ex) = run("11b_service_watchdog_trigger", 3, |_| {});
    judged(&ran, &ex);
    assert_eq!(
        world.logged("elevd:"),
        ["elevd:request service.restart [\"inputd\"]"]
    );
}

#[test]
fn example_11b_prints_a_refusal_instead_of_failing() {
    let ex = example("11b_service_watchdog_trigger");
    let world = gallery_world();
    // No elevd at all: the call is refused and the script says so.
    let bus = crate::testkit::FakeBus::new();
    let _ = world;
    let ran = Lab::over(bus).run(ex.source);
    ran.ok();
    assert!(ran.lines[0].starts_with("refused: "), "{:?}", ran.lines);
}

#[test]
fn example_12_serves_the_pirate_echo() {
    use std::cell::Cell;
    use std::rc::Rc;
    let echo = Rc::new(Cell::new(0));
    let ping = Rc::new(Cell::new(0));
    let (e, p) = (echo.clone(), ping.clone());
    let (ran, world, ex) = run("12_pirate_echo_server", 3, move |w| {
        w.bus.on_idle(move |bus| {
            let mut args = Map::new();
            args.insert("text".into(), s("ahoy"));
            args.insert("count".into(), Dynamic::from_int(2));
            e.set(bus.inject_request("demo.pirate", "Echo", args));
            p.set(bus.inject_request("demo.pirate", "Ping", Map::new()));
        });
    });
    judged(&ran, &ex);
    let reply = world
        .bus
        .reply_to("demo.pirate", "Echo", echo.get())
        .unwrap();
    assert_eq!(
        reply["reply"].clone().into_string().unwrap(),
        "AHOY! ARR. AHOY! ARR. "
    );
    let alive = world
        .bus
        .reply_to("demo.pirate", "Ping", ping.get())
        .unwrap();
    assert!(alive["alive"].as_bool().unwrap());
}

#[test]
fn example_12_ignores_a_one_way_method_it_has_no_handler_for() {
    use std::cell::Cell;
    use std::rc::Rc;
    let txn = Rc::new(Cell::new(0));
    let t = txn.clone();
    let (ran, world, ex) = run("12_pirate_echo_server", 3, move |w| {
        w.bus.on_idle(move |bus| {
            t.set(bus.inject_request("demo.pirate", "Notify", Map::new()));
        });
    });
    judged(&ran, &ex);
    // Notify is one-way and has no handler: nothing is answered.
    assert!(world
        .bus
        .replies
        .borrow()
        .iter()
        .all(|(id, _)| *id != txn.get()));
}

#[test]
fn example_12b_calls_the_server_by_name() {
    let ex = example("12b_pirate_echo_client");
    let world = gallery_world();
    add_pirate(&world.bus);
    let ran = Lab::over(world.bus.clone()).run(ex.source);
    judged(&ran, &ex);
    assert_eq!(ran.lines, ["AHOY! ARR. AHOY! ARR. "]);
}

#[test]
fn example_12b_fails_clearly_when_nothing_is_served() {
    let ex = example("12b_pirate_echo_client");
    let ran = Lab::over(gallery_world().bus).run(ex.source);
    assert!(
        ran.failure().contains("no service for os.lazy.echo.v1"),
        "{}",
        ran.failure()
    );
}

#[test]
fn example_13_shows_both_users_and_a_late_joiner_sees_the_last_line() {
    let (ran, world, ex) = run("13_chat_room", 3, |w| {
        w.bus.on_idle(|bus| {
            publish_bytes(bus, "demo/pg/chat/lobby", 99, b"bob: hello");
        });
    });
    judged(&ran, &ex);
    assert_eq!(ran.lines, ["alice joined", "bob: hello"]);
    // The join line was retained: a late subscriber reads it at once.
    let late = Lab::over(world.bus.clone()).run(
        "let s = msg::subscribe(\"demo/pg/chat/lobby\"); let e = s.next(10); print(e.payload.as_string()); print(e.retained);",
    );
    late.ok();
    assert_eq!(late.lines, ["alice joined", "true"]);
}
