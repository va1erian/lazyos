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

/// **F5 cleanup switch.** When `true`, an absolute path inside a home
/// directory (`/data/home/...`, `/home/...`) is refused with a pointer to
/// `$HOME`. It stays `false` until the packages that still spell their rules
/// that way have moved (issue #509 section 2); flip it together with
/// `REJECT_ABSOLUTE_HOME` in `tools/pkg/pkgmanifest.py`.
pub(crate) const REJECT_ABSOLUTE_HOME: bool = false;

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
    if REJECT_ABSOLUTE_HOME && !relative && is_absolute_home(path) {
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

/// Whether `path` is a home root or inside one.
pub(crate) fn is_absolute_home(path: &str) -> bool {
    [state::HOME_ROOT, mount::HOME].iter().any(|root| {
        path.strip_prefix(root)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
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
            "read:/data/home/*/pictures",
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
        assert!(is_absolute_home("/data/home"));
        assert!(is_absolute_home("/data/home/*/x"));
        assert!(is_absolute_home("/home/ada"));
        assert!(!is_absolute_home("/data/homework"));
        assert!(!is_absolute_home("/homes"));
        // Not rejected yet (issue #509, F5 cleanup).
        assert!(!REJECT_ABSOLUTE_HOME);
    }
}
