//! A throwaway debug probe of the PPU state after the sample ROM runs.

use std::collections::HashMap;

use tetanes_core::prelude::*;

fn main() {
    let path = std::env::args().nth(1).expect("a ROM path");
    let mut deck = ControlDeck::new();
    deck.load_rom_path(&path).expect("loads");
    for _ in 0..60 {
        while deck.clock_frame().expect("runs") == Clocked::Continue {}
    }
    let raw = deck.frame_buffer_raw();
    let mut values: HashMap<u16, usize> = HashMap::new();
    for value in raw.iter() {
        *values.entry(*value).or_default() += 1;
    }
    let mut list: Vec<_> = values.into_iter().collect();
    list.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    println!("raw pixels (top 8): {list:?}");
    println!("raw row 8: {:?}", &raw[8 * 256..8 * 256 + 16]);
    println!("raw row 40: {:?}", &raw[40 * 256..40 * 256 + 16]);
    let bus = deck.bus();
    println!(
        "ctrl_bg_select={:#x} ctrl_spr_select={:#x} rendering={}",
        bus.ppu.ctrl_bg_select, bus.ppu.ctrl_spr_select, bus.ppu.mask_rendering_enabled
    );
    let nt: Vec<u8> = (0..16).map(|i| bus.chr_peek(0x2000 + i)).collect();
    println!("nt[0..16]: {nt:?}");
    let row4: Vec<u8> = (0..24).map(|i| bus.chr_peek(0x2000 + 4 * 32 + i)).collect();
    println!("nt row 4: {row4:?}");
    let chr: Vec<u8> = (0..16).map(|i| bus.chr_peek(0x0200 + i)).collect();
    println!("chr@0x0200: {chr:?}");
    let chr2: Vec<u8> = (0..16).map(|i| bus.chr_peek(0x1200 + i)).collect();
    println!("chr@0x1200: {chr2:?}");
}
