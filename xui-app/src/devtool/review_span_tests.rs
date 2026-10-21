//! The gate judges the arrived span, not the whole buffer.

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
fn a_second_paste_widens_the_span_over_an_unconfirmed_first() {
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
    gate.edited(&after.replace("héllo wörld", ""));
    assert!(gate.needs_confirmation());
}

#[test]
fn following_a_span_through_edits() {
    // before, after, inside, and overlapping, by position.
    assert_eq!(follow((10, 20), 0, 0, 5), (15, 25), "inserted before");
    assert_eq!(follow((10, 20), 0, 4, 0), (6, 16), "deleted before");
    assert_eq!(
        follow((10, 20), 6, 4, 0),
        (6, 16),
        "deleted up to the start"
    );
    assert_eq!(follow((10, 20), 20, 0, 5), (10, 20), "inserted at the end");
    assert_eq!(follow((10, 20), 30, 3, 1), (10, 20), "after");
    assert_eq!(follow((10, 20), 12, 2, 5), (10, 23), "inside");
    assert_eq!(follow((10, 20), 5, 10, 0), (5, 10), "overlapping the start");
    assert_eq!(follow((10, 20), 15, 10, 1), (10, 16), "overlapping the end");
    assert_eq!(follow((10, 20), 0, 40, 0), (0, 0), "swallowed");
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
