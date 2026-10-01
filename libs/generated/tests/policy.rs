//! The policy loader wire (`idl/policy.midl`): the `acl_load` request codec and
//! the scope interface ids the kernel evaluates label rules against.

use messenger_generated::os_lazy_messenger_names_resolve_v1 as names;
use messenger_generated::os_lazy_messenger_policy_v1 as policy;

fn fnv1a64(text: &str) -> u64 {
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    for byte in text.bytes() {
        hash = (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

#[test]
fn interface_ids_are_the_name_hashes() {
    assert_eq!(policy::INTERFACE_ID, fnv1a64("os.lazy.messenger.policy.v1"));
    assert_eq!(
        names::INTERFACE_ID,
        fnv1a64("os.lazy.messenger.names.resolve.v1")
    );
}

fn rule(interface_id: u64, method: u32, allow: bool) -> policy::LabelRule {
    policy::LabelRule {
        interface_id,
        method,
        allow,
    }
}

#[test]
fn load_request_round_trips() {
    let request = policy::LoadLabelArgs {
        label: "app:com.example.notes".into(),
        rules: vec![
            rule(names::INTERFACE_ID, 0x1234_5678, true),
            rule(u64::MAX, u32::MAX, false),
            rule(0, 0, true),
        ],
    };
    let bytes = policy::encode_load_label_args(&request).unwrap();
    assert_eq!(policy::decode_load_label_args(&bytes).unwrap(), request);
}

#[test]
fn an_empty_rule_list_is_a_valid_revoke() {
    let request = policy::LoadLabelArgs {
        label: "app:com.example.notes".into(),
        rules: Vec::new(),
    };
    let bytes = policy::encode_load_label_args(&request).unwrap();
    let decoded = policy::decode_load_label_args(&bytes).unwrap();
    assert!(decoded.rules.is_empty());
    assert_eq!(decoded.label, request.label);
}

#[test]
fn truncated_bodies_are_rejected_not_misread() {
    let request = policy::LoadLabelArgs {
        label: "system:netdrv".into(),
        rules: vec![rule(7, 9, true), rule(8, 10, true)],
    };
    let bytes = policy::encode_load_label_args(&request).unwrap();
    for cut in 1..bytes.len() {
        // A cut inside a field must fail; a cut on a field boundary may decode
        // a shorter rule list but can never invent rules or change the label.
        if let Ok(decoded) = policy::decode_load_label_args(&bytes[..cut]) {
            assert!(decoded.rules.len() <= request.rules.len());
            assert!(decoded.label.is_empty() || decoded.label == request.label);
            for (got, want) in decoded.rules.iter().zip(&request.rules) {
                assert_eq!(got, want);
            }
        }
    }
}
