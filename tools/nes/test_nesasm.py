#!/usr/bin/env python3
"""Self-test of tools/nes/nesasm.py and tools/nes/sample_rom.py: the assembler
emits the bytes the 6502 manual says, refuses what it cannot encode, and the
sample ROM builds deterministically and is a well-formed iNES cartridge."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import make_sample  # noqa: E402
import nesasm  # noqa: E402
import sample_rom  # noqa: E402


class Assemble(unittest.TestCase):
    def assemble(self, source: str) -> bytes:
        return nesasm.assemble(source, origin=0x8000)

    def test_every_addressing_mode(self):
        # One instruction per shape, with the opcode and operand the manual
        # gives. `$10` is zero page, `$8000` absolute, a label absolute.
        source = """
        joy = $10
        ptr = $20
        start:
            lda #$05        ; A9 05
            sta $10         ; 85 10
            sta joy         ; 85 10
            lda $1234       ; AD 34 12
            lda $10,x       ; B5 10
            lda $1234,x     ; BD 34 12
            lda $1234,y     ; B9 34 12
            ldx $10,y       ; B6 10
            sta $1234,y     ; 99 34 12
            lda (ptr,x)     ; A1 20
            lda (ptr),y     ; B1 20
            jmp (ptr)       ; 6C 20 00
            asl             ; 0A
            rts             ; 60
            nop             ; EA
        """
        self.assertEqual(
            self.assemble(source),
            bytes.fromhex(
                "a905 8510 8510 ad3412 b510 bd3412 b93412 b610 993412"
                " a120 b120 6c2000 0a 60 ea"
            ),
        )

    def test_a_branch_is_relative_to_the_next_instruction(self):
        source = """
        loop:
            inx             ; E8
            cpx #$10        ; E0 10
            bne loop        ; D0 FB: $8006 - ($8009 + 2)
        """
        self.assertEqual(self.assemble(source), bytes.fromhex("e8e010d0fb"))

    def test_a_forward_branch_and_word_resolve(self):
        source = """
        start:
            jmp done        ; 4C 04 80
            .byte $FF       ; FF
        done:
            .word start     ; 00 80
        """
        self.assertEqual(self.assemble(source), bytes.fromhex("4c0480 ff 0080"))

    def test_byte_word_res_and_org(self):
        source = """
            .byte 1, $FF, "AB"  ; 01 FF 41 42
            .word $1234         ; 34 12
            .res 2              ; 00 00
            .org $8010
            .byte 7             ; 07, with the gap zero-filled
        """
        out = self.assemble(source)
        self.assertEqual(out[:8], bytes.fromhex("01ff4142 3412 0000"))
        self.assertEqual(len(out), 0x11)
        self.assertEqual(out[0x10], 7)

    def test_a_low_and_high_byte_of_a_label(self):
        source = """
            lda #<data       ; A9 05 (data is at $8005)
            lda #>data       ; A9 80
            rts              ; 60
        data:
            .byte 0
        """
        self.assertEqual(self.assemble(source), bytes.fromhex("a905 a980 60 00"))

    def test_errors_are_refused(self):
        for source in (
            "        frob #$01",          # unknown mnemonic
            "        lda #$01,x",         # no such form
            "        bne nowhere",        # out of reach (and unknown)
            "        .byte 256",          # out of range
            "        lda (ptr",           # unbalanced
        ):
            with self.assertRaises(nesasm.AsmError, msg=source):
                self.assemble(source)
        with self.assertRaises(nesasm.AsmError):
            # A branch that is in range of the label but past the offset limit.
            self.assemble("        beq far\n" + "        .res 200\nfar:    rts\n")


class SampleRom(unittest.TestCase):
    def test_the_rom_builds_deterministically_and_is_well_formed(self):
        first = sample_rom.build()
        self.assertEqual(first, sample_rom.build())
        self.assertIsNone(make_sample.check(first))

    def test_the_vectors_point_into_the_cartridge(self):
        rom = sample_rom.build()
        prg = rom[16 : 16 + 32768]
        nmi = int.from_bytes(prg[-6:-4], "little")
        reset = int.from_bytes(prg[-4:-2], "little")
        self.assertGreaterEqual(reset, 0x8000)
        self.assertGreaterEqual(nmi, 0x8000)
        # `sei` at the reset vector, so the first instruction is really there.
        self.assertEqual(prg[reset - 0x8000], 0x78)

    def test_check_names_a_damaged_cartridge(self):
        rom = bytearray(sample_rom.build())
        rom[0] = 0
        self.assertIsNotNone(make_sample.check(bytes(rom)))
        rom = bytearray(sample_rom.build())
        rom[16 + 32764] = 0  # a reset vector of zero
        rom[16 + 32765] = 0
        self.assertIsNotNone(make_sample.check(bytes(rom)))


if __name__ == "__main__":
    unittest.main(verbosity=2)
