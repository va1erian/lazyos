//! What a password must be (docs/accounts-plan.md U1): one rule, applied by
//! `accountsd` to every new password (Create, SetPassword, elevd's
//! `account.create`/`account.password`), and checked ahead of time by the
//! login screen's first-boot setup and the Settings Accounts page, so they
//! refuse exactly what the service would.

/// The fewest characters a password has. Short enough for the development
/// accounts (`lazy`), long enough that a one-key password is refused.
pub const MIN_SECRET: usize = 4;
/// The most bytes a password has: what `keyd` derives a verifier from.
pub const MAX_SECRET: usize = 64;
/// Why a password was refused.
pub const SECRET_RULE: &str = "a password is 4 to 64 characters, without control characters";

/// Whether `secret` may become a password: [`MIN_SECRET`] characters or
/// more, at most [`MAX_SECRET`] bytes, no control character.
pub fn check_secret(secret: &str) -> Result<(), &'static str> {
    let fits = secret.chars().count() >= MIN_SECRET && secret.len() <= MAX_SECRET;
    if !fits || secret.chars().any(char::is_control) {
        return Err(SECRET_RULE);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn the_length_and_character_rules() {
        for good in ["lazy", "nimda", "élan", &"x".repeat(MAX_SECRET)] {
            assert_eq!(check_secret(good), Ok(()), "{good}");
        }
        for bad in ["", "abc", "ab\tc", "abcd\n", &"x".repeat(MAX_SECRET + 1)] {
            assert_eq!(check_secret(bad), Err(SECRET_RULE), "{bad:?}");
        }
        // Characters, not bytes, count toward the minimum; bytes toward the
        // maximum (keyd's buffer).
        assert!(check_secret("ééé").is_err());
        assert!(check_secret(&"é".repeat(33)).is_err());
    }

    #[test]
    fn the_rule_text_states_the_limits() {
        assert!(SECRET_RULE.contains(&MIN_SECRET.to_string()));
        assert!(SECRET_RULE.contains(&MAX_SECRET.to_string()));
    }
}
