#!/usr/bin/env python3
"""The NES Player's sample ROM: an original, freely licensed homebrew.

`tools/nes/make_sample.py` assembles this into `assets/samples/lazyos-sample.nes`
(shipped at `/system/share/samples/`, the app's welcome screen plays it) and
`xui-app/crates/nes/tests/data/sample.nes` (the crate's tests run it). It is
NROM-256: 32 KiB of PRG, 8 KiB of CHR, mapper 0, battery-backed PRG RAM.

What it does, so a test or a harness can see the emulator working:

* draws a title screen with an 8x8 font drawn here (glyphs below);
* a 16x16 ball the D-pad moves, clamped to the screen;
* a four-note melody (A4, C5, E5, A5) on pulse 1, one note every 30 frames,
  and a short C6 beep when A is pressed;
* every frame it stores the controller byte at `$10` and the frame counter at
  `$12`/`$13`, and mirrors them into battery-backed PRG RAM at `$6000`, so a
  test can read the emulated console's own view of input and time;
* it leaves `OAMADDR` at 0 after its OAM writes: the PPU copies
  `OAM[OAMADDR & 0xF8 ..]` over the first eight OAM bytes on the pre-render
  line when `OAMADDR >= 8`, which would eat the ball's two top sprites every
  frame (the session test asserts the ball's height, so the bug cannot come
  back);
* keeps running forever; it never writes to the PPU outside vblank.
"""

from __future__ import annotations

import nesasm

# The NES CPU clock the APU pulse period is derived from (NTSC).
CPU_HZ = 1_789_772.727
# Timer values for the melody, and the A-button beep (C6).
NOTES = [440.0, 523.251, 659.255, 880.0]
BEEP_HZ = 1046.502
# Frames each note is held (30 at 60.1 Hz is half a second).
NOTE_FRAMES = 30

# The 8x8 font, one string per glyph with the eight rows separated by '/'.
# A '#' is a pixel; drawn here, not copied from another font.
GLYPHS = {
    " ": "......../......../......../......../......../......../......../........",
    "-": "......../......../......../#####.../......../......../......../........",
    "A": ".###..../#...#.../#...#.../#####.../#...#.../#...#.../#...#.../........",
    "B": "####..../#...#.../#...#.../####..../#...#.../#...#.../####..../........",
    "C": ".###..../#...#.../#......./#......./#......./#...#.../.###..../........",
    "D": "####..../#...#.../#...#.../#...#.../#...#.../#...#.../####..../........",
    "E": "#####.../#......./#......./####..../#......./#......./#####.../........",
    "H": "#...#.../#...#.../#...#.../#####.../#...#.../#...#.../#...#.../........",
    "I": ".###..../..#...../..#...../..#...../..#...../..#...../.###..../........",
    "L": "#......./#......./#......./#......./#......./#......./#####.../........",
    "M": "#...#.../##.##.../#.#.#.../#.#.#.../#...#.../#...#.../#...#.../........",
    "N": "#...#.../##..#.../#.#.#.../#.#.#.../#..##.../#...#.../#...#.../........",
    "O": ".###..../#...#.../#...#.../#...#.../#...#.../#...#.../.###..../........",
    "P": "####..../#...#.../#...#.../####..../#......./#......./#......./........",
    "R": "####..../#...#.../#...#.../####..../#.#...../#..#..../#...#.../........",
    "S": ".####.../#......./#......./.###..../....#.../....#.../####..../........",
    "T": "#####.../..#...../..#...../..#...../..#...../..#...../..#...../........",
    "U": "#...#.../#...#.../#...#.../#...#.../#...#.../#...#.../.###..../........",
    "V": "#...#.../#...#.../#...#.../#...#.../#...#.../.#.#..../..#...../........",
    "Y": "#...#.../#...#.../.#.#..../..#...../..#...../..#...../..#...../........",
    "Z": "#####.../....#.../...#..../..#...../.#....../#......./#####.../........",
}

# The 16x16 ball the D-pad moves: two eyes and a smile.
BALL = (
    "....########....",
    "..############..",
    ".##############.",
    "####..####..####",
    "###....##....###",
    "###....##....###",
    "####..####..####",
    "################",
    "##..########..##",
    "##..########..##",
    "###..######..###",
    "####..####..####",
    ".#####....#####.",
    "..############..",
    "....########....",
    "................",
)

# The screen: 32x30 tiles; where each line of text goes (row, column).
TEXT = [
    (4, 11, "LAZYOS NES"),
    (7, 11, "SAMPLE ROM"),
    (12, 10, "D-PAD MOVES"),
    (14, 12, "THE BALL"),
    (19, 13, "A BEEP"),
    (21, 10, "START MUSIC"),
]

# The ball's four tiles are indices 32-35 of the pattern table.
BALL_TILE = 32
# Palette indices from the 2C02's table: dark blue, white, light blue, blue,
# orange, red.
PALETTE = [
    0x01, 0x30, 0x21, 0x11,
    0x01, 0x30, 0x21, 0x11,
    0x01, 0x30, 0x21, 0x11,
    0x01, 0x30, 0x21, 0x11,
    0x01, 0x27, 0x30, 0x16,
    0x01, 0x30, 0x21, 0x11,
    0x01, 0x30, 0x21, 0x11,
    0x01, 0x30, 0x21, 0x11,
]


def rows(glyph: str) -> list[str]:
    """The eight rows of one glyph string."""
    parts = glyph.split("/")
    if len(parts) != 8 or any(len(part) != 8 for part in parts):
        raise ValueError(f"glyph {glyph!r} is not 8 rows of 8")
    return parts


def tile(art: list[str]) -> bytes:
    """One 8x8 tile as 16 CHR bytes: the low plane from `art`, plane 1 zero."""
    low = bytearray()
    for row in art:
        bits = sum(1 << (7 - x) for x, char in enumerate(row) if char != ".")
        low.append(bits)
    return bytes(low) + bytes(8)


def chr_rom() -> bytes:
    """8 KiB of CHR: the font from tile 0 of the background table (`$0000`),
    the ball from tile 32 of the sprite table (`$1000`)."""
    out = bytearray()
    for glyph in GLYPHS.values():
        out.extend(tile(rows(glyph)))
    if len(out) > BALL_TILE * 16:
        raise ValueError("the font outgrew the space before the ball's tiles")
    out.extend(bytes(0x1000 - len(out)))
    out.extend(bytes(BALL_TILE * 16 - (len(out) - 0x1000)))
    for y in range(0, 16, 8):
        for x in range(0, 16, 8):
            out.extend(tile([row[x : x + 8] for row in BALL[y : y + 8]]))
    out.extend(bytes(8192 - len(out)))
    return bytes(out)


def nametable() -> bytes:
    """The 960 nametable bytes plus the 64 attribute bytes, as one block (the
    ROM copies all 1024 to `$2000` in a single loop)."""
    screen = bytearray(b" " * 960)
    index = {char: n for n, char in enumerate(GLYPHS)}
    for row, column, text in TEXT:
        for offset, char in enumerate(text):
            if char not in index:
                raise ValueError(f"no glyph for {char!r} in {text!r}")
            screen[row * 32 + column + offset] = index[char]
    return bytes(screen) + bytes(64)


def timer(hz: float) -> int:
    """The 11-bit pulse period for `hz`."""
    return round(CPU_HZ / (16.0 * hz)) - 1


def byte_lines(name: str, data: bytes, width: int = 16) -> str:
    """`data` as a labelled run of `.byte` lines."""
    lines = [f"{name}:"]
    for at in range(0, len(data), width):
        chunk = data[at : at + width]
        lines.append("        .byte " + ", ".join(f"${byte:02X}" for byte in chunk))
    return "\n".join(lines) + "\n"


SOURCE = """\
; The NES Player's sample ROM (tools/nes/sample_rom.py). NROM-256, mapper 0,
; battery-backed PRG RAM. Written for this repository; GPL-3.0-or-later like
; the rest of the tree (assets/manifest.txt).

joy      = $10          ; controller 1, bit 7 = A ... bit 0 = Right
joy_prev = $11
frame_lo = $12
frame_hi = $13
ptr      = $14          ; the nametable copy's pointer
note_timer = $20
note_index = $21
beep     = $22
ball_x   = $30
ball_y   = $31

        .org $8000

RESET:
        sei
        cld
        ldx #$FF
        txs
        ; clear RAM
        lda #$00
        ldx #$00
clear:
        sta $0000,x
        sta $0100,x
        sta $0200,x
        sta $0300,x
        sta $0400,x
        sta $0500,x
        sta $0600,x
        sta $0700,x
        inx
        bne clear

        ; wait for the first vblank
wait_vblank:
        lda $2002
        bpl wait_vblank

        ; the nametable and attributes: 1024 bytes from $2000, four pages
        ; through a pointer (a single `,x` loop would interleave the pages)
        lda #<nt_data
        sta ptr
        lda #>nt_data
        sta ptr+1
        lda #$20
        sta $2006
        lda #$00
        sta $2006
        ldx #$04
nt_page:
        ldy #$00
nt_copy:
        lda (ptr),y
        sta $2007
        iny
        bne nt_copy
        inc ptr+1
        dex
        bne nt_page

        ; the palettes: 32 bytes from $3F00
        lda #$3F
        sta $2006
        lda #$00
        sta $2006
        ldx #$00
pal_copy:
        lda palettes,x
        sta $2007
        inx
        cpx #32
        bne pal_copy

        ; pulse 1 on, and its first note
        lda #$01
        sta $4015
        lda #$BF
        sta $4000
        lda #NOTES_0_LO
        sta $4002
        lda #NOTES_0_HI
        sta $4003

        ; the ball starts in the middle
        lda #120
        sta ball_x
        lda #104
        sta ball_y

        ; NMI on, background from $0000 (the font), sprites from $1000 (the
        ; ball): $88
        lda #$88
        sta $2000
        ; background and sprites on, no left-column clipping
        lda #$1E
        sta $2001
        lda #$00
        sta $2005
        sta $2005

main:
        jmp main

NMI:
        pha
        txa
        pha
        tya
        pha

        inc frame_lo
        bne nmi_counted
        inc frame_hi
nmi_counted:

        ; read controller 1: A B Select Start Up Down Left Right, bit 7 first
        lda #$00
        sta joy
        lda #$01
        sta $4016
        lda #$00
        sta $4016
        ldx #$08
read_pad:
        lda $4016
        lsr a
        rol joy
        dex
        bne read_pad

        ; A on an edge starts the beep
        lda joy
        and #$80
        beq no_edge
        lda joy_prev
        and #$80
        bne no_edge
        lda #12
        sta beep
no_edge:
        lda joy
        sta joy_prev

        ; the D-pad moves the ball
        lda joy
        and #$01
        beq no_right
        lda ball_x
        cmp #240
        bcs no_right
        inc ball_x
no_right:
        lda joy
        and #$02
        beq no_left
        lda ball_x
        cmp #8
        bcc no_left
        dec ball_x
no_left:
        lda joy
        and #$04
        beq no_down
        lda ball_y
        cmp #200
        bcs no_down
        inc ball_y
no_down:
        lda joy
        and #$08
        beq no_up
        lda ball_y
        cmp #16
        bcc no_up
        dec ball_y
no_up:

        ; the beep, else the melody: one note every NOTE_FRAMES
        lda beep
        beq music
        dec beep
        lda #<BEEP_TIMER
        sta $4002
        lda #>BEEP_TIMER
        sta $4003
        jmp sound_done
music:
        lda note_timer
        cmp #NOTE_FRAMES
        bcc note_tick
        lda #$00
        sta note_timer
        inc note_index
        lda note_index
        and #$03
        sta note_index
        tax
        lda notes_lo,x
        sta $4002
        lda notes_hi,x
        sta $4003
note_tick:
        inc note_timer
sound_done:

        ; OAM: four sprites making the 16x16 ball
        lda #$00
        sta $2003
        lda ball_y
        sta $2004
        lda #BALL_TILE+0
        sta $2004
        lda #$00
        sta $2004
        lda ball_x
        sta $2004

        lda ball_y
        sta $2004
        lda #BALL_TILE+1
        sta $2004
        lda #$00
        sta $2004
        lda ball_x
        clc
        adc #$08
        sta $2004

        lda ball_y
        clc
        adc #$08
        sta $2004
        lda #BALL_TILE+2
        sta $2004
        lda #$00
        sta $2004
        lda ball_x
        sta $2004

        lda ball_y
        clc
        adc #$08
        sta $2004
        lda #BALL_TILE+3
        sta $2004
        lda #$00
        sta $2004
        lda ball_x
        clc
        adc #$08
        sta $2004

        ; OAMADDR back to 0. The PPU copies OAM[OAMADDR & 0xF8 ..] over the
        ; first eight bytes on the pre-render line when OAMADDR >= 8, so a
        ; write loop that leaves it at 16 (four sprites) would eat the two
        ; top sprites every frame (docs/nes-player.md, "The sample cartridge").
        lda #$00
        sta $2003

        ; no scrolling
        lda #$00
        sta $2005
        sta $2005

        ; mirror the state into battery-backed PRG RAM
        lda joy
        sta $6002
        lda frame_lo
        sta $6000
        lda frame_hi
        sta $6001

        pla
        tay
        pla
        tax
        pla
        rti
"""


def source() -> str:
    """The whole assembly source: the program, its data, and the vectors."""
    text = SOURCE
    text = text.replace("NOTE_FRAMES", str(NOTE_FRAMES))
    text = text.replace("BEEP_TIMER", str(timer(BEEP_HZ)))
    text = text.replace("NOTES_0_LO", str(timer(NOTES[0]) & 0xFF))
    text = text.replace("NOTES_0_HI", str(timer(NOTES[0]) >> 8))
    text = text.replace("BALL_TILE", str(BALL_TILE))
    text += "\n" + byte_lines("palettes", bytes(PALETTE))
    text += "\n" + byte_lines("nt_data", nametable(), width=32)
    text += "\n" + byte_lines("notes_lo", bytes(timer(hz) & 0xFF for hz in NOTES), 4)
    text += "\n" + byte_lines("notes_hi", bytes(timer(hz) >> 8 for hz in NOTES), 4)
    text += """
        .org $FFFA
        .word NMI
        .word RESET
        .word $0000
"""
    return text


def build() -> bytes:
    """The whole ROM: a 16-byte iNES header, 32 KiB of PRG, 8 KiB of CHR."""
    header = bytes(
        [0x4E, 0x45, 0x53, 0x1A, 0x02, 0x01, 0x02, 0x00]
        + [0x00] * 8  # mapper 0, horizontal mirroring, battery, iNES
    )
    prg = nesasm.assemble(source(), origin=0x8000)
    if len(prg) != 32768:
        raise ValueError(f"PRG is {len(prg)} bytes, not 32768")
    return header + prg + chr_rom()
