//! The words `gpud` is started with, parsed once.
//!
//! `init` joins the driver row's manifest words (`audio=1 edid=<path>`, from
//! `LAZYOS_GPUD_ARGS`) and `devd`'s `dev=<id>`; `gpud` reads them with
//! `sys::service_args`, which reports the full length even when its buffer was
//! too short. The caller checks that length against its buffer
//! ([`Truncated`]) before it calls [`parse`], so a cut word is an error and
//! never a missing option. Here a bad or repeated option is an error too:
//! `gpud` runs as the only writer of the display's audio block, and a typo
//! must not quietly turn into "off", or into "the first GPU in the list".

use alloc::string::String;

/// Bytes `gpud` reads its arguments into: `dev=<id>`, `audio=1` and a path.
pub const BUFFER_BYTES: usize = 512;

/// What `gpud` was started with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// `dev=<id>`: the device `devd` matched this driver to.
    pub dev: Option<u64>,
    /// `audio=1`: enable the display side of HDMI audio (stage G2).
    pub audio: bool,
    /// `edid=<path>`: the sink's EDID, until a DDC reader exists.
    pub edid: Option<String>,
}

/// Why the arguments were refused; the word is attached for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgsError {
    /// `service_args` reported `needed` bytes for a buffer of `have`.
    Truncated { needed: usize, have: usize },
    /// Not valid UTF-8.
    NotText,
    /// `dev=` is not a decimal number.
    BadDev(String),
    /// `audio=` is neither `0` nor `1`.
    BadAudio(String),
    /// `edid=` names nothing.
    EmptyEdid,
    /// An option appears twice.
    Repeated(&'static str),
}

/// Check the length `service_args` returned against the buffer it filled and
/// return the text it holds.
pub fn text(buffer: &[u8], needed: usize) -> Result<&str, ArgsError> {
    if needed > buffer.len() {
        return Err(ArgsError::Truncated {
            needed,
            have: buffer.len(),
        });
    }
    core::str::from_utf8(&buffer[..needed]).map_err(|_| ArgsError::NotText)
}

fn once<T>(slot: &mut Option<T>, name: &'static str, value: T) -> Result<(), ArgsError> {
    match slot.replace(value) {
        None => Ok(()),
        Some(_) => Err(ArgsError::Repeated(name)),
    }
}

/// Parse the whitespace-separated words. A word that is not one of the
/// options is ignored (`init` may add others); an option with a bad value is
/// an error.
pub fn parse(text: &str) -> Result<Args, ArgsError> {
    let (mut dev, mut audio, mut edid) = (None, None, None);
    for word in text.split_whitespace() {
        if let Some(id) = word.strip_prefix("dev=") {
            let id = id
                .parse()
                .map_err(|_| ArgsError::BadDev(String::from(id)))?;
            once(&mut dev, "dev", id)?;
        } else if let Some(flag) = word.strip_prefix("audio=") {
            let on = match flag {
                "1" => true,
                "0" => false,
                other => return Err(ArgsError::BadAudio(String::from(other))),
            };
            once(&mut audio, "audio", on)?;
        } else if let Some(path) = word.strip_prefix("edid=") {
            if path.is_empty() {
                return Err(ArgsError::EmptyEdid);
            }
            once(&mut edid, "edid", String::from(path))?;
        }
    }
    Ok(Args {
        dev,
        audio: audio.unwrap_or(false),
        edid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn init_s_words_parse_in_any_order() {
        let a = parse("audio=1 edid=/system/share/gpu/edid.bin dev=7").unwrap();
        assert_eq!(
            a,
            Args {
                dev: Some(7),
                audio: true,
                edid: Some("/system/share/gpu/edid.bin".into())
            }
        );
        assert_eq!(parse("dev=7 edid=/e audio=1").unwrap(), {
            let mut b = a.clone();
            b.edid = Some("/e".into());
            b
        });
    }

    #[test]
    fn nothing_given_means_off_and_the_first_gpu() {
        assert_eq!(parse("").unwrap(), Args::default());
        assert_eq!(parse("  dev=3 ").unwrap().dev, Some(3));
        assert!(!parse("audio=0").unwrap().audio);
        assert_eq!(parse("ifname=eth0 other").unwrap(), Args::default());
    }

    #[test]
    fn a_bad_value_is_an_error_not_a_default() {
        assert_eq!(parse("dev=x"), Err(ArgsError::BadDev("x".into())));
        assert_eq!(parse("dev="), Err(ArgsError::BadDev("".into())));
        assert_eq!(parse("dev=-1"), Err(ArgsError::BadDev("-1".into())));
        assert_eq!(parse("audio=yes"), Err(ArgsError::BadAudio("yes".into())));
        assert_eq!(parse("audio="), Err(ArgsError::BadAudio("".into())));
        assert_eq!(parse("edid="), Err(ArgsError::EmptyEdid));
        assert_eq!(parse("dev=1 dev=2"), Err(ArgsError::Repeated("dev")));
        assert_eq!(parse("audio=1 audio=0"), Err(ArgsError::Repeated("audio")));
        assert_eq!(parse("edid=/a edid=/b"), Err(ArgsError::Repeated("edid")));
    }

    #[test]
    fn a_truncated_read_is_an_error_even_when_the_cut_text_parses() {
        // `audio=1 edid=/a/long/pa` cut from `... dev=7`: every word that
        // survived is well formed, and `dev=` is simply gone.
        let full = "audio=1 edid=/a/long/path dev=7";
        let buffer = &full.as_bytes()[..20];
        assert_eq!(
            text(buffer, full.len()),
            Err(ArgsError::Truncated {
                needed: full.len(),
                have: 20
            })
        );
        assert!(parse(core::str::from_utf8(buffer).unwrap()).is_ok());
        assert_eq!(text(full.as_bytes(), full.len()), Ok(full));
        // A buffer that is exactly full is fine.
        let exact = vec![b'a'; 4];
        assert!(text(&exact, 4).is_ok());
        assert!(text(&exact, 5).is_err());
    }

    #[test]
    fn invalid_utf8_is_refused() {
        assert_eq!(text(&[0xFF, 0xFE], 2), Err(ArgsError::NotText));
    }

    #[test]
    fn the_buffer_holds_the_largest_sane_line() {
        let line = alloc::format!("audio=1 edid=/{} dev=18446744073709551615", "d".repeat(400));
        assert!(line.len() <= BUFFER_BYTES);
        assert!(parse(&line).is_ok());
    }
}
