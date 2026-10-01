use super::*;
use crate::store::MemStore;
use crate::tree::Tree;

fn store() -> MemStore {
    MemStore::new()
}

fn new_key(path: &str, kind: Kind, text: &str) -> NewKeyEditor {
    NewKeyEditor {
        path: path.into(),
        kind,
        text: text.into(),
        ..NewKeyEditor::default()
    }
}

#[test]
fn a_list_error_leaves_the_previous_tree() {
    let store = store();
    store.seed("sys/a", Value::Bool(true));
    let mut tree = Tree::new(vec!["sys/old".to_string()]);
    let before = tree.paths().to_vec();
    *store.fail_lists.borrow_mut() = Some(StoreError::Io);
    assert!(reload_tree(&mut tree, "", &store).is_err());
    assert_eq!(tree.paths(), before.as_slice());
}

#[test]
fn apply_writes_and_clears_dirty() {
    let store = store();
    store.seed("sys/ui/mode", Value::Str("dark".into()));
    let mut editor = KeyEditor::default();
    editor.select(Some("sys/ui/mode".into()), &store);
    assert_eq!(editor.kind(), Kind::Str);
    assert!(!editor.dirty());
    editor.set_text("light".into());
    assert!(editor.dirty());
    assert_eq!(editor.apply(&store), Ok("saved sys/ui/mode".into()));
    assert!(!editor.dirty());
    assert_eq!(
        store.get("sys/ui/mode"),
        Ok(Some(Value::Str("light".into())))
    );
}

#[test]
fn apply_with_fail_writes_leaves_state_and_store_intact() {
    let store = store();
    store.seed("sys/ui/mode", Value::Str("dark".into()));
    let mut editor = KeyEditor::default();
    editor.select(Some("sys/ui/mode".into()), &store);
    editor.set_text("light".into());
    *store.fail_writes.borrow_mut() = Some(StoreError::Io);
    assert!(editor.apply(&store).is_err());
    // Displayed state: the edit is still there, the stored value untouched.
    assert_eq!(editor.text(), "light");
    assert_eq!(editor.stored(), Some(&Value::Str("dark".into())));
    assert!(editor.dirty());
    assert_eq!(
        store.get("sys/ui/mode"),
        Ok(Some(Value::Str("dark".into())))
    );
}

#[test]
fn denied_apply_flips_to_read_only_and_stops_calling_set() {
    let store = store();
    store.seed("sys/ui/mode", Value::Str("dark".into()));
    store.read_only_sys.set(true);
    let mut editor = KeyEditor::default();
    editor.select(Some("sys/ui/mode".into()), &store);
    editor.set_text("light".into());
    assert!(editor.apply(&store).is_err());
    assert!(editor.read_only);
    let writes = store.writes.get();
    // A second apply must not reach the store at all.
    assert!(editor.apply(&store).is_err());
    assert_eq!(store.writes.get(), writes);
}

#[test]
fn denied_get_marks_read_only() {
    let store = store();
    *store.fail_reads.borrow_mut() = Some(StoreError::Denied);
    let mut editor = KeyEditor::default();
    editor.select(Some("user/7/x".into()), &store);
    assert!(editor.read_only);
    assert!(editor.text().is_empty());
    assert!(editor.note().is_some());
}

#[test]
fn kind_change_keeps_the_old_value_on_parse_failure() {
    let store = store();
    store.seed("sys/name", Value::Str("hello".into()));
    let mut editor = KeyEditor::default();
    editor.select(Some("sys/name".into()), &store);
    assert!(editor.set_kind(Kind::U64).is_err());
    // The kind and the stored value are unchanged.
    assert_eq!(editor.kind(), Kind::Str);
    assert_eq!(editor.stored(), Some(&Value::Str("hello".into())));
    assert!(!editor.dirty());
    // A kind the (new) text fits is accepted, and the buffer is now dirty.
    editor.set_text("ab".into());
    assert_eq!(editor.set_kind(Kind::Bytes), Ok(()));
    assert_eq!(editor.kind(), Kind::Bytes);
    assert!(editor.dirty());
}

#[test]
fn revert_restores_the_stored_value() {
    let store = store();
    store.seed("sys/n", Value::U64(7));
    let mut editor = KeyEditor::default();
    editor.select(Some("sys/n".into()), &store);
    editor.set_text("9".into());
    editor.revert();
    assert_eq!(editor.text(), "7");
    assert!(!editor.dirty());
}

#[test]
fn the_buffer_is_tied_to_the_path_it_was_loaded_for() {
    let store = store();
    store.seed("sys/a", Value::U64(1));
    store.seed("sys/b", Value::U64(2));
    let mut editor = KeyEditor::default();
    editor.select(Some("sys/a".into()), &store);
    editor.set_text("9".into());
    editor.select(Some("sys/b".into()), &store);
    assert_eq!(editor.path(), Some("sys/b"));
    assert_eq!(editor.text(), "2");
    editor.set_text("5".into());
    editor.apply(&store).unwrap();
    assert_eq!(store.get("sys/a"), Ok(Some(Value::U64(1))));
    assert_eq!(store.get("sys/b"), Ok(Some(Value::U64(5))));
}

#[test]
fn reselecting_the_same_path_keeps_edits() {
    let store = store();
    store.seed("sys/a", Value::U64(1));
    let mut editor = KeyEditor::default();
    editor.select(Some("sys/a".into()), &store);
    editor.set_text("9".into());
    editor.select(Some("sys/a".into()), &store);
    assert_eq!(editor.text(), "9");
}

#[test]
fn a_key_deleted_elsewhere_clears_the_buffer_and_says_so() {
    let store = store();
    store.seed("sys/a", Value::Bool(true));
    let mut editor = KeyEditor::default();
    editor.select(Some("sys/a".into()), &store);
    store.delete("sys/a").unwrap();
    editor.refresh(&store);
    assert!(editor.text().is_empty());
    assert_eq!(editor.stored(), None);
    assert!(editor.note().unwrap().contains("no longer exists"));
    assert!(!editor.external_changed);
}

#[test]
fn an_external_change_does_not_clobber_a_dirty_buffer() {
    let store = store();
    store.seed("sys/a", Value::Str("a".into()));
    let mut editor = KeyEditor::default();
    editor.select(Some("sys/a".into()), &store);
    editor.set_text("b".into());
    store.seed("sys/a", Value::Str("c".into()));
    editor.refresh(&store);
    assert!(editor.external_changed);
    assert_eq!(editor.text(), "b");
    assert!(editor.dirty());
    // Reload is the deliberate way to take the external value.
    editor.reload(&store);
    assert_eq!(editor.text(), "c");
    assert!(!editor.external_changed);
    assert!(!editor.dirty());
}

#[test]
fn an_external_change_reloads_a_clean_buffer() {
    let store = store();
    store.seed("sys/a", Value::Str("a".into()));
    let mut editor = KeyEditor::default();
    editor.select(Some("sys/a".into()), &store);
    store.seed("sys/a", Value::Str("c".into()));
    editor.refresh(&store);
    assert_eq!(editor.text(), "c");
    assert!(!editor.external_changed);
}

#[test]
fn a_refresh_with_no_external_change_keeps_a_dirty_buffer() {
    let store = store();
    store.seed("sys/a", Value::Str("a".into()));
    let mut editor = KeyEditor::default();
    editor.select(Some("sys/a".into()), &store);
    editor.set_text("b".into());
    // The store still holds "a"; a refresh must not discard the edit.
    editor.refresh(&store);
    assert_eq!(editor.text(), "b");
    assert!(editor.dirty());
    assert!(!editor.external_changed);
}

#[test]
fn delete_confirmation_can_be_cancelled() {
    let store = store();
    store.seed("sys/a", Value::Bool(true));
    let mut editor = KeyEditor::default();
    editor.select(Some("sys/a".into()), &store);
    assert!(!editor.arm_delete());
    assert!(editor.confirm_delete);
    // Another action cancels the pending confirm.
    editor.clear_confirm();
    assert!(!editor.confirm_delete);
    assert!(!editor.arm_delete());
    // Arming twice in a row proceeds.
    assert!(editor.arm_delete());
    assert_eq!(editor.delete(&store), Ok("deleted sys/a".into()));
    assert_eq!(store.get("sys/a"), Ok(None));
}

#[test]
fn new_key_rejects_invalid_and_too_long_paths() {
    let store = store();
    let mut new_key = new_key("bad path", Kind::Str, "");
    assert!(matches!(new_key.create(&store), CreateOutcome::Failed(_)));
    new_key.path = format!("sys/{}", "a".repeat(300));
    assert!(matches!(new_key.create(&store), CreateOutcome::Failed(_)));
}

#[test]
fn new_key_needs_a_second_click_to_overwrite() {
    let store = store();
    store.seed("sys/a", Value::Bool(true));
    let mut new_key = new_key("sys/a", Kind::Bool, "false");
    assert_eq!(new_key.create(&store), CreateOutcome::NeedsConfirm);
    assert!(new_key.confirm_clobber);
    assert_eq!(new_key.create(&store), CreateOutcome::Created);
    assert_eq!(store.get("sys/a"), Ok(Some(Value::Bool(false))));
    // A successful create clears the fields.
    assert!(new_key.path.is_empty());
    assert!(new_key.text.is_empty());
}

#[test]
fn new_key_rejects_a_too_large_value() {
    let store = store();
    let big = "a".repeat(confd::MAX_VALUE_LEN + 1);
    let mut new_key = new_key("sys/big", Kind::Str, &big);
    assert!(matches!(new_key.create(&store), CreateOutcome::Failed(_)));
    assert!(store.is_empty());
}

#[test]
fn new_key_creates_a_fresh_value() {
    let store = store();
    let mut new_key = new_key("sys/net/eth0/mtu", Kind::U64, "0x5dc");
    assert_eq!(new_key.create(&store), CreateOutcome::Created);
    assert_eq!(store.get("sys/net/eth0/mtu"), Ok(Some(Value::U64(1500))));
}

#[test]
fn dirty_tracks_kind_change() {
    let store = store();
    store.seed("sys/a", Value::Str("1".into()));
    let mut editor = KeyEditor::default();
    editor.select(Some("sys/a".into()), &store);
    assert!(!editor.dirty());
    editor.set_kind(Kind::U64).unwrap();
    assert!(editor.dirty());
    editor.apply(&store).unwrap();
    assert!(!editor.dirty());
    assert_eq!(store.get("sys/a"), Ok(Some(Value::U64(1))));
}
