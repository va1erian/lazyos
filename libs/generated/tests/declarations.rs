//! The protocol facts a `.midl` declares (`service`, `errors`, `scope`,
//! `deadline`) and the constants `midlc` generates from them, checked against
//! the values the system relies on.

use messenger_generated::{
    declared_service, os_lazy_audio_v1 as audio, os_lazy_clipboard_v1 as clipboard,
    os_lazy_confd_v1 as confd, os_lazy_display_cursor_v1 as cursor, os_lazy_input_v1 as input,
    os_lazy_net_nic_v1 as nic, DECLARED_SERVICES,
};

/// FNV-1a 64, the hash the kernel and `midlc` give an interface name.
const fn fnv1a64(text: &str) -> u64 {
    let bytes = text.as_bytes();
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    let mut index = 0;
    while index < bytes.len() {
        hash = (hash ^ bytes[index] as u64).wrapping_mul(0x0000_0100_0000_01B3);
        index += 1;
    }
    hash
}

#[test]
fn a_client_gets_its_service_name_from_the_interface() {
    assert_eq!(confd::SERVICE_NAME, "os.lazy.confd");
    assert_eq!(clipboard::SERVICE_NAME, "os.lazy.clipboard");
    // Names that keep their `.vN` are declared as they are registered.
    assert_eq!(input::SERVICE_NAME, "os.lazy.input.v1");
    // A scope interface of the display is served on the display endpoint.
    assert_eq!(cursor::SERVICE_NAME, "os.lazy.display.v1");
}

#[test]
fn a_family_has_a_prefix_not_a_name() {
    assert_eq!(nic::SERVICE_INSTANCE_PREFIX, "os.lazy.net.nic/");
    // The mixer name and the per-card family both serve `audio`.
    assert_eq!(audio::SERVICE_NAME, "os.lazy.audio");
    assert_eq!(audio::SERVICE_INSTANCE_PREFIX, "os.lazy.audio.card/");
}

#[test]
fn the_service_table_finds_the_serving_name() {
    let confd_service = declared_service(confd::INTERFACE_ID).expect("confd is declared");
    assert_eq!(confd_service.name, "os.lazy.confd");
    assert!(!confd_service.instanced);
    // The singleton wins over the family for an interface served as both.
    assert_eq!(
        declared_service(audio::INTERFACE_ID).unwrap().name,
        "os.lazy.audio"
    );
    assert!(declared_service(nic::INTERFACE_ID).unwrap().instanced);
    assert!(declared_service(0x1234).is_none());
    let mut names: Vec<_> = DECLARED_SERVICES.iter().map(|s| s.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        names.len(),
        DECLARED_SERVICES.len(),
        "a name is declared once"
    );
}

#[test]
fn clipboard_scopes_are_the_name_hashes_the_kernel_checks() {
    assert_eq!(
        clipboard::METHOD_OFFER_SCOPE,
        fnv1a64("os.lazy.clipboard.write.v1")
    );
    assert_eq!(
        clipboard::METHOD_REQUEST_SCOPE,
        fnv1a64("os.lazy.clipboard.read.v1")
    );
}

#[test]
fn clipboard_deadlines_are_the_bounds_clients_use() {
    assert_eq!(clipboard::METHOD_OFFER_DEADLINE_MS, 500);
    assert_eq!(clipboard::METHOD_REQUEST_DEADLINE_MS, 500);
    assert_eq!(clipboard::METHOD_OFFERBUFFER_DEADLINE_MS, 5_000);
    assert_eq!(clipboard::METHOD_FETCH_DEADLINE_MS, 5_000);
}

#[test]
fn confd_errors_keep_their_codes_and_round_trip() {
    use confd::ServiceError;
    let expected = [
        (ServiceError::NotFound, 1001),
        (ServiceError::BadPath, 1002),
        (ServiceError::TooLarge, 1003),
        (ServiceError::Denied, 1004),
        (ServiceError::Io, 1005),
        (ServiceError::BadValue, 1006),
    ];
    assert_eq!(confd::SERVICE_ERRORS.len(), expected.len());
    for (error, code) in expected {
        assert_eq!(error.code(), code);
        assert_eq!(ServiceError::from_code(code), Some(error));
    }
    assert_eq!(ServiceError::from_code(0), None);
    assert_eq!(ServiceError::from_code(1007), None);
    assert_eq!(ServiceError::NotFound.name(), "NotFound");
}

#[test]
fn confd_value_kind_keeps_its_wire_numbers() {
    // `Value.kind` was a `U32` with these discriminants before it was an enum.
    assert_eq!(
        [
            confd::KIND_BOOL,
            confd::KIND_I64,
            confd::KIND_U64,
            confd::KIND_STR,
            confd::KIND_BYTES
        ],
        [0, 1, 2, 3, 4]
    );
    let value = confd::Value {
        kind: confd::KIND_STR,
        str_value: Some("light".into()),
        ..confd::Value::default()
    };
    let body = confd::encode_value(&value).unwrap();
    assert_eq!(confd::decode_value(&body).unwrap(), value);
}
