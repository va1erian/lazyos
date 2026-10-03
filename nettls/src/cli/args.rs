//! A small getopt: one option table per personality, GNU-style spelling.
//!
//! Accepted forms: `--long`, `--long=value`, `--long value`, `-x`,
//! `-xvalue`, `-x value`, and clusters of short flags where the last one may
//! take a value (`-sSL`, `-sSLo out`, `-sSLoout`). `--` ends the options; a
//! lone `-` is a positional argument.

use std::time::Duration;

/// One option a personality understands.
pub struct Spec<T> {
    pub short: Option<char>,
    pub long: Option<&'static str>,
    pub takes_value: bool,
    pub id: T,
}

/// The options found, in order, and the positional arguments.
#[derive(Debug)]
pub struct Parsed<T> {
    pub options: Vec<(T, Option<String>)>,
    pub positional: Vec<String>,
}

/// Split `args` (without `argv[0]`) by `specs`. An unknown option or a
/// missing value is an error naming the option as typed.
pub fn tokenize<T: Copy>(args: &[String], specs: &[Spec<T>]) -> Result<Parsed<T>, String> {
    let mut parsed = Parsed {
        options: Vec::new(),
        positional: Vec::new(),
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--" {
            parsed.positional.extend(rest.by_ref().cloned());
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            parsed.options.push(long_option(long, specs, &mut rest)?);
        } else if arg.len() > 1 && arg.starts_with('-') {
            short_cluster(&arg[1..], specs, &mut rest, &mut parsed.options)?;
        } else {
            parsed.positional.push(arg.clone());
        }
    }
    Ok(parsed)
}

fn long_option<'a, T: Copy>(
    text: &str,
    specs: &[Spec<T>],
    rest: &mut impl Iterator<Item = &'a String>,
) -> Result<(T, Option<String>), String> {
    let (name, inline) = match text.split_once('=') {
        Some((name, value)) => (name, Some(value.to_string())),
        None => (text, None),
    };
    let spec = specs
        .iter()
        .find(|s| s.long == Some(name))
        .ok_or_else(|| format!("option --{name}: is unknown"))?;
    if !spec.takes_value {
        if inline.is_some() {
            return Err(format!("option --{name}: takes no value"));
        }
        return Ok((spec.id, None));
    }
    let value = match inline {
        Some(value) => value,
        None => rest
            .next()
            .cloned()
            .ok_or_else(|| format!("option --{name}: requires a value"))?,
    };
    Ok((spec.id, Some(value)))
}

fn short_cluster<'a, T: Copy>(
    cluster: &str,
    specs: &[Spec<T>],
    rest: &mut impl Iterator<Item = &'a String>,
    out: &mut Vec<(T, Option<String>)>,
) -> Result<(), String> {
    for (at, flag) in cluster.char_indices() {
        let spec = specs
            .iter()
            .find(|s| s.short == Some(flag))
            .ok_or_else(|| format!("option -{flag}: is unknown"))?;
        if !spec.takes_value {
            out.push((spec.id, None));
            continue;
        }
        // A value-taking flag ends the cluster: the rest of it, or else the
        // next argument, is its value.
        let tail = &cluster[at + flag.len_utf8()..];
        let value = if tail.is_empty() {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("option -{flag}: requires a value"))?
        } else {
            tail.to_string()
        };
        out.push((spec.id, Some(value)));
        return Ok(());
    }
    Ok(())
}

/// The value of an option that [`tokenize`] guaranteed has one.
pub fn value(v: Option<String>) -> String {
    v.unwrap_or_default()
}

/// Seconds as curl and wget take them (`5`, `2.5`). Zero means no limit.
pub fn seconds(text: &str) -> Result<Option<Duration>, String> {
    let secs: f64 = text
        .trim()
        .parse()
        .map_err(|_| format!("'{text}' is not a number of seconds"))?;
    // A week is far beyond any useful limit and keeps the Duration finite.
    if !secs.is_finite() || !(0.0..=604_800.0).contains(&secs) {
        return Err(format!(
            "'{text}' is not a number of seconds between 0 and 604800"
        ));
    }
    Ok((secs > 0.0).then(|| Duration::from_secs_f64(secs)))
}

/// The most redirects any personality follows, whatever was asked: a loop
/// is reported as a failure instead of running forever.
pub const MAX_REDIRS_CEILING: u32 = 100;

/// A redirect limit. `-1` (curl's "unlimited") means [`MAX_REDIRS_CEILING`].
pub fn redirect_limit(text: &str) -> Result<u32, String> {
    let n: i64 = text
        .trim()
        .parse()
        .map_err(|_| format!("'{text}' is not a number"))?;
    match n {
        -1 => Ok(MAX_REDIRS_CEILING),
        0..=100 => Ok(n as u32),
        _ => Err(format!(
            "redirect limit {n} is outside 0..={MAX_REDIRS_CEILING} (or -1)"
        )),
    }
}

/// A request header `Name: value`. The name must be an HTTP token and the
/// value must not contain CR, LF or NUL, so an argument can never splice a
/// second header or request into the stream.
pub fn header(text: &str) -> Result<(String, String), String> {
    let (name, value) = text
        .split_once(':')
        .ok_or_else(|| format!("header '{text}' is not 'Name: value'"))?;
    let token = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
    if name.is_empty() || !name.chars().all(token) {
        return Err(format!("header name '{name}' is not a valid HTTP token"));
    }
    let value = value.trim_matches(|c| c == ' ' || c == '\t');
    if value.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
        return Err(format!(
            "header '{name}' has a line break or NUL in its value"
        ));
    }
    Ok((name.to_string(), value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Id {
        S,
        L,
        O,
        Max,
    }

    const SPECS: &[Spec<Id>] = &[
        Spec {
            short: Some('s'),
            long: Some("silent"),
            takes_value: false,
            id: Id::S,
        },
        Spec {
            short: Some('L'),
            long: Some("location"),
            takes_value: false,
            id: Id::L,
        },
        Spec {
            short: Some('o'),
            long: Some("output"),
            takes_value: true,
            id: Id::O,
        },
        Spec {
            short: None,
            long: Some("max-redirs"),
            takes_value: true,
            id: Id::Max,
        },
    ];

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn clusters_and_values() {
        let p = tokenize(&args(&["-sLo", "out", "u"]), SPECS).unwrap();
        assert_eq!(
            p.options,
            vec![(Id::S, None), (Id::L, None), (Id::O, Some("out".into()))]
        );
        assert_eq!(p.positional, vec!["u"]);
        let p = tokenize(&args(&["-sLoout"]), SPECS).unwrap();
        assert_eq!(p.options[2], (Id::O, Some("out".into())));
    }

    #[test]
    fn long_forms() {
        let p = tokenize(
            &args(&["--output=f", "--max-redirs", "3", "--silent"]),
            SPECS,
        )
        .unwrap();
        assert_eq!(
            p.options,
            vec![
                (Id::O, Some("f".into())),
                (Id::Max, Some("3".into())),
                (Id::S, None)
            ]
        );
    }

    #[test]
    fn errors_and_terminator() {
        assert!(tokenize(&args(&["-x"]), SPECS).unwrap_err().contains("-x"));
        assert!(tokenize(&args(&["--nope"]), SPECS).is_err());
        assert!(tokenize(&args(&["-o"]), SPECS)
            .unwrap_err()
            .contains("requires"));
        assert!(tokenize(&args(&["--silent=1"]), SPECS).is_err());
        let p = tokenize(&args(&["--", "-s", "-"]), SPECS).unwrap();
        assert!(p.options.is_empty());
        assert_eq!(p.positional, vec!["-s", "-"]);
    }

    #[test]
    fn numbers() {
        assert_eq!(seconds("2.5").unwrap(), Some(Duration::from_millis(2500)));
        assert_eq!(seconds("0").unwrap(), None);
        assert!(seconds("-1").is_err());
        assert!(seconds("nan").is_err());
        assert!(seconds("inf").is_err());
        assert_eq!(redirect_limit("-1").unwrap(), MAX_REDIRS_CEILING);
        assert_eq!(redirect_limit("5").unwrap(), 5);
        assert!(redirect_limit("1000").is_err());
        assert!(redirect_limit("x").is_err());
    }

    #[test]
    fn headers_are_validated() {
        assert_eq!(header("X-A:  b c ").unwrap(), ("X-A".into(), "b c".into()));
        assert_eq!(header("X-Empty:").unwrap(), ("X-Empty".into(), "".into()));
        assert!(header("no colon").is_err());
        assert!(header("Bad Name: v").is_err());
        assert!(header("X: a\r\nHost: evil").is_err());
        assert!(header("X: a\nb").is_err());
        assert!(header(": v").is_err());
    }
}
