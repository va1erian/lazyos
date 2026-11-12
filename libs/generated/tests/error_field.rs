//! The standard error field id has two spellings, `midlc`'s generated
//! `errors::ERROR_FIELD` and the client runtime's `libmessenger::client`
//! constant (the crate cannot depend on generated code). They must agree.

use messenger_generated::errors;

#[test]
fn the_client_runtime_reads_the_field_midlc_writes() {
    assert_eq!(libmessenger::client::ERROR_FIELD, errors::ERROR_FIELD);
}
