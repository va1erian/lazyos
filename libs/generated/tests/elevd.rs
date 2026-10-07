//! Round-trip tests for the generated `os.lazy.elevd.v1` and
//! `os.lazy.display.prompt.v1` stubs (docs/accounts-plan.md U2, issue #625).

use messenger_generated::os_lazy_display_prompt_v1 as prompt;
use messenger_generated::os_lazy_elevd_v1::*;

#[test]
fn a_request_and_its_reply_roundtrip() {
    let args = RequestArgs {
        operation: "conf.set".into(),
        args: vec!["sys/ui/demo".into(), "str".into(), String::new()],
    };
    let body = encode_request_args(&args).unwrap();
    assert_eq!(decode_request_args(&body).unwrap(), args);
    let reply = RequestReply {
        detail: "Set sys/ui/demo".into(),
        values: vec!["str".into(), "dark".into()],
    };
    let body = encode_request_reply(&reply).unwrap();
    assert_eq!(decode_request_reply(&body).unwrap(), reply);
}

#[test]
fn the_audit_record_roundtrips_on_its_topic() {
    let record = Record {
        operation: "account.create".into(),
        summary: "Create the account 'bob' (a user)".into(),
        uid: 1000,
        user: "user".into(),
        label: 7,
        admin: "admin".into(),
        outcome: "granted".into(),
    };
    let body = encode_record(&record).unwrap();
    assert_eq!(decode_record(&body).unwrap(), record);
    assert_eq!(TOPIC_SYSTEM_EVENTS_ELEVD_REQUEST, "system/events/elevd/request");
}

#[test]
fn the_prompt_roundtrips_and_names_its_outcomes() {
    let args = prompt::PromptArgs {
        summary: "Set the clock".into(),
        uid: 1000,
        user: "user".into(),
        label_id: 0,
        admin: String::new(),
        error: "That is not an administrator's name and password.".into(),
    };
    let body = prompt::encode_prompt_args(&args).unwrap();
    assert_eq!(prompt::decode_prompt_args(&body).unwrap(), args);
    let reply = prompt::PromptReply {
        outcome: prompt::PROMPT_OUTCOME_APPROVED,
        name: "admin".into(),
        secret: "nimda".into(),
    };
    let body = prompt::encode_prompt_reply(&reply).unwrap();
    assert_eq!(prompt::decode_prompt_reply(&body).unwrap(), reply);
    assert_ne!(prompt::PROMPT_OUTCOME_CANCELLED, prompt::PROMPT_OUTCOME_APPROVED);
    assert_ne!(prompt::PROMPT_OUTCOME_TIMED_OUT, prompt::PROMPT_OUTCOME_CANCELLED);
}
