//! The label table and the credential-gate rules around it.

use super::*;
use crate::ipc::credentials::{LabelStamp, TransitionError, CAP_SETUID};
use crate::process::cred_op;

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const EACCES: i64 = 13;
const EINVAL: i64 = 22;

/// Equal strings share an id, distinct strings do not, ids round-trip, and
/// anything outside the two label forms or the `[a-z0-9.:-]` charset is
/// refused without touching the table.
pub fn intern_dedup_and_charset() -> Result<(), String> {
    fresh()?;
    let notes = labels::intern("app:com.example.notes").map_err(|_| "intern failed")?;
    let again = labels::intern("app:com.example.notes").map_err(|_| "re-intern failed")?;
    let net = labels::intern("system:netdrv").map_err(|_| "intern system failed")?;
    check!(notes != 0 && net != 0, "id 0 is reserved for unlabelled");
    check!(notes == again, "an equal string got a second id");
    check!(notes != net, "distinct labels share an id");
    check!(labels::count() == 2, "count is {}", labels::count());
    check!(
        labels::name_of(notes).as_deref() == Some("app:com.example.notes"),
        "the id does not round-trip"
    );
    check!(labels::name_of(0).is_none(), "id 0 names a string");
    check!(
        labels::name_of(99).is_none(),
        "an unknown id names a string"
    );
    check!(
        labels::kind_of(notes) == Some(labels::Kind::App)
            && labels::kind_of(net) == Some(labels::Kind::System),
        "kinds are wrong"
    );
    check!(
        labels::lookup("app:com.example.notes") == Some(notes),
        "lookup"
    );
    check!(
        labels::lookup("app:com.example.other").is_none(),
        "lookup adds"
    );

    let too_long = format!("app:{}", "a".repeat(labels::MAX_LABEL_BYTES - 3));
    let longest = format!("app:{}", "a".repeat(labels::MAX_LABEL_BYTES - 4));
    let bad = [
        "",
        "app:",
        "system:",
        "App:com.example",
        "app:Com.Example",
        "app:com example",
        "app:com_example",
        "app:com/example",
        "app:com.example:x",
        "app:.com",
        "app:com.",
        "app:-com",
        "app:com..example",
        "user:alice",
        "appcom.example",
        "app:\u{e9}",
        &too_long,
    ];
    for label in bad {
        check!(
            labels::intern(label) == Err(labels::Error::Malformed),
            "{label:?} was accepted"
        );
    }
    check!(
        labels::intern(&longest).is_ok(),
        "a label of exactly {} bytes was refused",
        labels::MAX_LABEL_BYTES
    );
    check!(labels::count() == 3, "a bad label changed the table");
    Ok(())
}

/// The table is bounded: a full table refuses new labels, keeps serving the
/// ones it has, and never evicts.
pub fn intern_capacity() -> Result<(), String> {
    fresh()?;
    let mut first = 0;
    for index in 0..labels::CAPACITY {
        let id = labels::intern(&format!("app:cap.fill{index}")).map_err(|_| "fill failed")?;
        if index == 0 {
            first = id;
        }
    }
    check!(labels::count() == labels::CAPACITY, "table not full");
    check!(
        labels::intern("app:cap.overflow") == Err(labels::Error::TableFull),
        "a full table accepted a new label"
    );
    check!(
        labels::intern("app:cap.fill0") == Ok(first),
        "an existing label stopped resolving when the table filled"
    );
    check!(
        labels::count() == labels::CAPACITY,
        "a refused intern changed the count"
    );
    fresh()?;
    Ok(())
}

/// A label goes from 0 to a value once, on a child being created; every other
/// stamp must keep it, and it needs `CAP_SETUID` like any stamp.
pub fn gate_assigns_once() -> Result<(), String> {
    fresh()?;
    let app = labels::intern("app:com.gate.a").map_err(|_| "intern")?;
    let other = labels::intern("app:com.gate.b").map_err(|_| "intern")?;
    let child = task::spawn_child("gate", &service_suite::minimal_elf()).map_err(to_string)?;
    let actor = task::current();
    let labelled = Cred::new(0, 0, CAP_SETUID, app, 0);

    // An unlabelled CAP_SETUID holder may assign a label to a new child.
    credentials::set(actor, Cred::new(0, 0, CAP_SETUID, 0, 0));
    check!(
        credentials::transition_with(actor, child, labelled, LabelStamp::Assign) == Ok(labelled),
        "the labelled spawn stamp was refused"
    );
    check!(credentials::of(child).label_id == app, "label not applied");

    // A plain stamp (SET) cannot change or strip it, on the child or itself.
    let strip = Cred::new(0, 0, CAP_SETUID, 0, 0);
    check!(
        credentials::transition(actor, child, strip) == Err(TransitionError::LabelLocked),
        "SET stripped a label"
    );
    let swap = Cred::new(0, 0, CAP_SETUID, other, 0);
    check!(
        credentials::transition(actor, child, swap) == Err(TransitionError::LabelLocked),
        "SET swapped a label"
    );
    check!(
        credentials::transition(actor, actor, swap) == Err(TransitionError::LabelLocked),
        "a task labelled itself"
    );
    check!(
        credentials::of(child).label_id == app && credentials::of(actor).label_id == 0,
        "a refused stamp changed a label"
    );
    // Re-stamping with the same label is an ordinary stamp and is fine.
    check!(
        credentials::transition(actor, child, labelled).is_ok(),
        "a stamp that keeps the label was refused"
    );

    // A labelled creator cannot assign a different label; its own is fine.
    credentials::set(actor, labelled);
    let grandchild =
        task::spawn_child("gate2", &service_suite::minimal_elf()).map_err(to_string)?;
    check!(
        credentials::transition_with(actor, grandchild, swap, LabelStamp::Assign)
            == Err(TransitionError::LabelLocked),
        "a labelled task assigned another label"
    );
    check!(
        credentials::transition_with(actor, grandchild, labelled, LabelStamp::Assign).is_ok(),
        "a labelled task could not pass its own label on"
    );
    // Assigning "no label" is not an assignment.
    let none = Cred::new(0, 0, CAP_SETUID, 0, 0);
    credentials::set(actor, Cred::new(0, 0, CAP_SETUID, 0, 0));
    check!(
        credentials::transition_with(actor, grandchild, none, LabelStamp::Assign)
            == Err(TransitionError::LabelLocked),
        "Assign accepted label 0"
    );

    // Without CAP_SETUID nothing is assigned, and the refusal is the usual one.
    credentials::set(actor, Cred::new(1000, 1000, 0, 0, 0));
    check!(
        credentials::transition_with(actor, child, labelled, LabelStamp::Assign)
            == Err(TransitionError::NotPrivileged),
        "an unprivileged task assigned a label"
    );
    Ok(())
}

/// A child inherits its creator's label (so helpers stay in the sandbox), and a
/// labelled spawn naming a different label replaces the inherited one only
/// when the creator is unlabelled.
pub fn not_inherited_across_labelled_spawn() -> Result<(), String> {
    fresh()?;
    let a = labels::intern("app:com.inherit.a").map_err(|_| "intern")?;
    let b = labels::intern("app:com.inherit.b").map_err(|_| "intern")?;
    let parent = labelled_task("app:com.inherit.a", 0, CAP_SETUID)?;
    let plain = task::spawn_child("plain", &service_suite::minimal_elf()).map_err(to_string)?;
    credentials::inherit(parent, plain);
    check!(
        credentials::of(plain).label_id == a,
        "a plain child did not inherit the label"
    );

    // The labelled parent cannot spawn a child labelled b.
    let child = task::spawn_child("kid", &service_suite::minimal_elf()).map_err(to_string)?;
    credentials::inherit(parent, child);
    let request = Cred::new(0, 0, 0, b, 0);
    check!(
        credentials::transition_with(parent, child, request, LabelStamp::Assign)
            == Err(TransitionError::LabelLocked),
        "a labelled parent relabelled its child"
    );
    check!(credentials::of(child).label_id == a, "child label changed");

    // An unlabelled creator's labelled spawn gives b, not the creator's 0.
    let init = labelled_task("", 0, CAP_SETUID)?;
    let child2 = task::spawn_child("kid2", &service_suite::minimal_elf()).map_err(to_string)?;
    credentials::inherit(parent, child2); // pretend the slot inherited a
    check!(credentials::of(child2).label_id == a, "setup");
    let stamped = credentials::transition_with(init, child2, request, LabelStamp::Assign);
    check!(
        stamped.is_ok(),
        "an unlabelled creator was refused: {stamped:?}"
    );
    check!(
        credentials::of(child2).label_id == b,
        "the child kept the inherited label {}",
        credentials::of(child2).label_id
    );
    Ok(())
}

/// The program the labelled spawns name: missing, so a spawn that clears the
/// credential checks fails only with `-ENOENT`.
const NOSUCH: &[u8] = b"/system/bin/nosuch";
/// Where its `argv` block (`[NOSUCH]`) sits in the scratch space.
const ARGV: u64 = CMDLINE + 0x80;

/// Write an `AsLabelled` `spawnv` request (path, `argv`, credential words,
/// label pointer/length) and the label bytes into the scratch space.
fn write_block(uid: u32, caps: u32, label: &str) {
    use crate::process::spawnv::{cred_mode, personality, REQ_WORDS};
    let mut words = [0u64; REQ_WORDS];
    words[..5].copy_from_slice(&[
        CMDLINE,
        NOSUCH.len() as u64,
        ARGV,
        NOSUCH.len() as u64 + 1,
        1,
    ]);
    words[8] = personality::NATIVE;
    words[9] = cred_mode::AS_LABELLED;
    words[10..15].copy_from_slice(&[uid as u64, uid as u64, caps as u64, 0, 0]);
    words[15..].copy_from_slice(&[LABEL_BYTES, label.len() as u64]);
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    write_bytes(BLOCK, &bytes);
    write_bytes(LABEL_BYTES, label.as_bytes());
    write_bytes(CMDLINE, NOSUCH);
    let mut argv = NOSUCH.to_vec();
    argv.push(0);
    write_bytes(ARGV, &argv);
}

fn gate(op: u64, a1: u64, a2: u64) -> u64 {
    process::dispatch_for_test(10, op, a1, a2)
}

/// `spawnv` (syscall 30) of the request [`write_block`] wrote.
fn spawn_labelled() -> u64 {
    process::dispatch_for_test(30, BLOCK, 0, 0)
}

/// The native gate: labelled spawn (`spawnv`), label-name reads and
/// `cred_get`.
pub fn gate_syscalls() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let me = task::current();

        // The retired command-line spawn ops (2 and 3) are unknown ops now.
        write_block(1000, 0, "app:com.gate.retired");
        for op in [2, 3] {
            let code = gate(op, CMDLINE, BLOCK);
            check!(code == failed(EINVAL), "retired op {op} -> {code:#x}");
        }
        check!(labels::count() == 0, "a retired op interned a label");

        // A malformed label is `-EINVAL`, before any task exists.
        write_block(1000, 0, "Bad Label");
        let code = spawn_labelled();
        check!(code == failed(EINVAL), "bad label -> {code:#x}");
        check!(labels::count() == 0, "a malformed label was interned");

        // A well-formed one clears the gate and fails only on the missing
        // file, proving the stamp was approved (and the label interned).
        write_block(1000, 0, "app:com.gate.ok");
        let code = spawn_labelled();
        check!(code == failed(ENOENT), "valid labelled spawn -> {code:#x}");
        let ok = labels::lookup("app:com.gate.ok").ok_or("label not interned")?;

        // Widening is refused even though the label is fine.
        credentials::set(me, Cred::new(1000, 1000, CAP_SETUID, 0, 0));
        write_block(1000, credentials::CAP_ALL, "app:com.gate.wide");
        let code = spawn_labelled();
        check!(code == failed(EACCES), "widening -> {code:#x}");

        // Without CAP_SETUID: `-EPERM`, and the label table is not probed.
        credentials::set(me, Cred::new(1000, 1000, 0, 0, 0));
        write_block(1000, 0, "app:com.gate.unpriv");
        let code = spawn_labelled();
        check!(code == failed(EPERM), "unprivileged -> {code:#x}");
        check!(
            labels::lookup("app:com.gate.unpriv").is_none(),
            "an unprivileged caller interned a label"
        );

        // A labelled caller may keep its own label but not pick another.
        credentials::set(me, Cred::new(1000, 1000, CAP_SETUID, ok, 0));
        write_block(1000, 0, "app:com.gate.ok");
        let code = spawn_labelled();
        check!(code == failed(ENOENT), "same label -> {code:#x}");
        write_block(1000, 0, "app:com.gate.other");
        let code = spawn_labelled();
        check!(code == failed(EPERM), "different label -> {code:#x}");

        // The plain SET cannot label the caller either.
        credentials::set(me, Cred::new(1000, 1000, CAP_SETUID, 0, 0));
        write_bytes(
            BLOCK,
            &Cred::new(1000, 1000, 0, ok, 0)
                .to_words()
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<u8>>(),
        );
        let code = gate(cred_op::SET, u64::MAX, BLOCK);
        check!(code == failed(EPERM), "SET with a label -> {code:#x}");
        check!(credentials::of(me).label_id == 0, "SET labelled the caller");

        // LABEL_NAME: a CAP_SETUID holder reads any label.
        let code = gate(cred_op::LABEL_NAME, ok as u64, LABEL_OUT);
        check!(code == 0, "label name -> {code:#x}");
        let block = read_bytes(LABEL_OUT, 8 + labels::MAX_LABEL_BYTES);
        let len = u64::from_le_bytes(block[..8].try_into().map_err(|_| "len")?) as usize;
        check!(
            &block[8..8 + len] == b"app:com.gate.ok",
            "label name is {:?}",
            &block[8..8 + len]
        );
        check!(
            gate(cred_op::LABEL_NAME, 0, LABEL_OUT) == failed(ENOENT),
            "label 0 has a name"
        );
        check!(
            gate(cred_op::LABEL_NAME, 4000, LABEL_OUT) == failed(ENOENT),
            "an unknown label has a name"
        );
        // Without the cap only the task's own label is readable.
        credentials::set(me, Cred::new(1000, 1000, 0, ok, 0));
        check!(
            gate(cred_op::LABEL_NAME, ok as u64, LABEL_OUT) == 0,
            "a task could not read its own label"
        );
        let stranger = labels::intern("app:com.gate.stranger").map_err(|_| "intern")?;
        check!(
            gate(cred_op::LABEL_NAME, stranger as u64, LABEL_OUT) == failed(EPERM),
            "a task read another app's label"
        );

        // `cred_get` returns the label id to a CAP_SETUID holder.
        let child = labelled_task("app:com.gate.ok", 1000, 0)?;
        credentials::set(me, Cred::new(0, 0, CAP_SETUID, 0, 0));
        let code = gate(cred_op::GET, child as u64, LABEL_OUT);
        check!(code == 0, "cred_get -> {code:#x}");
        let words = read_bytes(LABEL_OUT, 40);
        let label_word = u64::from_le_bytes(words[24..32].try_into().map_err(|_| "word")?);
        check!(
            label_word == ok as u64,
            "cred_get label word is {label_word}"
        );
        Ok(())
    })
}
