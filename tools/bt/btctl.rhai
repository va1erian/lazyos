#!/system/bin/rhai
// btctl: Bluetooth devices from the shell (docs/bluetooth-plan.md B5).
//
//   btctl list                  the devices btd knows: bonded ones and what a scan found
//   btctl scan [seconds]        scan (default 10 s, at most 60) and print what is found
//   btctl pair <address>        pair with a device a scan found; prints the passkey to type
//                               on a keyboard, or the number to compare (see `confirm`)
//   btctl confirm <address> yes|no   answer a number to compare
//   btctl cancel                give up the pairing in progress
//   btctl forget <address>      remove the bond and drop the device
//   btctl status                the pairing in progress, or the outcome of the last one
//
// The calls are `os.lazy.bt.v1`: allowed to the user of the login session,
// refused to anyone else, and no call ever returns a key. Output lines are
// `<what>: <value>`; the passkey and the number are printed as they are shown,
// which is what a person at the machine needs and nothing else.

let POLL_MS = 500;
// A pairing window is 60 s; leave a little more for the last answer.
let PAIR_LIMIT_MS = 70000;

fn usage() {
    print("usage: btctl list | scan [seconds] | pair <address> | confirm <address> yes|no | cancel | forget <address> | status");
    exit(2);
}

fn kind(device) {
    let t = if device.transport == "Le" { "le" } else { "bredr" };
    let i = switch device.input {
        "Keyboard" => "keyboard",
        "Pointer" => "pointer",
        _ => "device",
    };
    t + " " + i
}

fn show(device) {
    let state = if device.connected { "connected" } else if device.bonded { "paired" } else { "found" };
    let signal = if device.rssi == () { "" } else { " " + device.rssi + "dBm" };
    let name = if device.name == "" { "(unnamed)" } else { device.name };
    print(device.address + "  " + state + "  " + kind(device) + signal + "  " + name);
}

fn list() {
    let reply = sys::bt::list();
    if !reply.controller {
        print("controller: none");
    }
    for device in reply.devices {
        show(device);
    }
}

fn scan(seconds) {
    sys::bt::start_discovery(seconds);
    let seen = #{};
    let waited = 0;
    while waited < seconds * 1000 {
        sleep(POLL_MS);
        waited += POLL_MS;
        for device in sys::bt::list().devices {
            if !device.bonded && !(device.address in seen) {
                seen[device.address] = true;
                show(device);
            }
        }
    }
    sys::bt::stop_discovery();
    print("scan: done");
}

// Print what the pairing shows, once per change, until it ends.
fn pair(address) {
    sys::bt::pair(address);
    let last = "";
    let waited = 0;
    while waited < PAIR_LIMIT_MS {
        let p = sys::bt::status().pairing;
        let line = switch p.phase {
            "ShowPasskey" => "passkey: " + format_pad(p.passkey),
            "ConfirmNumber" => "number: " + format_pad(p.number),
            "Paired" => "paired: " + p.address,
            "Failed" => "failed: " + p.reason,
            _ => "",
        };
        if line != "" && line != last {
            print(line);
            last = line;
        }
        if p.phase == "Paired" {
            return;
        }
        if p.phase == "Failed" {
            exit(1);
        }
        sleep(POLL_MS);
        waited += POLL_MS;
    }
    sys::bt::cancel_pairing();
    print("failed: Timeout");
    exit(1);
}

// Six digits, as a keyboard asks for them.
fn format_pad(n) {
    let s = n.to_string();
    while s.len < 6 {
        s = "0" + s;
    }
    s
}

let args = os::args();
if args.len == 0 {
    usage();
}
try {
    switch args[0] {
        "list" => list(),
        "scan" => scan(if args.len > 1 { parse_int(args[1]) } else { 10 }),
        "pair" => { if args.len < 2 { usage(); } pair(args[1]) }
        "confirm" => {
            if args.len < 3 || (args[2] != "yes" && args[2] != "no") { usage(); }
            sys::bt::confirm_number(args[1], args[2] == "yes");
        }
        "cancel" => sys::bt::cancel_pairing(),
        "forget" => { if args.len < 2 { usage(); } sys::bt::forget(args[1]); print("forgotten: " + args[1]); }
        "status" => {
            let p = sys::bt::status().pairing;
            print("phase: " + p.phase + (if p.address != "" { " " + p.address } else { "" }));
        }
        _ => usage(),
    }
} catch (e) {
    print("btctl: " + e);
    exit(1);
}
