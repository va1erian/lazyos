//! Delimiter and value tags (RFC 8010 3.5).

/// `operation-attributes-tag`.
pub const OPERATION: u8 = 0x01;
/// `job-attributes-tag`.
pub const JOB: u8 = 0x02;
/// `end-of-attributes-tag`.
pub const END: u8 = 0x03;
/// `printer-attributes-tag`.
pub const PRINTER: u8 = 0x04;
/// `unsupported-attributes-tag`.
pub const UNSUPPORTED_GROUP: u8 = 0x05;
/// The highest delimiter tag (`0x00..=0x0F` are delimiters).
pub const MAX_DELIMITER: u8 = 0x0F;

/// Out-of-band `unsupported`.
pub const UNSUPPORTED: u8 = 0x10;
/// Out-of-band `unknown`.
pub const UNKNOWN: u8 = 0x12;
/// Out-of-band `no-value`.
pub const NO_VALUE: u8 = 0x13;

pub const INTEGER: u8 = 0x21;
pub const BOOLEAN: u8 = 0x22;
pub const ENUM: u8 = 0x23;
pub const OCTET_STRING: u8 = 0x30;
pub const DATE_TIME: u8 = 0x31;
pub const RESOLUTION: u8 = 0x32;
pub const RANGE_OF_INTEGER: u8 = 0x33;
pub const BEG_COLLECTION: u8 = 0x34;
pub const TEXT_WITH_LANGUAGE: u8 = 0x35;
pub const NAME_WITH_LANGUAGE: u8 = 0x36;
pub const END_COLLECTION: u8 = 0x37;
pub const TEXT: u8 = 0x41;
pub const NAME: u8 = 0x42;
pub const KEYWORD: u8 = 0x44;
pub const URI: u8 = 0x45;
pub const URI_SCHEME: u8 = 0x46;
pub const CHARSET: u8 = 0x47;
pub const NATURAL_LANGUAGE: u8 = 0x48;
pub const MIME_MEDIA_TYPE: u8 = 0x49;
pub const MEMBER_ATTR_NAME: u8 = 0x4A;
/// The extension tag: a 4-byte tag follows. Refused on decode.
pub const EXTENSION: u8 = 0x7F;

/// Whether `tag` is one of the plain string kinds [`crate::Value::String`]
/// carries.
pub fn is_string(tag: u8) -> bool {
    matches!(
        tag,
        TEXT | NAME | KEYWORD | URI | URI_SCHEME | CHARSET | NATURAL_LANGUAGE | MIME_MEDIA_TYPE
    )
}

/// Whether `tag` is an out-of-band value (`0x10..=0x1F`).
pub fn is_out_of_band(tag: u8) -> bool {
    (0x10..=0x1F).contains(&tag)
}
