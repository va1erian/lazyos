//! The Intel `.ddc` device configuration file: a list of records, each a
//! length octet `L` followed by `L` octets (a 16-bit parameter id and its
//! value), written with `Intel_Write_DDC` (`0xFC8B`) whose parameters are the
//! whole record, length octet included (`btintel_load_ddc_config`, Linux).

use core::ops::Range;

/// The record at `pos` as a range of the file, or `None` at the end or when
/// it is malformed: a record holds at least the id (so `L >= 2`), must fit in
/// one command (`L + 1 <= 255`) and must lie inside the file.
pub(crate) fn record(data: &[u8], pos: usize) -> Option<Range<usize>> {
    let len = usize::from(*data.get(pos)?);
    let total = len + 1;
    let end = pos.checked_add(total)?;
    (3..=255).contains(&total).then_some(())?;
    (end <= data.len()).then_some(pos..end)
}

/// Count the records, or say the file is malformed. Done before the first
/// record is sent so a damaged file writes nothing.
pub(crate) fn validate(data: &[u8]) -> Result<u16, &'static str> {
    let mut pos = 0;
    let mut count = 0u16;
    while pos < data.len() {
        let range = record(data, pos).ok_or("malformed DDC record")?;
        pos = range.end;
        count = count.checked_add(1).ok_or("too many DDC records")?;
    }
    Ok(count)
}
