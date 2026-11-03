//! `fakebt`: a fake USB Bluetooth dongle for QEMU (docs/bluetooth-plan.md,
//! phases B2 and B3).
//!
//! QEMU has had no Bluetooth emulation of its own since `-bt` was removed, so
//! the guest sees a controller through `usb-redir`: this program listens on a
//! TCP port, QEMU connects (`-chardev socket,id=bt,host=127.0.0.1,port=N
//! -device usb-redir,chardev=bt`), and the dongle's USB side (`device.rs`,
//! `redir.rs`) forwards HCI packets to a [`controller::Controller`]: the
//! simulator of `libs/btsim` with a keyboard and a mouse peer that pair, bond,
//! reconnect and type on a script (`world.rs`, `script.rs`).
//!
//! Output is for the harness and the person reading its log (lower case, so
//! it is not mistaken for the guest's evidence, which is the serial log).
//!
//! ```text
//! fakebt [--listen 127.0.0.1:0] [--bd-addr 00:1A:7D:DA:71:13]
//!        [--connect-after-ms N] [--unplug-after-ms N]
//!        [--keyboard AA:BB:CC:DD:EE:FF] [--mouse AA:BB:CC:DD:EE:FF]
//!        [--pairable] [--forged-bond] [--mouse-waits]
//!        [--passkey correct|wrong|never] [--state FILE] [--at SPEC]...
//! ```
//!
//! `--pairable` puts a peer that has no stored bond in pairing mode;
//! `--state FILE` keeps the peers' link keys between two QEMU runs (a reboot
//! test); `--forged-bond` gives a peer without a bond a key the guest never
//! issued, so it pages the guest like a stranger would; `--mouse-waits` makes
//! the mouse wait to be paged (HIDNormallyConnectable) instead of paging the
//! host. `--at` is a step of `script.rs`.

mod controller;
mod device;
mod redir;
mod script;
mod world;

use std::io::Write;
use std::net::TcpListener;
use std::process::ExitCode;

use btsim::PasskeyBehavior;
use redir::{Options, Session};
use world::World;

struct Args {
    listen: String,
    options: Options,
    world: world::Options,
}

/// `AA:BB:CC:DD:EE:FF` as six octets in the printed order.
fn parse_address(text: &str) -> Result<[u8; 6], String> {
    let octets: Vec<u8> = text
        .split(':')
        .map(|part| u8::from_str_radix(part, 16).map_err(|e| format!("{text}: {e}")))
        .collect::<Result<_, _>>()?;
    octets
        .try_into()
        .map_err(|_| format!("{text}: want six octets"))
}

fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        listen: String::from("127.0.0.1:0"),
        options: Options {
            connect_after_ms: 0,
            unplug_after_ms: None,
        },
        world: world::Options {
            address: parse_address("00:1A:7D:DA:71:13")?,
            keyboard: None,
            mouse: None,
            pairable: false,
            forged_bond: false,
            mouse_waits_to_be_paged: false,
            passkey: PasskeyBehavior::Correct,
            state: None,
            steps: Vec::new(),
        },
    };
    let mut args = args;
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--listen" => parsed.listen = value()?,
            "--bd-addr" => parsed.world.address = parse_address(&value()?)?,
            "--connect-after-ms" => {
                parsed.options.connect_after_ms =
                    value()?.parse().map_err(|e| format!("{flag}: {e}"))?
            }
            "--unplug-after-ms" => {
                parsed.options.unplug_after_ms =
                    Some(value()?.parse().map_err(|e| format!("{flag}: {e}"))?)
            }
            "--keyboard" => parsed.world.keyboard = Some(parse_address(&value()?)?),
            "--mouse" => parsed.world.mouse = Some(parse_address(&value()?)?),
            "--pairable" => parsed.world.pairable = true,
            "--forged-bond" => parsed.world.forged_bond = true,
            "--mouse-waits" => parsed.world.mouse_waits_to_be_paged = true,
            "--passkey" => {
                parsed.world.passkey = match value()?.as_str() {
                    "correct" => PasskeyBehavior::Correct,
                    "wrong" => PasskeyBehavior::Wrong,
                    "never" => PasskeyBehavior::Never,
                    other => {
                        return Err(format!("--passkey {other}: want correct, wrong or never"))
                    }
                }
            }
            "--state" => parsed.world.state = Some(value()?.into()),
            "--at" => parsed.world.steps.extend(script::parse(&value()?)?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(parsed)
}

/// Serve one connection with the controller this build uses.
fn serve(listener: TcpListener, args: Args) -> std::io::Result<()> {
    let (stream, peer) = listener.accept()?;
    println!("fakebt: connected {peer}");
    let controller = World::new(args.world);
    let mut session = Session::new(stream, controller, args.options)?;
    let result = session.run();
    println!("fakebt: connection closed");
    result
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("fakebt: {message}");
            return ExitCode::from(2);
        }
    };
    let listener = match TcpListener::bind(&args.listen) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("fakebt: cannot listen on {}: {error}", args.listen);
            return ExitCode::from(1);
        }
    };
    match listener.local_addr() {
        Ok(local) => println!("fakebt: listening port={}", local.port()),
        Err(error) => {
            eprintln!("fakebt: {error}");
            return ExitCode::from(1);
        }
    }
    // The harness reads the port from a pipe: do not leave it buffered.
    let _ = std::io::stdout().flush();
    match serve(listener, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fakebt: {error}");
            ExitCode::from(1)
        }
    }
}
