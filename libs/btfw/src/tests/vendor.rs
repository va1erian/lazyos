//! Which loader a device gets.

use bthci::ret::LocalVersion;

use crate::ids::REALTEK;
use crate::step::{Done, Input, Step};
use crate::{Loader, Vendor};

#[test]
fn the_realtek_id_table_is_sorted_for_binary_search() {
    assert!(
        REALTEK.windows(2).all(|w| w[0] < w[1]),
        "sorted, no duplicates"
    );
}

#[test]
fn intel_radios_by_usb_id() {
    for pid in [
        0x0025, 0x0026, 0x0029, 0x0032, 0x0033, 0x0a2b, 0x0aaa, 0x0a5a,
    ] {
        assert_eq!(
            Vendor::from_usb(0x8087, pid),
            Vendor::Intel,
            "8087:{pid:04x}"
        );
    }
    assert_eq!(
        Vendor::from_usb(0x8087, 0x07da),
        Vendor::Generic,
        "a CSR clone"
    );
}

#[test]
fn realtek_radios_by_usb_id() {
    // RTL8822CE, RTL8761BU dongles, RTL8852 cards, a third-party OEM id.
    for (vid, pid) in [
        (0x0bda, 0xb00c),
        (0x0bda, 0xc822),
        (0x0bda, 0x8771),
        (0x2550, 0x8761),
        (0x0bda, 0x4852),
        (0x13d3, 0x3571),
        (0x0bda, 0x1234), // any Realtek product
    ] {
        assert_eq!(
            Vendor::from_usb(vid, pid),
            Vendor::Realtek,
            "{vid:04x}:{pid:04x}"
        );
    }
}

#[test]
fn everything_else_is_generic() {
    for (vid, pid) in [(0x0a12, 0x0001), (0x0a5c, 0x21e8), (0x46f4, 0x0001), (0, 0)] {
        assert_eq!(Vendor::from_usb(vid, pid), Vendor::Generic);
    }
}

fn version(manufacturer: u16) -> LocalVersion {
    LocalVersion {
        hci_version: 0xb,
        hci_revision: 0,
        lmp_version: 0xb,
        manufacturer,
        lmp_subversion: 0,
    }
}

#[test]
fn the_manufacturer_wins_when_it_names_a_vendor() {
    assert_eq!(Vendor::from_local_version(&version(2)), Vendor::Intel);
    assert_eq!(Vendor::from_local_version(&version(93)), Vendor::Realtek);
    assert_eq!(Vendor::from_local_version(&version(15)), Vendor::Generic);
    // A Realtek-branded id carrying an Intel part: believe the radio.
    assert_eq!(
        Vendor::identify(Some((0x0bda, 0x1)), Some(&version(2))),
        Vendor::Intel
    );
    // An unhelpful manufacturer falls back to the USB ids.
    assert_eq!(
        Vendor::identify(Some((0x8087, 0x26)), Some(&version(0xffff))),
        Vendor::Intel
    );
    assert_eq!(Vendor::identify(None, None), Vendor::Generic);
    assert_eq!(Vendor::identify(None, Some(&version(93))), Vendor::Realtek);
}

#[test]
fn a_generic_controller_finishes_at_once() {
    let mut loader = Loader::new(Vendor::Generic);
    assert_eq!(
        loader.step(0, Input::Start),
        Step::Done(Done::NoFirmwareNeeded)
    );
    assert_eq!(loader.deadline(), None);
    assert_eq!(loader.step(1, Input::Tick), Step::Wait);
}
