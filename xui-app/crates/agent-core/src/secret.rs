//! A key or token. It is held in memory only (docs/ai-agent-plan.md §5.3),
//! has no readable `Debug`, and is never written, logged or traced.

use std::fmt;

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }

    /// The value, for the one place that puts it in a request header.
    pub(crate) fn expose(&self) -> &str {
        self.0.trim()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_value() {
        let secret = Secret::new("fw_abc123");
        assert!(!format!("{secret:?}").contains("abc123"));
        assert!(!format!("{:?}", Some(&secret)).contains("abc123"));
    }
}
