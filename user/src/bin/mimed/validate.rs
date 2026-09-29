//! Wire-token validation for the MIME and open-with interface: request tokens,
//! MIME types, app ids and override extensions. Every untrusted field crossing
//! the Messenger boundary is checked here before it is stored or published.

/// A request token (app or verb): no whitespace and no topic separators.
pub(crate) fn valid_token(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+'))
}

/// Whether `text` is safe as the single `<app>` topic segment `open_path`
/// publishes to (`system/events/open/<app>`). The central broker's publish
/// validator (`messengerd`'s `valid_topic`, mirroring the kernel ACL gate)
/// accepts the same charset as [`valid_token`] but always refuses `+` and `#`
/// in a publish segment (they are subscribe-only wildcards), so `OPEN` would
/// resolve the app and then report `published=false` after retrying a
/// publish the broker can never accept. Registration is the point to catch
/// that, once, rather than every `OPEN` paying for 32 failed publish
/// attempts.
pub(crate) fn valid_app_id(text: &str) -> bool {
    valid_token(text) && !text.contains('+') && !text.contains('#')
}

/// A MIME type: `type/subtype`, no whitespace.
pub(crate) fn valid_mime(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text.contains('/')
        && !text.contains("//")
        && text.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'.' | b'+')
        })
}

/// An override extension token: a bare extension, no dot or separator.
pub(crate) fn valid_extension(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 16
        && !text.contains('.')
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+'))
}
