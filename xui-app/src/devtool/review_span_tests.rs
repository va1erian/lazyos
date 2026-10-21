//! The gate judges the arrived text, not the whole buffer.

use super::super::gate::MAX_ARRIVALS;
use super::*;

const MINE: &str = "let mine = 1;\nlet more = 2;\nlet and_more = 3;\nprint(mine);\n";
const PASTED: &str = "sys::init::shutdown();\n";

/// Paste `pasted` at the end of `before`, into a gate that holds `before`.
fn paste_after(before: &str, pasted: &str) -> (ReviewGate, String) {
    let mut gate = ReviewGate::new(Provenance::Typed, before);
    let after = format!("{before}{pasted}");
    gate.arrive_edit(Provenance::Clipboard, before, &after);
    (gate, after)
}

#[test]
fn a_paste_after_typed_text_is_foreign_and_deleting_the_typed_text_keeps_asking() {
    // The bypass: baseline was the whole buffer, so deleting the longer typed
    // text "replaced most of it" and cleared a gate whose pasted script then
    // ran with no card.
    let (mut gate, _) = paste_after(MINE, PASTED);
    assert!(gate.needs_confirmation());
    assert_eq!(gate.provenance(), Provenance::Clipboard);
    gate.edited(PASTED);
    assert!(gate.needs_confirmation(), "only the pasted script is left");
    assert_eq!(gate.provenance(), Provenance::Clipboard);
}

#[test]
fn deleting_the_typed_text_one_piece_at_a_time_keeps_asking() {
    let (mut gate, mut text) = paste_after(MINE, PASTED);
    // Backspace through the typed text, a character at a time from its end.
    while text.len() > PASTED.len() {
        let cut = text.len() - PASTED.len() - 1;
        text.remove(cut);
        gate.edited(&text);
        assert!(gate.needs_confirmation(), "{text:?}");
    }
    assert_eq!(text, PASTED);
}

#[test]
fn a_paste_before_typed_text_is_just_as_foreign() {
    let mut gate = ReviewGate::new(Provenance::Typed, MINE);
    let after = format!("{PASTED}{MINE}");
    gate.arrive_edit(Provenance::Clipboard, MINE, &after);
    assert!(gate.needs_confirmation());
    gate.edited(PASTED);
    assert!(gate.needs_confirmation());
}

#[test]
fn typing_around_a_pasted_block_keeps_asking() {
    let (mut gate, text) = paste_after(MINE, PASTED);
    let around = format!("// before\n{text}print(\"after\");\n");
    gate.edited(&around);
    assert!(gate.needs_confirmation());
    // Typing in the user's own part, before the block.
    let moved = around.replacen("mine", "yours", 1);
    gate.edited(&moved);
    assert!(gate.needs_confirmation());
    assert_eq!(gate.provenance(), Provenance::Clipboard);
}

#[test]
fn a_repeating_typed_prefix_cannot_slide_the_span_away() {
    // "aaa" then "aab": diffing "aaaaab" to "aab" is ambiguous (which a was
    // removed), and a sliding diff once judged the wrong bytes.
    let (mut gate, _) = paste_after("aaa", "aab");
    gate.edited("aab");
    assert!(gate.needs_confirmation());
}

#[test]
fn rewriting_most_of_the_pasted_block_makes_it_the_users() {
    let (mut gate, text) = paste_after(MINE, PASTED);
    let rewritten = text.replace(PASTED, "print(\"my own, quite different script\");\n");
    gate.edited(&rewritten);
    assert!(!gate.needs_confirmation());
    assert_eq!(gate.provenance(), Provenance::Typed);
    // And it stays the user's.
    gate.edited(&text);
    assert!(!gate.needs_confirmation());
}

#[test]
fn a_small_edit_inside_the_pasted_block_keeps_asking() {
    let (mut gate, text) = paste_after(MINE, PASTED);
    gate.edited(&text.replace("shutdown", "shutdawn"));
    assert!(gate.needs_confirmation());
}

#[test]
fn a_second_paste_is_a_second_arrival_and_the_first_still_asks() {
    // Paste A, then B after it, then delete B: A was never asked about.
    let (mut gate, with_a) = paste_after(MINE, PASTED);
    let with_b = format!("{with_a}print(1);\n");
    gate.arrive_edit(Provenance::Clipboard, &with_a, &with_b);
    assert!(gate.needs_confirmation());
    gate.edited(&with_a);
    assert!(gate.needs_confirmation(), "A is still there");
    gate.edited(PASTED);
    assert!(gate.needs_confirmation());
}

#[test]
fn a_paste_into_a_confirmed_buffer_asks_about_the_paste_alone() {
    let (mut gate, text) = paste_after(MINE, PASTED);
    gate.confirm();
    assert!(!gate.needs_confirmation());
    let more = format!("{text}print(2);\n");
    gate.arrive_edit(Provenance::Clipboard, &text, &more);
    assert!(gate.needs_confirmation());
    // Delete everything but the new paste: still asks.
    gate.edited("print(2);\n");
    assert!(gate.needs_confirmation());
}

#[test]
fn a_paste_over_a_selection_makes_the_whole_replacement_the_span() {
    let mut gate = ReviewGate::new(Provenance::Typed, MINE);
    gate.arrive_edit(Provenance::Clipboard, MINE, PASTED);
    assert!(gate.needs_confirmation());
    gate.edited("");
    assert!(!gate.needs_confirmation(), "emptying the buffer clears it");
}

#[test]
fn an_arrival_in_the_middle_of_multibyte_text_is_cut_on_characters() {
    let before = "let s = \"héllo wörld\";\n";
    let mut gate = ReviewGate::new(Provenance::Typed, before);
    let after = before.replacen('é', "é; evil(); é", 1);
    gate.arrive_edit(Provenance::Clipboard, before, &after);
    assert!(gate.needs_confirmation());
    // Delete the user's own text in front of the arrival (the arrival is
    // `; evil(); é`, the second é included, so the cut is on characters).
    gate.edited(&after.replace("let s = \"h", ""));
    assert!(gate.needs_confirmation());
    // Deleting the arrival itself leaves nothing foreign.
    gate.edited(&after.replace("; evil(); é", ""));
    assert!(!gate.needs_confirmation());
}

#[test]
fn a_soak_of_pastes_and_deletes_never_loses_an_unconfirmed_arrival() {
    // Deterministic pseudo-random edits around a pasted block: as long as the
    // block is whole in the buffer, the gate asks.
    let (mut gate, mut text) = paste_after(MINE, PASTED);
    let mut state = 0x2545_f491_4f6c_dd1du64;
    for _ in 0..5_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let Some(block) = text.find(PASTED) else {
            panic!("the block was lost: {text:?}");
        };
        // An edit that stays out of the block: before it, or after it.
        let (lo, hi) = if state & 1 == 0 {
            (0, block)
        } else {
            (block + PASTED.len(), text.len())
        };
        let pos = lo + (state >> 8) as usize % (hi - lo + 1);
        let pos = (0..=pos).rev().find(|p| text.is_char_boundary(*p)).unwrap();
        if state & 2 == 0 && pos < hi {
            text.remove(pos);
        } else {
            text.insert(pos, (b'a' + (state >> 20) as u8 % 26) as char);
        }
        gate.edited(&text);
        assert!(gate.needs_confirmation(), "lost after {text:?}");
    }
}

/// A comment header of the kind a hostile script is padded with.
const HEADER: &str = "// Helper (reviewed upstream), keep. \n";

#[test]
fn deleting_the_padding_around_one_dangerous_line_keeps_asking() {
    // Six headers, the dangerous line, one more header: delete the tail, then
    // the head. What is left at the two ends is nothing and the middle is
    // the whole script's point; a half-at-the-ends rule cleared here.
    let pasted = format!("{}{PASTED}{HEADER}", HEADER.repeat(6));
    let mut gate = ReviewGate::new(Provenance::Typed, "");
    gate.arrive_edit(Provenance::Clipboard, "", &pasted);
    assert!(gate.needs_confirmation());
    let mut text = pasted.clone();
    // Backspace the tail, a character at a time.
    while text.len() > pasted.len() - HEADER.len() {
        text.pop();
        gate.edited(&text);
        assert!(gate.needs_confirmation(), "{text:?}");
    }
    // Delete the head, a character at a time from the front.
    while !text.ends_with(PASTED) || text.len() > PASTED.len() {
        text.remove(0);
        gate.edited(&text);
        assert!(gate.needs_confirmation(), "{text:?}");
    }
    assert_eq!(text, PASTED);
    // Or both cuts at once.
    let mut gate = ReviewGate::new(Provenance::Typed, "");
    gate.arrive_edit(Provenance::Clipboard, "", &pasted);
    gate.edited(PASTED);
    assert!(gate.needs_confirmation());
}

#[test]
fn a_second_paste_does_not_swallow_the_text_typed_between() {
    // Paste S1, type a hundred bytes, paste S1 again, delete the typed middle:
    // both copies are still there.
    let s1 = "sys::init::shutdown();\nsys::init::reboot();\n";
    let mut gate = ReviewGate::new(Provenance::Typed, "");
    gate.arrive_edit(Provenance::Clipboard, "", s1);
    let mut text = s1.to_owned();
    for _ in 0..13 {
        let next = format!("{text}let typed = 12;\n");
        gate.edited(&next);
        text = next;
    }
    assert!(text.len() > s1.len() + 100);
    let second = format!("{text}{s1}");
    gate.arrive_edit(Provenance::Clipboard, &text, &second);
    assert!(gate.needs_confirmation());
    // Delete the typed middle at once: both copies remain, and it asks.
    gate.edited(&format!("{s1}{s1}"));
    assert!(gate.needs_confirmation());
    // Delete one copy: the other still asks. Delete that one too: it is clear.
    gate.edited(s1);
    assert!(gate.needs_confirmation());
    gate.edited("");
    assert!(!gate.needs_confirmation());
}

#[test]
fn each_arrival_is_judged_on_its_own_bytes() {
    let first = "alpha_alpha_alpha_alpha();\n";
    let second = "omega_omega_omega_omega();\n";
    let mut gate = ReviewGate::new(Provenance::Typed, "");
    gate.arrive_edit(Provenance::Clipboard, "", first);
    let both = format!("{first}// mine\n{second}");
    gate.arrive_edit(Provenance::Clipboard, first, &both);
    // The first goes: the second alone keeps asking, and only then does it clear.
    gate.edited(&both.replace(first, ""));
    assert!(gate.needs_confirmation());
    gate.edited("// mine\n");
    assert!(!gate.needs_confirmation());
}

#[test]
fn typing_inside_the_pasted_block_keeps_asking_until_no_run_is_left() {
    let line = "sys::init::shutdown_the_machine_now();\n";
    let mut gate = ReviewGate::new(Provenance::Typed, "");
    gate.arrive_edit(Provenance::Clipboard, "", line);
    // Type characters into the middle: a piece of at least 12 bytes stays
    // whole on one side the whole time.
    let mut text = line.to_owned();
    let middle = line.len() / 2;
    for (i, c) in "// not what it looks like".chars().enumerate() {
        text.insert(middle + i, c);
        gate.edited(&text);
        assert!(gate.needs_confirmation(), "{text:?}");
    }
    // A typed character every 10 bytes breaks it into pieces of 10: no run
    // of the arrival is left, so it is the user's.
    let mut chopped = String::new();
    for (i, c) in line.chars().enumerate() {
        if i > 0 && i % 10 == 0 {
            chopped.push('#');
        }
        chopped.push(c);
    }
    gate.edited(&chopped);
    assert!(!gate.needs_confirmation());
}

#[test]
fn a_flood_of_pastes_is_merged_not_forgotten() {
    // More arrivals than the gate tracks one by one: the later ones are
    // merged into the last, and every byte of them still asks.
    let mut gate = ReviewGate::new(Provenance::Typed, "");
    let mut text = String::new();
    for i in 0..(MAX_ARRIVALS + 20) {
        let next = format!("{text}piece_number_{i:04}();\n");
        gate.arrive_edit(Provenance::Clipboard, &text, &next);
        text = next;
    }
    assert!(gate.needs_confirmation());
    let last = format!("piece_number_{:04}();\n", MAX_ARRIVALS + 19);
    gate.edited(&last);
    assert!(
        gate.needs_confirmation(),
        "a merged arrival is still followed"
    );
    gate.edited("");
    assert!(!gate.needs_confirmation());
}

#[test]
fn an_edit_that_brings_in_nothing_foreign_changes_nothing() {
    let (mut gate, text) = paste_after(MINE, PASTED);
    // The "paste" took text out and put none foreign in (a shorter selection).
    gate.arrive_edit(Provenance::Clipboard, &text, MINE);
    assert!(!gate.needs_confirmation(), "the paste was deleted");
    let mut gate = ReviewGate::new(Provenance::Typed, MINE);
    gate.arrive_edit(Provenance::Typed, MINE, &format!("{MINE}{PASTED}"));
    assert!(!gate.needs_confirmation(), "typed text is not asked about");
}
