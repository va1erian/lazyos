//! The package manager's wire (`idl/pkgd.midl`) and the `Unregister` request
//! it relies on in `idl/mimed.midl`: records round-trip, and a truncated body is
//! an error, never a misread record (these bytes are stored in `confd` and in the
//! hash-chained audit log).

use messenger_generated::os_lazy_mimed_v1 as mimed;
use messenger_generated::os_lazy_pkgd_v1::*;

fn installed() -> Installed {
    Installed {
        system_name: "org.lazy.counter".into(),
        name: "Packaged Counter".into(),
        version: "1.0.0".into(),
        install_dir: "org.lazy.counter/1.0.0-0848a590".into(),
        digest: "ab".repeat(32),
        binary: "bin/counter.elf".into(),
        installed_at: 12_345,
        abi: "linux".into(),
        args: vec!["--client".into(), "second".into()],
    }
}

#[test]
fn installed_round_trips_with_abi_and_args() {
    let row = installed();
    let bytes = encode_installed(&row).unwrap();
    assert_eq!(decode_installed(&bytes).unwrap(), row);
    let bare = Installed::default();
    assert_eq!(
        decode_installed(&encode_installed(&bare).unwrap()).unwrap(),
        bare
    );
}

#[test]
fn a_row_stored_before_abi_and_args_existed_decodes_with_defaults() {
    // The two fields were appended; a record without them is still readable.
    let old_style = Installed {
        abi: String::new(),
        args: Vec::new(),
        ..installed()
    };
    let decoded = decode_installed(&encode_installed(&old_style).unwrap()).unwrap();
    assert_eq!(decoded.abi, "");
    assert!(decoded.args.is_empty());
}

#[test]
fn pkg_event_round_trips() {
    let event = PkgEvent {
        op: "denied".into(),
        system_name: "org.lazy.counter".into(),
        version: "1.0.0".into(),
        install_dir: "org.lazy.counter/1.0.0-0848a590".into(),
        digest: "cd".repeat(32),
        actor_uid: 1000,
        ok: false,
        detail: "only a logged-in user may install".into(),
    };
    assert_eq!(
        decode_pkg_event(&encode_pkg_event(&event).unwrap()).unwrap(),
        event
    );
}

#[test]
fn package_info_round_trips() {
    let info = PackageInfo {
        name: "Demo".into(),
        system_name: "org.lazy.demo".into(),
        author: "A".into(),
        version: "1.0.0".into(),
        description: "d".into(),
        digest: "00".repeat(32),
        install_dir: "org.lazy.demo/1.0.0-00000000".into(),
        mime: vec![MimeHandler {
            mime_type: "image/png".into(),
            verbs: vec!["open".into(), "edit".into()],
            has_icon: true,
        }],
        permissions: vec![Permission {
            kind: "interface".into(),
            value: "os.lazy.clipboard.v1".into(),
            risk: "medium".into(),
            explanation: "Read and change what you copy and paste".into(),
        }],
        problems: vec!["one".into(), "two".into()],
    };
    assert_eq!(
        decode_package_info(&encode_package_info(&info).unwrap()).unwrap(),
        info
    );
}

#[test]
fn truncated_rows_are_errors_not_misreads() {
    let bytes = encode_installed(&installed()).unwrap();
    for cut in 1..bytes.len() {
        if let Ok(row) = decode_installed(&bytes[..cut]) {
            assert_ne!(row, installed(), "a cut at {cut} decoded as the whole row");
        }
    }
}

#[test]
fn the_event_topic_names_the_operation() {
    assert_eq!(
        name_system_events_pkg("install").unwrap(),
        "system/events/pkg/install"
    );
    assert!(name_system_events_pkg("a/b").is_err());
    assert_eq!(TOPIC_SYSTEM_EVENTS_PKG, "system/events/pkg/+");
}

#[test]
fn mime_unregister_round_trips() {
    let args = mimed::UnregisterArgs {
        mime: "image/png".into(),
        app: "org.lazy.counter".into(),
        verb: "open".into(),
    };
    assert_eq!(
        mimed::decode_unregister_args(&mimed::encode_unregister_args(&args).unwrap()).unwrap(),
        args
    );
}
