//! Pure presentation helpers: risk grouping and the text hygiene every
//! untrusted package field goes through before it reaches a widget.
//!
//! The screen is a fixed-width label/list UI, so "display width" is a character
//! budget rather than a pixel measurement; [`elide`] counts Unicode scalar
//! values (never bytes), so a multi-byte name can neither be split mid-glyph
//! nor make the result panic.

use super::model::Permission;

/// A permission's risk, ordered most severe first for the consent screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    /// The package can read or change the user's data or the system.
    High,
    /// A meaningful but bounded capability.
    Medium,
    /// A capability with little privacy or security impact.
    Low,
    /// A risk `pkgd` did not name (or one this client does not know).
    Unknown,
}

impl Risk {
    /// The display order: high, medium, low, then anything unrecognised.
    pub const ORDER: [Risk; 4] = [Risk::High, Risk::Medium, Risk::Low, Risk::Unknown];

    /// Decodes a `pkgd` risk label; case and surrounding space are ignored.
    /// Anything unrecognised is [`Risk::Unknown`], never a panic.
    pub fn from_label(label: &str) -> Risk {
        match label.trim().to_ascii_lowercase().as_str() {
            "high" => Risk::High,
            "medium" => Risk::Medium,
            "low" => Risk::Low,
            _ => Risk::Unknown,
        }
    }

    /// The heading shown above the group.
    pub fn title(self) -> &'static str {
        match self {
            Risk::High => "High risk",
            Risk::Medium => "Medium risk",
            Risk::Low => "Low risk",
            Risk::Unknown => "Other",
        }
    }
}

/// One risk's permissions, in the order `pkgd` supplied them.
pub struct RiskGroup<'a> {
    /// The group's risk.
    pub risk: Risk,
    /// The permissions of that risk, in stable input order.
    pub permissions: Vec<&'a Permission>,
}

/// Groups `permissions` by risk, high first, keeping the input order inside a
/// group and dropping empty groups. A stable order matters: the consent screen
/// must not reshuffle between two paints of the same package.
pub fn group_by_risk(permissions: &[Permission]) -> Vec<RiskGroup<'_>> {
    let mut groups = Vec::new();
    for risk in Risk::ORDER {
        let members: Vec<&Permission> = permissions
            .iter()
            .filter(|permission| Risk::from_label(&permission.risk) == risk)
            .collect();
        if !members.is_empty() {
            groups.push(RiskGroup {
                risk,
                permissions: members,
            });
        }
    }
    groups
}

/// Strips every control character (including newlines and the ESC used to
/// forge terminal markers) from untrusted text.
pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect()
}

/// Bounds `text` to at most `max_chars` Unicode scalar values, replacing the
/// tail with a single `…`. Control characters are stripped first. An empty
/// string, `max_chars == 0`, and non-ASCII text are all handled without a
/// panic; a multi-byte character is never split.
pub fn elide(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let cleaned = clean(text);
    if cleaned.chars().count() <= max_chars {
        return cleaned;
    }
    let mut out: String = cleaned.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

/// The short form of an archive digest shown on the consent screen: the first
/// eight characters, or `unknown` when `pkgd` sent nothing.
pub fn short_digest(digest: &str) -> String {
    let cleaned = clean(digest);
    let head: String = cleaned.chars().take(8).collect();
    if head.is_empty() {
        "unknown".to_owned()
    } else {
        head
    }
}

/// One consent row: the friendly explanation, with the concrete value in
/// parentheses when both are present. Falls back to the kind when `pkgd` sent
/// no explanation, so a row is never blank.
pub fn permission_line(permission: &Permission) -> String {
    let explanation = clean(&permission.explanation);
    let value = clean(&permission.value);
    let kind = clean(&permission.kind);
    if explanation.is_empty() {
        if value.is_empty() {
            kind
        } else {
            format!("{kind}: {value}")
        }
    } else if value.is_empty() {
        explanation
    } else {
        format!("{explanation} ({value})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission(risk: &str, value: &str) -> Permission {
        Permission {
            kind: "interface".into(),
            value: value.into(),
            risk: risk.into(),
            explanation: format!("uses {value}"),
        }
    }

    #[test]
    fn risk_decoding_is_case_insensitive_and_total() {
        assert_eq!(Risk::from_label("HIGH"), Risk::High);
        assert_eq!(Risk::from_label(" Medium "), Risk::Medium);
        assert_eq!(Risk::from_label("low"), Risk::Low);
        assert_eq!(Risk::from_label("critical"), Risk::Unknown);
        assert_eq!(Risk::from_label(""), Risk::Unknown);
        assert_eq!(Risk::from_label("\u{1F512}"), Risk::Unknown);
    }

    #[test]
    fn groups_are_high_then_medium_then_low_then_unknown() {
        let permissions = vec![
            permission("low", "a"),
            permission("high", "b"),
            permission("medium", "c"),
            permission("banana", "d"),
            permission("high", "e"),
        ];
        let groups = group_by_risk(&permissions);
        let order: Vec<Risk> = groups.iter().map(|group| group.risk).collect();
        assert_eq!(
            order,
            vec![Risk::High, Risk::Medium, Risk::Low, Risk::Unknown]
        );
        // Stable inside a group: the two high permissions keep input order.
        assert_eq!(
            groups[0]
                .permissions
                .iter()
                .map(|p| p.value.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "e"]
        );
    }

    #[test]
    fn empty_groups_are_dropped() {
        let permissions = [permission("low", "a")];
        let groups = group_by_risk(&permissions);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].risk, Risk::Low);
        assert!(group_by_risk(&[]).is_empty());
    }

    #[test]
    fn elision_counts_characters_not_bytes() {
        assert_eq!(elide("hello", 10), "hello");
        assert_eq!(elide("hello world", 5), "hell…");
        // Four Cyrillic characters (two bytes each) stay intact.
        assert_eq!(elide("привет", 3), "пр…");
        assert_eq!(elide("", 5), "");
        assert_eq!(elide("anything", 0), "");
        assert_eq!(elide("abc", 1), "…");
    }

    #[test]
    fn elision_strips_control_characters_first() {
        assert_eq!(elide("a\nb\tc", 10), "abc");
        assert_eq!(clean("x\u{1b}[2Jy"), "x[2Jy");
        assert_eq!(clean(""), "");
    }

    #[test]
    fn short_digest_bounds_and_falls_back() {
        assert_eq!(short_digest("0123456789abcdef"), "01234567");
        assert_eq!(short_digest("abc"), "abc");
        assert_eq!(short_digest(""), "unknown");
        assert_eq!(short_digest("\u{7}"), "unknown");
    }

    #[test]
    fn permission_line_never_returns_blank() {
        let full = permission("high", "os.lazy.fs.v1");
        assert_eq!(permission_line(&full), "uses os.lazy.fs.v1 (os.lazy.fs.v1)");
        let no_explanation = Permission {
            kind: "network".into(),
            value: "outbound".into(),
            ..Permission::default()
        };
        assert_eq!(permission_line(&no_explanation), "network: outbound");
        let nothing = Permission::default();
        assert_eq!(permission_line(&nothing), "");
    }
}
