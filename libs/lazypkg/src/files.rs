//! `[permissions] files` rules: `read:<path>` or `write:<path>`.
//!
//! A path is absolute (`/a/b`) or relative to the user's home directory
//! (`$HOME/Documents/*`). `$HOME` may only be the first segment: it stands for
//! one directory, so `/x/$HOME` or `$HOME/$HOME` mean nothing. Segments are
//! `[A-Za-z0-9_.-]+` or `*`, with no `..`. `tools/pkg/pkgmanifest.py` checks
//! the same rules.

use fhs::{mount, state};

/// The home-directory variable a rule may start with.
pub const HOME_VAR: &str = "$HOME";

/// **F5 cleanup switch** (issue #509 section 2): an absolute path inside a
/// home directory (`/home/...`, or the legacy `/data/home/...`) is refused
/// with a pointer to `$HOME`, the only way a package names the running
/// user's home. On since every packager emits `$HOME` (`lazyrad-packager`
/// writes `$HOME/.apps/<system_name>`); `REJECT_ABSOLUTE_HOME` in
/// `tools/pkg/pkgmanifest.py` is the same switch.
pub(crate) const REJECT_ABSOLUTE_HOME: bool = true;

/// Why a files rule is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuleError {
    /// Not `read:`/`write:` followed by a well-formed path.
    Shape,
    /// `$HOME` somewhere other than the first segment.
    HomeNotFirst,
    /// An absolute path under a home directory ([`REJECT_ABSOLUTE_HOME`]).
    AbsoluteHome,
}

impl RuleError {
    /// The problem line for `rule`.
    pub(crate) fn message(self, rule: &str) -> alloc::string::String {
        match self {
            RuleError::Shape => alloc::format!(
                "permissions.files entry {rule:?} is not a read:/write: absolute or $HOME/ path"
            ),
            RuleError::HomeNotFirst => alloc::format!(
                "permissions.files entry {rule:?} may use $HOME only as its first segment"
            ),
            RuleError::AbsoluteHome => alloc::format!(
                "permissions.files entry {rule:?} names a home directory; write it as $HOME/..."
            ),
        }
    }
}

/// Check one files rule.
pub(crate) fn check_rule(rule: &str) -> Result<(), RuleError> {
    check_rule_with(rule, REJECT_ABSOLUTE_HOME)
}

fn check_rule_with(rule: &str, reject_absolute_home: bool) -> Result<(), RuleError> {
    let path = rule
        .strip_prefix("read:")
        .or_else(|| rule.strip_prefix("write:"))
        .ok_or(RuleError::Shape)?;
    let (relative, rest) = match path.strip_prefix(HOME_VAR) {
        Some(rest) => (true, rest.strip_prefix('/').ok_or(RuleError::Shape)?),
        None => (false, path.strip_prefix('/').ok_or(RuleError::Shape)?),
    };
    if rest.contains(HOME_VAR) {
        return Err(RuleError::HomeNotFirst);
    }
    if rest.is_empty() || !rest.split('/').all(valid_segment) {
        return Err(RuleError::Shape);
    }
    if reject_absolute_home && !relative && is_absolute_home(path) {
        return Err(RuleError::AbsoluteHome);
    }
    Ok(())
}

fn valid_segment(segment: &str) -> bool {
    if segment.is_empty() || segment == ".." {
        return false;
    }
    segment == "*"
        || segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Whether `path` is a home root or inside one: `/home`, or the legacy data
/// volume's `/data/home` that F4 retired.
pub(crate) fn is_absolute_home(path: &str) -> bool {
    let legacy = path
        .strip_prefix(mount::DATA)
        .filter(|rest| rest.starts_with('/'));
    [Some(path), legacy].into_iter().flatten().any(|path| {
        [state::HOME_ROOT, mount::HOME].iter().any(|root| {
            path.strip_prefix(root)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_is_allowed_only_as_the_first_segment() {
        for good in [
            "read:$HOME/Documents/*",
            "write:$HOME/.apps/org.lazy.demo/data",
            "write:$HOME/.apps/org.lazy.demo",
            "read:/system/share/*",
        ] {
            assert_eq!(check_rule(good), Ok(()), "{good}");
        }
        for (bad, error) in [
            ("read:$HOME", RuleError::Shape),
            ("read:$HOME/", RuleError::Shape),
            ("read:$HOMEX/a", RuleError::Shape),
            ("read:/x/$HOME/a", RuleError::HomeNotFirst),
            ("read:$HOME/$HOME/a", RuleError::HomeNotFirst),
            ("read:$HOME/a/../b", RuleError::Shape),
            ("read:home/a", RuleError::Shape),
            ("exec:$HOME/a", RuleError::Shape),
        ] {
            assert_eq!(check_rule(bad), Err(error), "{bad}");
        }
    }

    #[test]
    fn absolute_home_paths_are_recognised_for_the_cleanup_switch() {
        assert!(is_absolute_home("/home"));
        assert!(is_absolute_home("/home/*/x"));
        assert!(is_absolute_home("/home/ada"));
        assert!(!is_absolute_home("/homework"));
        assert!(!is_absolute_home("/homes"));
        assert!(is_absolute_home("/data/home/*/x"));
        assert!(is_absolute_home("/data/home"));
        assert!(!is_absolute_home("/data/homework"));
        assert!(!is_absolute_home("/database/home"));
        assert!(!is_absolute_home("/data/apps/x"));
    }

    #[test]
    fn the_cleanup_switch_refuses_absolute_home_paths_only() {
        let strict = |rule| check_rule_with(rule, true);
        assert_eq!(strict("read:/home/*/x"), Err(RuleError::AbsoluteHome));
        assert_eq!(strict("write:/home/ada"), Err(RuleError::AbsoluteHome));
        assert_eq!(strict("read:$HOME/x"), Ok(()));
        assert_eq!(strict("read:/homework"), Ok(()));
        assert!(RuleError::AbsoluteHome
            .message("read:/home/x")
            .contains("write it as $HOME/"));
        // On since the F5 cleanup (issue #509).
        for rule in [
            "read:/home/*/x",
            "write:/home/*/.apps/org.lazy.demo",
            "read:/home",
        ] {
            assert_eq!(check_rule(rule), Err(RuleError::AbsoluteHome), "{rule}");
        }
        assert_eq!(
            check_rule_with("read:/home/*/x", false),
            Ok(()),
            "the switch is the only difference"
        );
    }
}
