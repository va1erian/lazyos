//! Who may do what (`rules.rs`), and the Wi-Fi key rule.

use crate::*;

/// A profile id of the form `wlanmd` mints.
const ID: &str = "w0123456789ab";

fn caller(uid: u32) -> Caller {
    Caller {
        uid,
        label_id: 0,
        session: 0,
    }
}

#[test]
fn authorization_table() {
    let session_user = Caller {
        uid: 1000,
        label_id: 0,
        session: 3,
    };
    let elevd = caller(ELEVD_UID);
    let wlan = caller(WLAN_UID);
    let pmk = |owner_uid| Op::Pmk { owner_uid };

    // User scope: always the caller's own uid, whatever the name.
    for op in [Op::Store, Op::Delete, Op::List] {
        for name in ["home", ID] {
            assert_eq!(
                authorize(session_user, "user", name, op),
                Ok(Owner::User(1000))
            );
        }
    }
    assert_eq!(
        authorize(elevd, "user", "home", Op::Store),
        Ok(Owner::User(ELEVD_UID))
    );
    // System scope: changes are elevd's alone, listing is open.
    for op in [Op::Store, Op::Delete] {
        assert_eq!(authorize(elevd, "system", "office", op), Ok(Owner::System));
        assert_eq!(authorize(elevd, "system", ID, op), Ok(Owner::System));
        for denied in [session_user, wlan, caller(0), caller(1001)] {
            for name in ["office", ID] {
                assert_eq!(
                    authorize(denied, "system", name, op),
                    Err(Denied::Perm),
                    "{denied:?}"
                );
            }
        }
    }
    assert_eq!(
        authorize(session_user, "system", "", Op::List),
        Ok(Owner::System)
    );
    // The PMK is wlanmd's alone, for the uid it names.
    assert_eq!(
        authorize(wlan, "user", ID, pmk(1000)),
        Ok(Owner::User(1000))
    );
    assert_eq!(authorize(wlan, "system", ID, pmk(0)), Ok(Owner::System));
    assert_eq!(
        authorize(wlan, "system", ID, pmk(1000)),
        Err(Denied::Invalid)
    );
    for denied in [session_user, elevd, caller(0), caller(1000), caller(913)] {
        for scope in ["user", "system"] {
            assert_eq!(
                authorize(denied, scope, ID, pmk(0)),
                Err(Denied::Perm),
                "{denied:?}"
            );
            assert_eq!(authorize(denied, scope, ID, pmk(1000)), Err(Denied::Perm));
        }
    }
    // Unlabelled and outside a session are part of the identity: an app under
    // a label, or a task in a session, with wlanmd's uid is not wlanmd.
    for forged in [
        Caller {
            label_id: 4,
            ..wlan
        },
        Caller { session: 1, ..wlan },
    ] {
        assert_eq!(authorize(forged, "user", ID, pmk(1000)), Err(Denied::Perm));
    }
    for forged in [
        Caller {
            label_id: 4,
            ..elevd
        },
        Caller {
            session: 1,
            ..elevd
        },
    ] {
        assert_eq!(
            authorize(forged, "system", "office", Op::Store),
            Err(Denied::Perm)
        );
    }
    // No scope but the two.
    for scope in ["", "User", "all", "system "] {
        assert_eq!(authorize(elevd, scope, "", Op::List), Err(Denied::Invalid));
    }
}

#[test]
fn only_wlanmd_stores_and_deletes_for_a_user() {
    let wlan = caller(WLAN_UID);
    for op in [
        Op::StoreFor { owner_uid: 1000 },
        Op::DeleteFor { owner_uid: 1000 },
    ] {
        assert_eq!(authorize(wlan, "user", ID, op), Ok(Owner::User(1000)));
        for denied in [
            caller(ELEVD_UID),
            caller(0),
            caller(1000),
            caller(913),
            Caller {
                label_id: 4,
                ..wlan
            },
            Caller { session: 2, ..wlan },
        ] {
            for scope in ["user", "system"] {
                assert_eq!(
                    authorize(denied, scope, ID, op),
                    Err(Denied::Perm),
                    "{denied:?}"
                );
            }
        }
    }
    assert_eq!(
        authorize(wlan, "users", ID, Op::StoreFor { owner_uid: 1 }),
        Err(Denied::Invalid)
    );
}

#[test]
fn wlanmd_never_writes_the_system_scope() {
    // A `system` secret is elevd's (`net.wifi.system`): a compromised wlanmd
    // must not plant or remove one without an administrator.
    let wlan = caller(WLAN_UID);
    for owner_uid in [0, 1000] {
        for op in [Op::StoreFor { owner_uid }, Op::DeleteFor { owner_uid }] {
            assert_eq!(authorize(wlan, "system", ID, op), Err(Denied::Perm));
        }
    }
}

#[test]
fn wlanmd_names_only_wifi_profiles() {
    // A compromised wlanmd must not reach a user's other secrets: no PMK of
    // them (crackable offline), no overwrite, no delete.
    let wlan = caller(WLAN_UID);
    let long = alloc::format!("{ID}0");
    for name in [
        "home",
        "",
        "w0123456789a",
        long.as_str(),
        "w0123456789AB",
        "x0123456789ab",
        "w0123456789ag",
        "w-123456789ab",
        "w0123456789a:",
    ] {
        for op in [
            Op::Pmk { owner_uid: 1000 },
            Op::StoreFor { owner_uid: 1000 },
            Op::DeleteFor { owner_uid: 1000 },
        ] {
            assert_eq!(
                authorize(wlan, "user", name, op),
                Err(Denied::Perm),
                "{name:?} {op:?}"
            );
        }
        assert_eq!(
            authorize(wlan, "system", name, Op::Pmk { owner_uid: 0 }),
            Err(Denied::Perm),
            "{name:?}"
        );
    }
    for good in ["w000000000000", "wffffffffffff", ID] {
        assert!(is_wifi_profile_name(good), "{good}");
    }
}

/// The rule here is the one `wlanmd` mints ids by (`wlansta::profile`):
/// drift between the two would lock every network out of its passphrase.
#[test]
fn profile_names_match_wlansta() {
    for random in [[0u8; 6], [0xff; 6], [0x01, 0x23, 0x45, 0x67, 0x89, 0xab]] {
        let id = wlansta::profile::new_id(random);
        assert!(is_wifi_profile_name(&id), "{id}");
        assert!(valid_name(&id), "{id}");
    }
    for name in [
        "home",
        "w0123456789AB",
        "w0123456789a",
        "w0123456789abc",
        ID,
    ] {
        assert_eq!(
            is_wifi_profile_name(name),
            wlansta::profile::valid_id(name),
            "{name}"
        );
    }
}

/// `keyd` answers PMKs to the uid the registry lets claim `os.lazy.net.wifi`
/// and `confd` lets write the Wi-Fi profiles: one value in two crates, so a
/// drift fails here (CI runs `cargo test -p secretstore`).
#[test]
fn wlan_uid_matches_netpolicy() {
    assert_eq!(WLAN_UID, netpolicy::WLAN_UID);
}

#[test]
fn wifi_keys_are_utf8_passphrases_or_raw_psks() {
    let euros = "€".repeat(21);
    let too_many = "€".repeat(22);
    for good in [
        &b"password"[..],
        "contraseña".as_bytes(),
        euros.as_bytes(),
        &[b'a'; 63],
        &[b'0'; 64],
    ] {
        assert!(valid_wifi_key(good), "{good:?}");
    }
    for bad in [
        &b"short"[..],
        &[b'a'; 65],
        &[b'g'; 64],
        b"pass\nword",
        b"\xff\xfe\xfd\xfc\xfb\xfa\xf9\xf8",
        too_many.as_bytes(),
    ] {
        assert!(!valid_wifi_key(bad), "{bad:?}");
    }
    let mut store = Store::new();
    store
        .put(Owner::System, ID, "contraseña".as_bytes())
        .unwrap();
    assert!(store.pmk(Owner::System, ID, b"net").is_ok());
}
