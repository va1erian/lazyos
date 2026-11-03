//! What the fake peers do and when: the `--at` steps of the harness.
//!
//! ```text
//! --at [armedN+|hidN+|mhidN+]MS:ACTION
//!   type=TEXT         the keyboard types TEXT (letters, digits, space, enter),
//!                     one character every 40 ms
//!   key=0x04          the keyboard presses and releases one usage
//!   hold=0x04         the keyboard presses a usage and keeps holding it
//!   release           the keyboard releases everything
//!   wake              both peers are touched (a bonded one that pages the
//!                     host starts paging; nothing is typed)
//!   move=DX,DY,BTNS   the mouse moves and holds the buttons (a bit mask)
//!   dropkbd=0x08      the keyboard's link is lost (HCI reason)
//!   dropmouse=0x08    the mouse's link is lost
//! ```
//!
//! `MS` counts from the connection to QEMU; with `armedN+`, from the Nth time
//! the guest turned page scan on (the host is armed: 1 is the first boot of
//! `btd`, 2 the restart after it was killed); with `hidN+` (`mhidN+` for the
//! mouse), from the Nth time the keyboard's (mouse's) HID channels were both
//! open, which is when typing cannot be lost to a slow guest. A step is a
//! one-shot.

/// What a step is measured from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base {
    Connect,
    /// The Nth time page scan went on (1-based).
    Armed(usize),
    /// The Nth time the keyboard's HID channels were both open.
    Keyboard(usize),
    /// The same for the mouse.
    Mouse(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Key(u8),
    Hold(u8),
    Release,
    Wake,
    Move { dx: i8, dy: i8, buttons: u8 },
    DropKeyboard(u8),
    DropMouse(u8),
}

#[derive(Clone, Debug)]
pub struct Step {
    pub base: Base,
    pub at_ms: u64,
    pub action: Action,
    pub fired: bool,
}

/// Milliseconds between the characters of `type=`.
pub const CHAR_GAP_MS: u64 = 40;

/// The HID usage a typed character presses (the keys the harness uses).
pub fn usage(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(0x04 + (c as u8 - b'a')),
        '1'..='9' => Some(0x1E + (c as u8 - b'1')),
        '0' => Some(0x27),
        '\n' => Some(0x28),
        ' ' => Some(0x2C),
        _ => None,
    }
}

fn number(text: &str) -> Result<u8, String> {
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u8::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.map_err(|e| format!("{text}: {e}"))
}

/// Parse one `--at` argument into its steps.
pub fn parse(spec: &str) -> Result<Vec<Step>, String> {
    let (when, action) = spec
        .split_once(':')
        .ok_or_else(|| format!("{spec}: want [armedN+]MS:ACTION"))?;
    type Make = fn(usize) -> Base;
    let prefixes: [(&str, Make); 3] = [
        ("armed", Base::Armed),
        ("mhid", Base::Mouse),
        ("hid", Base::Keyboard),
    ];
    let mut based = None;
    for (prefix, make) in prefixes {
        if let Some(rest) = when.strip_prefix(prefix) {
            let (n, ms) = rest
                .split_once('+')
                .ok_or_else(|| format!("{when}: want {prefix}N+MS"))?;
            let n: usize = n.parse().map_err(|e| format!("{when}: {e}"))?;
            based = Some((make(n), ms));
            break;
        }
    }
    let (base, ms) = based.unwrap_or((Base::Connect, when));
    let at_ms: u64 = ms.parse().map_err(|e| format!("{when}: {e}"))?;
    let (name, value) = action.split_once('=').unwrap_or((action, ""));
    let step = |at_ms, action| Step {
        base,
        at_ms,
        action,
        fired: false,
    };
    Ok(match name {
        "type" => value
            .chars()
            .enumerate()
            .map(|(i, c)| {
                usage(c)
                    .map(|u| step(at_ms + i as u64 * CHAR_GAP_MS, Action::Key(u)))
                    .ok_or_else(|| format!("cannot type {c:?}"))
            })
            .collect::<Result<_, _>>()?,
        "key" => vec![step(at_ms, Action::Key(number(value)?))],
        "hold" => vec![step(at_ms, Action::Hold(number(value)?))],
        "release" => vec![step(at_ms, Action::Release)],
        "wake" => vec![step(at_ms, Action::Wake)],
        "dropkbd" => vec![step(at_ms, Action::DropKeyboard(number(value)?))],
        "dropmouse" => vec![step(at_ms, Action::DropMouse(number(value)?))],
        "move" => {
            let parts: Vec<&str> = value.split(',').collect();
            let [dx, dy, buttons] = parts.as_slice() else {
                return Err(format!("{value}: want DX,DY,BUTTONS"));
            };
            let signed = |t: &str| t.parse::<i8>().map_err(|e| format!("{t}: {e}"));
            vec![step(
                at_ms,
                Action::Move {
                    dx: signed(dx)?,
                    dy: signed(dy)?,
                    buttons: number(buttons)?,
                },
            )]
        }
        other => return Err(format!("unknown action {other}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_becomes_one_key_per_gap() {
        let steps = parse("armed1+500:type=ab 1").unwrap();
        let usages: Vec<(u64, Action)> =
            steps.iter().map(|s| (s.at_ms, s.action.clone())).collect();
        assert_eq!(
            usages,
            [
                (500, Action::Key(4)),
                (540, Action::Key(5)),
                (580, Action::Key(0x2C)),
                (620, Action::Key(0x1E)),
            ]
        );
        assert!(steps.iter().all(|s| s.base == Base::Armed(1)));
    }

    #[test]
    fn the_bases_parse() {
        assert_eq!(
            parse("hid1+300:release").unwrap()[0].base,
            Base::Keyboard(1)
        );
        assert_eq!(parse("mhid2+0:move=1,1,0").unwrap()[0].base, Base::Mouse(2));
        assert_eq!(parse("5:release").unwrap()[0].base, Base::Connect);
        assert!(parse("hid+1:release").is_err());
    }

    #[test]
    fn the_other_actions_parse() {
        assert_eq!(parse("100:hold=0x04").unwrap()[0].action, Action::Hold(4));
        assert_eq!(parse("100:release").unwrap()[0].action, Action::Release);
        assert_eq!(
            parse("7:move=5,-3,1").unwrap()[0].action,
            Action::Move {
                dx: 5,
                dy: -3,
                buttons: 1
            }
        );
        assert_eq!(
            parse("9:dropkbd=8").unwrap()[0].action,
            Action::DropKeyboard(8)
        );
    }

    #[test]
    fn bad_specs_are_errors() {
        for bad in [
            "",
            "100",
            "x:key=4",
            "100:key",
            "100:type=A",
            "100:move=1,2",
            "armed:100:key=1",
            "100:fly",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
