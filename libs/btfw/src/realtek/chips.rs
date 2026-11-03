//! Which Realtek controller this is, and which files it takes.
//!
//! The ROM identifies itself in `Read_Local_Version` (LMP subversion, HCI
//! revision, HCI version); the match and the file stems are the USB rows of
//! the table in Linux's `drivers/bluetooth/btrtl.c` (`ic_id_table`). The
//! firmware is named `rtl_bt/<stem>_fw.bin` and the optional per-board
//! configuration `rtl_bt/<stem>_config.bin`.

/// LMP subversions of the ROMs (the first digits of the chip name).
pub(crate) const LMP_8723A: u16 = 0x1200;
pub(crate) const LMP_8723B: u16 = 0x8723;
pub(crate) const LMP_8821A: u16 = 0x8821;
pub(crate) const LMP_8761A: u16 = 0x8761;
pub(crate) const LMP_8822B: u16 = 0x8822;
pub(crate) const LMP_8852A: u16 = 0x8852;
pub(crate) const LMP_8851B: u16 = 0x8851;
pub(crate) const LMP_8922A: u16 = 0x8922;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Chip {
    pub lmp_subver: u16,
    pub hci_rev: u16,
    /// `0` matches any HCI version.
    pub hci_ver: u8,
    pub stem: &'static str,
    /// Whether the ROM has the `0xFC6D` ROM-version command; the patch to
    /// use depends on it.
    pub has_rom_version: bool,
    /// Whether the board configuration file must exist.
    pub config_needed: bool,
    /// Whether there is a configuration file at all.
    pub has_config: bool,
}

const fn chip(
    lmp_subver: u16,
    hci_rev: u16,
    hci_ver: u8,
    stem: &'static str,
    config_needed: bool,
) -> Chip {
    Chip {
        lmp_subver,
        hci_rev,
        hci_ver,
        stem,
        has_rom_version: lmp_subver != LMP_8723A,
        config_needed,
        has_config: lmp_subver != LMP_8723A,
    }
}

pub(crate) const CHIPS: &[Chip] = &[
    chip(LMP_8723A, 0xb, 0x6, "rtl8723a", false),
    chip(LMP_8723B, 0xb, 0x6, "rtl8723b", false),
    chip(LMP_8723B, 0xd, 0x8, "rtl8723d", true),
    chip(LMP_8821A, 0xa, 0x6, "rtl8821a", false),
    chip(LMP_8821A, 0xc, 0x8, "rtl8821c", false),
    chip(LMP_8761A, 0xa, 0x6, "rtl8761a", false),
    chip(LMP_8761A, 0xb, 0xa, "rtl8761bu", false),
    chip(LMP_8761A, 0x0e, 0, "rtl8761cu", false),
    chip(LMP_8822B, 0xc, 0xa, "rtl8822cu", false),
    chip(LMP_8822B, 0xb, 0x7, "rtl8822b", true),
    chip(LMP_8852A, 0xa, 0xb, "rtl8852au", false),
    chip(LMP_8852A, 0xb, 0xb, "rtl8852bu", false),
    chip(LMP_8852A, 0xc, 0xc, "rtl8852cu", false),
    chip(LMP_8851B, 0xb, 0xc, "rtl8851bu", false),
    chip(LMP_8922A, 0xa, 0xc, "rtl8922au", false),
    chip(LMP_8852A, 0x87, 0xc, "rtl8852btu", false),
];

/// The chip for what `Read_Local_Version` reported, if we know it.
pub(crate) fn find(lmp_subver: u16, hci_rev: u16, hci_ver: u8) -> Option<&'static Chip> {
    CHIPS.iter().find(|c| {
        c.lmp_subver == lmp_subver
            && c.hci_rev == hci_rev
            && (c.hci_ver == 0 || c.hci_ver == hci_ver)
    })
}
