//! `fakebt`: a fake USB Bluetooth dongle for QEMU (docs/bluetooth-plan.md,
//! phase B2).
//!
//! QEMU has had no Bluetooth emulation of its own since `-bt` was removed, so
//! the guest sees a controller through `usb-redir`: this program listens on a
//! TCP port, QEMU connects (`-chardev socket,id=bt,host=127.0.0.1,port=N
//! -device usb-redir,chardev=bt`), and the dongle's USB side (`device.rs`,
//! `redir.rs`) forwards HCI packets to a [`controller::Controller`]. Today
//! that is the stub in `stub.rs` (Reset, Read_BD_ADDR, Read_Local_Version);
//! `libs/btsim` implements the same trait, and serving it instead is the one
//! line in [`serve`] that names the stub.
//!
//! Output is for the harness and the person reading its log (lower case, so
//! it is not mistaken for the guest's evidence, which is the serial log).
//!
//! ```text
//! fakebt [--listen 127.0.0.1:0] [--bd-addr 00:1A:7D:DA:71:13]
//!        [--connect-after-ms N] [--unplug-after-ms N]
//! ```

mod controller;
mod device;
mod redir;
mod stub;

use std::io::Write;
use std::net::TcpListener;
use std::process::ExitCode;

use redir::{Options, Session};

struct Args {
    listen: String,
    address: [u8; 6],
    options: Options,
}

/// `AA:BB:CC:DD:EE:FF` as six octets, least significant first (the wire
/// order of Read_BD_ADDR).
fn parse_address(text: &str) -> Result<[u8; 6], String> {
    let octets: Vec<u8> = text
        .split(':')
        .map(|part| u8::from_str_radix(part, 16).map_err(|e| format!("{text}: {e}")))
        .collect::<Result<_, _>>()?;
    let mut address: [u8; 6] = octets
        .try_into()
        .map_err(|_| format!("{text}: want six octets"))?;
    address.reverse();
    Ok(address)
}

fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        listen: String::from("127.0.0.1:0"),
        address: parse_address("00:1A:7D:DA:71:13")?,
        options: Options {
            connect_after_ms: 0,
            unplug_after_ms: None,
        },
    };
    let mut args = args;
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--listen" => parsed.listen = value()?,
            "--bd-addr" => parsed.address = parse_address(&value()?)?,
            "--connect-after-ms" => {
                parsed.options.connect_after_ms =
                    value()?.parse().map_err(|e| format!("{flag}: {e}"))?
            }
            "--unplug-after-ms" => {
                parsed.options.unplug_after_ms =
                    Some(value()?.parse().map_err(|e| format!("{flag}: {e}"))?)
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(parsed)
}

/// Serve one connection with the controller this build uses.
fn serve(listener: TcpListener, args: Args) -> std::io::Result<()> {
    let (stream, peer) = listener.accept()?;
    println!("fakebt: connected {peer}");
    // The one line that names the stub: swap it for `btsim`'s controller.
    let controller = stub::Stub::new(args.address);
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
