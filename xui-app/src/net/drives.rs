//! The Network Drives app's model (pure, host tested): the connect form
//! checked with `mountd`'s own rules (`libs/mounttable`), so a request the
//! service would refuse is refused here with the reason, and the mount table
//! as list rows and status words.

use messenger_generated::os_lazy_mount_v1::MountInfo;
use mounttable::{Kind, Request};

/// The mount name an empty Name field gets for an FTP server.
pub const DEFAULT_NAME: &str = "ftp";
/// The mount name an empty Name field gets for an SMB share whose own name
/// cannot be one.
pub const DEFAULT_SMB_NAME: &str = "smb";

/// What the connect form holds, as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    pub kind: Kind,
    pub host: String,
    pub port: String,
    /// The SMB share (ignored for FTP).
    pub share: String,
    pub user: String,
    pub password: String,
    pub name: String,
}

impl Default for Form {
    fn default() -> Form {
        Form {
            kind: Kind::Ftp,
            host: String::new(),
            port: String::new(),
            share: String::new(),
            user: String::new(),
            password: String::new(),
            name: String::new(),
        }
    }
}

/// The name an empty Name field stands for: `ftp`, or the share's own name
/// in lower case when that is a valid mount name.
pub fn default_name(form: &Form) -> String {
    if form.kind == Kind::Ftp {
        return String::from(DEFAULT_NAME);
    }
    let share = form.share.trim().to_ascii_lowercase();
    let ok = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_';
    if !share.is_empty() && share.len() <= mounttable::NAME_MAX && share.chars().all(ok) {
        share
    } else {
        String::from(DEFAULT_SMB_NAME)
    }
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
        "" => default_name(form),
        name => String::from(name),
    };
    let (host, user) = (form.host.trim(), form.user.trim());
    match form.kind {
        Kind::Ftp => mounttable::validate(&name, host, port, user, &form.password),
        Kind::Smb => {
            let share = form.share.trim();
            mounttable::validate_smb(&name, host, port, share, user, &form.password)
        }
    }
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

/// `ftp://user@host:port` or `smb://user@host:port/share`, with the port
/// only when it is not the protocol's own.
pub fn location(info: &MountInfo) -> String {
    let usual = match info.kind.as_str() {
        mounttable::KIND_SMB => mounttable::SMB_PORT,
        _ => mounttable::DEFAULT_PORT,
    };
    let port = if info.port == u32::from(usual) {
        String::new()
    } else {
        format!(":{}", info.port)
    };
    let share = if info.share.is_empty() {
        String::new()
    } else {
        format!("/{}", info.share)
    };
    format!("{}://{}@{}{port}{share}", info.kind, info.user, info.host)
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
            ..Form::default()
        }
    }

    fn smb_form(share: &str, user: &str, name: &str) -> Form {
        Form {
            kind: Kind::Smb,
            host: "10.0.2.2".into(),
            share: share.into(),
            user: user.into(),
            password: "pw".into(),
            name: name.into(),
            ..Form::default()
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
            share: String::new(),
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
    fn an_smb_form_names_a_share_and_a_user() {
        let request = check(&smb_form("Public", "chaton", "")).unwrap();
        assert_eq!(request.kind, Kind::Smb);
        assert_eq!(request.port, mounttable::SMB_PORT);
        assert_eq!(
            (request.share.as_str(), request.name.as_str()),
            ("Public", "public")
        );
        assert_eq!(
            check(&smb_form("My Files", "u", "")).unwrap().name,
            DEFAULT_SMB_NAME
        );
        assert_eq!(check(&smb_form("s", "u", "nas")).unwrap().name, "nas");
        assert_eq!(
            check(&smb_form("s", "", "")).unwrap_err(),
            "An SMB share needs a user name."
        );
        assert!(check(&smb_form("", "u", "")).is_err());
        assert!(check(&smb_form("a/b", "u", "")).is_err());
        // An FTP form ignores the share field.
        let mut ftp = smb_form("ignored", "", "");
        ftp.kind = Kind::Ftp;
        ftp.password.clear();
        assert_eq!(check(&ftp).unwrap().share, "");
    }

    #[test]
    fn an_smb_row_shows_the_share() {
        let mut smb = info("mounted", "", 445);
        smb.kind = "smb".into();
        smb.share = "share".into();
        assert_eq!(location(&smb), "smb://lazy@10.0.2.2/share");
        smb.port = 1445;
        assert_eq!(location(&smb), "smb://lazy@10.0.2.2:1445/share");
    }

    #[test]
    fn refusals_are_sentences() {
        assert_eq!(refusal(-17), "A mount with that name already exists.");
        assert_eq!(refusal(-99), "The mount service failed (error 99).");
    }
}
