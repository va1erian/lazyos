"""midlc errors: the standard reply error every service shares.

Part of the Messenger IDL compiler; see `midlc.py` for the CLI and
`docs/midl.md` ("Errors") for the rule. A failed call is answered with one
`Error` field at [`ERROR_FIELD`] instead of the declared reply fields. Its
payload is the errno-style code and the message (what every service sent
before this was standardized), optionally followed by a NUL and a detail
record carrying the rest of the shape of `docs/messenger.md` section 12:

    1 domain: String   the code's namespace ("" means the service's errno)
    2 hint:   String   how to fix it
    3 docs:   String   a documentation id ("err.messenger.denied")

The id is generated, never hand-typed: the parser refuses it as a reply
field id, and `ERRORS_SUPPORT` below is the crate-level `errors` module of
`libs/generated` that services and clients write and read it with.
"""

from __future__ import annotations

from midlc_model import ERROR_FIELD

DETAIL_FIELDS = {"domain": 1, "hint": 2, "docs": 3}

ERRORS_SUPPORT = """\
/// The standard reply error (`docs/midl.md`, "Errors"): one `Error` field at
/// [`errors::ERROR_FIELD`] answers a failed call instead of the declared reply.
#[rustfmt::skip]
pub mod errors {
    use alloc::string::String;
    use alloc::vec::Vec;
    use libmessenger::{Decoder, Encoder, Error, Field, Kind};

    /// The reply field id of the standard error, the same in every reply of
    /// every interface (`midlc` refuses it as a declared reply field id).
    pub const ERROR_FIELD: u16 = __ERROR_FIELD__;
    const DOMAIN: u16 = __DOMAIN__;
    const HINT: u16 = __HINT__;
    const DOCS: u16 = __DOCS__;

    /// A decoded standard error. `domain` is empty for the service's own
    /// errno-style codes.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct ReplyError {
        pub domain: String,
        pub code: u32,
        pub message: String,
        pub hint: Option<String>,
        pub docs: Option<String>,
    }

    impl ReplyError {
        /// An error with a code and a message and nothing else.
        pub fn new(code: u32, message: &str) -> Self {
            ReplyError { code, message: message.into(), ..ReplyError::default() }
        }

        /// The same error in `domain`.
        pub fn with_domain(mut self, domain: &str) -> Self {
            self.domain = domain.into();
            self
        }

        /// The same error with a hint on how to fix it.
        pub fn with_hint(mut self, hint: &str) -> Self {
            self.hint = Some(hint.into());
            self
        }

        /// The same error with a documentation id.
        pub fn with_docs(mut self, docs: &str) -> Self {
            self.docs = Some(docs.into());
            self
        }
    }

    /// Whether `field` is the standard error.
    pub fn is_error(field: &Field<'_>) -> bool {
        field.kind == Kind::Error && field.id == ERROR_FIELD
    }

    /// Append `error` to `target` as the standard error field. Without a
    /// domain, hint or docs it is exactly a code and a message.
    pub fn write(target: &mut Encoder, error: &ReplyError) -> Result<(), Error> {
        if error.domain.is_empty() && error.hint.is_none() && error.docs.is_none() {
            return target.error(ERROR_FIELD, error.code, &error.message);
        }
        let mut detail = Encoder::new();
        if !error.domain.is_empty() {
            detail.string(DOMAIN, &error.domain)?;
        }
        if let Some(hint) = &error.hint {
            detail.string(HINT, hint)?;
        }
        if let Some(docs) = &error.docs {
            detail.string(DOCS, docs)?;
        }
        target.error_detail(ERROR_FIELD, error.code, &error.message, &detail)
    }

    /// Append an errno-style `code` and `message` as the standard error.
    pub fn write_code(target: &mut Encoder, code: u32, message: &str) -> Result<(), Error> {
        target.error(ERROR_FIELD, code, message)
    }

    /// A reply body holding only `error`.
    pub fn encode(error: &ReplyError) -> Result<Vec<u8>, Error> {
        let mut target = Encoder::new();
        write(&mut target, error)?;
        Ok(target.finish())
    }

    /// Decode one standard error field (see [`is_error`]).
    pub fn decode(field: &Field<'_>) -> Result<ReplyError, Error> {
        let (code, message) = field.error_parts()?;
        let mut error = ReplyError::new(code, message);
        if let Some(mut detail) = field.error_detail() {
            while let Some(item) = detail.next()? {
                match item.id {
                    DOMAIN => error.domain = item.as_str()?.into(),
                    HINT => error.hint = Some(item.as_str()?.into()),
                    DOCS => error.docs = Some(item.as_str()?.into()),
                    _ => {}
                }
            }
        }
        Ok(error)
    }

    /// The standard error of a reply `body`, or `None` for a success.
    pub fn find(body: &[u8]) -> Result<Option<ReplyError>, Error> {
        let mut decoder = Decoder::new(body);
        while let Some(field) = decoder.next()? {
            if is_error(&field) {
                return decode(&field).map(Some);
            }
        }
        Ok(None)
    }

    /// The code and message of a reply `body`'s standard error, borrowed.
    pub fn find_code(body: &[u8]) -> Result<Option<(u32, &str)>, Error> {
        let mut decoder = Decoder::new(body);
        while let Some(field) = decoder.next()? {
            if is_error(&field) {
                return field.error_parts().map(Some);
            }
        }
        Ok(None)
    }
}
"""

ERRORS_SUPPORT = (
    ERRORS_SUPPORT.replace("__ERROR_FIELD__", str(ERROR_FIELD))
    .replace("__DOMAIN__", str(DETAIL_FIELDS["domain"]))
    .replace("__HINT__", str(DETAIL_FIELDS["hint"]))
    .replace("__DOCS__", str(DETAIL_FIELDS["docs"]))
)
