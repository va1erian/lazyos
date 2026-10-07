//! The Network Drives app's model (pure, host tested): the connect form
//! checked with `mountd`'s own rules (`libs/mounttable`), so a request the
//! service would refuse is refused here with the reason, and the mount table
//! as list rows and status words.

use messenger_generated::os_lazy_mount_v1::MountInfo;
use mounttable::Request;

/// The mount name an empty Name field gets.
pub const DEFAULT_NAME: &str = "ftp";

/// What the connect form holds, as typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Form {
    pub host: String,
    pub port: String,
    pub user: String,
    pub password: String,
    pub name: String,
}

/// Check the form; the error is a sentence for the message line.
pub fn check(form: &Form) -> Result<Request, String> {
    let port = form.port.trim();
    let port = if port.is_empty() {
        0
    } else {
        port.parse::<u32>()
            .map_err(|_| String::from("The port must be a number from 1 to 65535."))?
    };
    let name = match form.name.trim() {
        "" => DEFAULT_NAME,
        name => name,
    };
    mounttable::validate(name, form.host.trim(), port, form.user.trim(), &form.password)
        .map_err(sentence)
}

/// `reason` with a capital and a full stop.
fn sentence(reason: &str) -> String {
    let mut chars = reason.chars();
    match chars.next() {
        Some(first) => format!("{}{}.", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// `ftp://user@host:port`, with the port only when it is not 21.
pub fn location(info: &MountInfo) -> String {
    let port = if info.port == u32::from(mounttable::DEFAULT_PORT) {
        String::new()
    } else {
        format!(":{}", info.port)
    };
    format!("{}://{}@{}{port}", info.kind, info.user, info.host)
}

/// The state in words.
pub fn state_text(info: &MountInfo) -> String {
    match info.state.as_str() {
        "connecting" => String::from("Connecting..."),
        "mounted" => String::from("Mounted"),
        "failed" if info.detail.is_empty() => String::from("Failed"),
        "failed" => format!("Failed: {}", info.detail),
        other => String::from(other),
    }
}

/// One list row: name, server, folder, state.
pub fn row(info: &MountInfo) -> Vec<String> {
    vec![
        info.name.clone(),
        location(info),
        info.path.clone(),
        state_text(info),
    ]
}

/// A refused request (a negative errno from `mountd`) in words.
pub fn refusal(code: i64) -> String {
    String::from(match -code {
        1 => "That mount belongs to another user.",
        2 => "There is no mount with that name.",
        11 => "Too many mounts: unmount one first.",
        13 => "This app may not use the mount service.",
        17 => "A mount with that name already exists.",
        22 => "The mount service refused the request.",
        110 => "The mount service did not answer in time.",
        _ => return format!("The mount service failed (error {}).", -code),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(host: &str, port: &str, user: &str, password: &str, name: &str) -> Form {
        Form {
            host: host.into(),
            port: port.into(),
            user: user.into(),
            password: password.into(),
            name: name.into(),
        }
    }

    fn info(state: &str, detail: &str, port: u32) -> MountInfo {
        MountInfo {
            name: "site".into(),
            kind: "ftp".into(),
            host: "10.0.2.2".into(),
            port,
            user: "lazy".into(),
            path: "/mnt/site".into(),
            state: state.into(),
            detail: detail.into(),
            owner: 0,
        }
    }

    #[test]
    fn empty_port_and_name_take_the_defaults() {
        let request = check(&form(" files.lan ", "", "", "", "")).unwrap();
        assert_eq!(request.host, "files.lan");
        assert_eq!(request.port, 21);
        assert_eq!(request.name, DEFAULT_NAME);
        assert_eq!(request.user, "");
    }

    #[test]
    fn the_service_rules_apply_here() {
        let request = check(&form("10.0.2.2", "2121", "lazy", "os", "site")).unwrap();
        assert_eq!((request.port, request.name.as_str()), (2121, "site"));
        assert_eq!(
            check(&form("h", "x", "", "", "")).unwrap_err(),
            "The port must be a number from 1 to 65535."
        );
        assert_eq!(
            check(&form("h", "", "", "", "My Files")).unwrap_err(),
            "The name must be 1 to 32 of a-z, 0-9, - and _."
        );
        assert_eq!(
            check(&form("h:21", "", "", "", "")).unwrap_err(),
            "Give the port in its own field."
        );
        assert!(check(&form("", "", "", "", "")).is_err());
        assert!(check(&form("h", "", "", "secret", "")).is_err());
    }

    #[test]
    fn rows_read_like_a_table() {
        assert_eq!(
            row(&info("mounted", "", 2121)),
            ["site", "ftp://lazy@10.0.2.2:2121", "/mnt/site", "Mounted"]
        );
        assert_eq!(location(&info("mounted", "", 21)), "ftp://lazy@10.0.2.2");
        assert_eq!(state_text(&info("connecting", "", 21)), "Connecting...");
        assert_eq!(
            state_text(&info("failed", "cannot find the host", 21)),
            "Failed: cannot find the host"
        );
    }

    #[test]
    fn refusals_are_sentences() {
        assert_eq!(refusal(-17), "A mount with that name already exists.");
        assert_eq!(refusal(-99), "The mount service failed (error 99).");
    }
}
