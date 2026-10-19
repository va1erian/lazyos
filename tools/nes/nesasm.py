#!/usr/bin/env python3
"""A two-pass 6502 assembler for the NES Player's sample ROM.

The sample ROM (`tools/nes/sample_rom.py`) is built on the host at image-build
time and checked in, and the repo has no cc65/asm6 dependency to build it with.
This is the small assembler it uses instead: the official opcodes, the operand
shapes a hand-written ROM needs, labels, `.byte`/`.word`/`.res` and `.org`.

    joy = $10               ; a named constant, for zero-page variables
    start:                  ; a label at the current address
        lda #$10            ; immediate (#)
        sta $2007           ; zero page for a value < $100, absolute otherwise
        lda table,x         ; ,x / ,y indexing
        jmp (vector)        ; (indirect), and (indirect,x) / (indirect),y
        bne start           ; a relative branch, resolved in the second pass
        lda #<table         ; the low byte of a label; > is the high byte
        .byte 1, 2, $FF     ; raw bytes; a "string" emits its bytes
        .word start         ; a 16-bit little-endian word
        .res 16             ; 16 zero bytes
        .org $FFFA          ; pad with zero bytes to this address

Pass one lays the program out and records the labels; pass two emits, so a
forward branch and a forward `.word` both resolve. An operand that is a value
below `$100` assembles as zero page and anything else as absolute; a forward
label counts as absolute, and the second pass keeps the mode the first chose,
so the two always agree.
"""

from __future__ import annotations

# (mnemonic, mode) -> opcode. The official instruction set.
OPCODES = {
    ("adc", "imm"): 0x69, ("adc", "zp"): 0x65, ("adc", "zpx"): 0x75,
    ("adc", "abs"): 0x6D, ("adc", "absx"): 0x7D, ("adc", "absy"): 0x79,
    ("adc", "indx"): 0x61, ("adc", "indy"): 0x71,
    ("and", "imm"): 0x29, ("and", "zp"): 0x25, ("and", "zpx"): 0x35,
    ("and", "abs"): 0x2D, ("and", "absx"): 0x3D, ("and", "absy"): 0x39,
    ("and", "indx"): 0x21, ("and", "indy"): 0x31,
    ("asl", "acc"): 0x0A, ("asl", "zp"): 0x06, ("asl", "zpx"): 0x16,
    ("asl", "abs"): 0x0E, ("asl", "absx"): 0x1E,
    ("bcc", "rel"): 0x90, ("bcs", "rel"): 0xB0, ("beq", "rel"): 0xF0,
    ("bit", "zp"): 0x24, ("bit", "abs"): 0x2C, ("bmi", "rel"): 0x30,
    ("bne", "rel"): 0xD0, ("bpl", "rel"): 0x10, ("brk", "imp"): 0x00,
    ("bvc", "rel"): 0x50, ("bvs", "rel"): 0x70,
    ("clc", "imp"): 0x18, ("cld", "imp"): 0xD8, ("cli", "imp"): 0x58,
    ("clv", "imp"): 0xB8,
    ("cmp", "imm"): 0xC9, ("cmp", "zp"): 0xC5, ("cmp", "zpx"): 0xD5,
    ("cmp", "abs"): 0xCD, ("cmp", "absx"): 0xDD, ("cmp", "absy"): 0xD9,
    ("cmp", "indx"): 0xC1, ("cmp", "indy"): 0xD1,
    ("cpx", "imm"): 0xE0, ("cpx", "zp"): 0xE4, ("cpx", "abs"): 0xEC,
    ("cpy", "imm"): 0xC0, ("cpy", "zp"): 0xC4, ("cpy", "abs"): 0xCC,
    ("dec", "zp"): 0xC6, ("dec", "zpx"): 0xD6, ("dec", "abs"): 0xCE,
    ("dec", "absx"): 0xDE, ("dex", "imp"): 0xCA, ("dey", "imp"): 0x88,
    ("eor", "imm"): 0x49, ("eor", "zp"): 0x45, ("eor", "zpx"): 0x55,
    ("eor", "abs"): 0x4D, ("eor", "absx"): 0x5D, ("eor", "absy"): 0x59,
    ("eor", "indx"): 0x41, ("eor", "indy"): 0x51,
    ("inc", "zp"): 0xE6, ("inc", "zpx"): 0xF6, ("inc", "abs"): 0xEE,
    ("inc", "absx"): 0xFE, ("inx", "imp"): 0xE8, ("iny", "imp"): 0xC8,
    ("jmp", "abs"): 0x4C, ("jmp", "ind"): 0x6C, ("jsr", "abs"): 0x20,
    ("lda", "imm"): 0xA9, ("lda", "zp"): 0xA5, ("lda", "zpx"): 0xB5,
    ("lda", "abs"): 0xAD, ("lda", "absx"): 0xBD, ("lda", "absy"): 0xB9,
    ("lda", "indx"): 0xA1, ("lda", "indy"): 0xB1,
    ("ldx", "imm"): 0xA2, ("ldx", "zp"): 0xA6, ("ldx", "zpy"): 0xB6,
    ("ldx", "abs"): 0xAE, ("ldx", "absy"): 0xBE,
    ("ldy", "imm"): 0xA0, ("ldy", "zp"): 0xA4, ("ldy", "zpx"): 0xB4,
    ("ldy", "abs"): 0xAC, ("ldy", "absx"): 0xBC,
    ("lsr", "acc"): 0x4A, ("lsr", "zp"): 0x46, ("lsr", "zpx"): 0x56,
    ("lsr", "abs"): 0x4E, ("lsr", "absx"): 0x5E,
    ("nop", "imp"): 0xEA,
    ("ora", "imm"): 0x09, ("ora", "zp"): 0x05, ("ora", "zpx"): 0x15,
    ("ora", "abs"): 0x0D, ("ora", "absx"): 0x1D, ("ora", "absy"): 0x19,
    ("ora", "indx"): 0x01, ("ora", "indy"): 0x11,
    ("pha", "imp"): 0x48, ("php", "imp"): 0x08, ("pla", "imp"): 0x68,
    ("plp", "imp"): 0x28,
    ("rol", "acc"): 0x2A, ("rol", "zp"): 0x26, ("rol", "zpx"): 0x36,
    ("rol", "abs"): 0x2E, ("rol", "absx"): 0x3E,
    ("ror", "acc"): 0x6A, ("ror", "zp"): 0x66, ("ror", "zpx"): 0x76,
    ("ror", "abs"): 0x6E, ("ror", "absx"): 0x7E,
    ("rti", "imp"): 0x40, ("rts", "imp"): 0x60,
    ("sbc", "imm"): 0xE9, ("sbc", "zp"): 0xE5, ("sbc", "zpx"): 0xF5,
    ("sbc", "abs"): 0xED, ("sbc", "absx"): 0xFD, ("sbc", "absy"): 0xF9,
    ("sbc", "indx"): 0xE1, ("sbc", "indy"): 0xF1,
    ("sec", "imp"): 0x38, ("sed", "imp"): 0xF8, ("sei", "imp"): 0x78,
    ("sta", "zp"): 0x85, ("sta", "zpx"): 0x95, ("sta", "zpy"): 0x94,
    ("sta", "abs"): 0x8D, ("sta", "absx"): 0x9D, ("sta", "absy"): 0x99,
    ("sta", "indx"): 0x81, ("sta", "indy"): 0x91,
    ("stx", "zp"): 0x86, ("stx", "zpy"): 0x96, ("stx", "abs"): 0x8E,
    ("sty", "zp"): 0x84, ("sty", "zpx"): 0x94, ("sty", "abs"): 0x8C,
    ("tax", "imp"): 0xAA, ("tay", "imp"): 0xA8, ("tsx", "imp"): 0xBA,
    ("txa", "imp"): 0x8A, ("txs", "imp"): 0x9A, ("tya", "imp"): 0x98,
}

# Bytes each addressing mode occupies, including the opcode.
SIZES = {"imp": 1, "acc": 1, "imm": 2, "zp": 2, "zpx": 2, "zpy": 2,
         "rel": 2, "indx": 2, "indy": 2, "abs": 3, "absx": 3, "absy": 3,
         "ind": 3}


class AsmError(Exception):
    """The source could not be assembled; the message names the line."""


def parse_number(text: str) -> int | None:
    """A literal (`$1F`, `%101`, `31`), or `None` when it is not one."""
    text = text.strip()
    try:
        if text.startswith("$"):
            return int(text[1:], 16)
        if text.startswith("%"):
            return int(text[1:], 2)
        return int(text, 10)
    except ValueError:
        return None


def split_terms(expr: str, line: int) -> list[tuple[int, str]]:
    """`a+1-b` as `[(1, "a"), (1, "1"), (-1, "b")]`."""
    terms, sign, current = [], 1, ""
    for char in expr:
        if char in "+-" and current.strip():
            terms.append((sign, current))
            sign, current = (1 if char == "+" else -1), ""
        else:
            current += char
    if current.strip():
        terms.append((sign, current))
    if not terms:
        raise AsmError(f"line {line}: empty expression")
    return terms


def evaluate(expr: str, labels: dict[str, int], line: int) -> int:
    """A constant expression: a number, a label, a label plus or minus a
    number, or `<`/`>` of any of those (the low or high byte)."""
    expr = expr.strip()
    byte_of = None
    if expr[:1] in ("<", ">"):
        byte_of, expr = expr[0], expr[1:].strip()
    total = 0
    for sign, term in split_terms(expr, line):
        term = term.strip()
        number = parse_number(term)
        if number is not None:
            total += sign * number
        elif term in labels:
            total += sign * labels[term]
        else:
            raise AsmError(f"line {line}: unknown label {term!r}")
    if byte_of == "<":
        return total & 0xFF
    if byte_of == ">":
        return (total >> 8) & 0xFF
    return total


def shape_of(operand: str | None, line: int) -> tuple[str, str]:
    """The syntactic shape of an operand and its expression: one of `imp`,
    `acc`, `imm`, `ind`, `indx`, `indy`, `direct`, `indexed-x`, `indexed-y`."""
    if operand is None or not operand.strip():
        return "imp", ""
    operand = operand.strip()
    if operand.upper() == "A":
        return "acc", ""
    if operand.startswith("#"):
        return "imm", operand[1:].strip()
    if operand.startswith("("):
        close = operand.find(")")
        if close < 0:
            raise AsmError(f"line {line}: unbalanced ( in {operand!r}")
        inner = operand[1:close].strip()
        rest = operand[close + 1 :].strip()
        if rest.lower() == ",y":
            return "indy", inner
        if rest:
            raise AsmError(f"line {line}: unexpected {rest!r} after {operand!r}")
        if inner.lower().endswith(",x"):
            return "indx", inner[:-2].strip()
        return "ind", inner
    lowered = operand.lower()
    if lowered.endswith(",x"):
        return "indexed-x", operand[:-2].strip()
    if lowered.endswith(",y"):
        return "indexed-y", operand[:-2].strip()
    return "direct", operand


def concrete(shape: str, expr: str, known: dict[str, int], line: int) -> str:
    """The addressing mode: a value below `$100` is zero page, anything else
    (including a label not yet defined) absolute. The mode is decided once,
    in the first pass, and kept."""
    if shape in ("imp", "acc", "imm", "ind", "indx", "indy"):
        return shape
    try:
        value: int | None = evaluate(expr, known, line)
    except AsmError:
        value = None  # a forward label: absolute, which the second pass keeps
    small = value is not None and 0 <= value < 0x100
    if shape == "direct":
        return "zp" if small else "abs"
    index = "x" if shape == "indexed-x" else "y"
    if small:
        return "zpx" if index == "x" else "zpy"
    return "absx" if index == "x" else "absy"


def byte_values(operand: str | None, line: int) -> list[int]:
    """`.byte` arguments: numbers and "strings"."""

    def number(text: str) -> int:
        text = text.strip()
        if text.startswith("$"):
            return int(text[1:], 16)
        if text.startswith("%"):
            return int(text[1:], 2)
        return int(text, 0)

    if not operand:
        raise AsmError(f"line {line}: .byte needs a value")
    values, text, quote = [], "", None
    for char in operand:
        if quote:
            if char == quote:
                values.extend(text.encode("ascii"))
                text, quote = "", None
            else:
                text += char
        elif char in "\"'":
            if text.strip():
                values.append(number(text))
            text, quote = "", char
        elif char == ",":
            if text.strip():
                values.append(number(text))
            text = ""
        else:
            text += char
    if quote:
        raise AsmError(f"line {line}: unterminated string")
    if text.strip():
        values.append(number(text))
    for value in values:
        if not 0 <= value <= 0xFF:
            raise AsmError(f"line {line}: byte {value} is out of range")
    return values


def directive(head: str, operand: str | None, line: int) -> tuple[str, object]:
    """One `.` directive as `(kind, payload)`."""
    if head == ".byte":
        return "byte", byte_values(operand, line)
    if head == ".word":
        values = [part.strip() for part in (operand or "").split(",") if part.strip()]
        if not values:
            raise AsmError(f"line {line}: .word needs a value")
        return "word", values
    if head == ".res":
        value = parse_number(operand or "")
        if value is None or value < 0:
            raise AsmError(f"line {line}: .res needs a count")
        return "res", value
    raise AsmError(f"line {line}: unknown directive {head!r}")


def assemble(source: str, origin: int = 0x8000) -> bytes:
    """The machine code `source` assembles to, loaded at `origin`."""
    items: list[tuple] = []  # ("ins", address, mnemonic, mode, expr, line)
    labels: dict[str, int] = {}
    constants: dict[str, int] = {}
    address = origin
    for number, raw in enumerate(source.splitlines(), start=1):
        line = raw.split(";", 1)[0].rstrip()
        if not line.strip():
            continue
        rest = line
        if "=" in rest and ":" not in rest.split("=", 1)[0]:
            name, expr = rest.split("=", 1)
            name = name.strip()
            if not name or not name.replace("_", "").isalnum():
                raise AsmError(f"line {number}: bad constant {name!r}")
            constants[name] = evaluate(expr, {**labels, **constants}, number)
            continue
        if ":" in rest:
            label, rest = rest.split(":", 1)
            label = label.strip()
            if not label or not label.replace("_", "").isalnum():
                raise AsmError(f"line {number}: bad label {label!r}")
            if label in labels:
                raise AsmError(f"line {number}: label {label!r} already defined")
            labels[label] = address
        rest = rest.strip()
        if not rest:
            continue
        parts = rest.split(None, 1)
        head = parts[0].lower()
        operand = parts[1].strip() if len(parts) > 1 else None
        if head == ".org":
            value = parse_number(operand or "")
            if value is None:
                raise AsmError(f"line {number}: .org needs a number")
            if value < address:
                raise AsmError(f"line {number}: .org {operand} goes backwards")
            items.append(("org", value))
            address = value
            continue
        if head.startswith("."):
            kind, payload = directive(head, operand, number)
            size = len(payload) if kind == "byte" else (
                2 * len(payload) if kind == "word" else int(payload))
            items.append((kind, payload, number))
            address += size
            continue
        if head not in {mnemonic for mnemonic, _ in OPCODES}:
            raise AsmError(f"line {number}: unknown mnemonic {head!r}")
        shape, expr = shape_of(operand, number)
        mode = concrete(shape, expr, {**labels, **constants}, number)
        if (head, mode) not in OPCODES:
            # The only form the mnemonic has: a branch is relative, and an
            # operandless shift/rotate is the accumulator form.
            if shape == "direct" and (head, "rel") in OPCODES:
                mode = "rel"
            elif shape == "imp" and (head, "acc") in OPCODES:
                mode = "acc"
        if (head, mode) not in OPCODES:
            raise AsmError(f"line {number}: {head} has no {mode} form")
        items.append(("ins", address, head, mode, expr, number))
        address += SIZES[mode]
    return emit(items, {**labels, **constants}, origin)


def emit(items: list[tuple], labels: dict[str, int], origin: int) -> bytes:
    """Pass two: the bytes, with labels, branches, words and `.org` resolved."""
    out = bytearray()
    for item in items:
        kind = item[0]
        if kind == "org":
            if item[1] < origin + len(out):
                raise AsmError(f"cannot pad back to {item[1]:#x}")
            out.extend(b"\0" * (item[1] - origin - len(out)))
        elif kind == "res":
            out.extend(b"\0" * item[1])
        elif kind == "byte":
            out.extend(item[1])
        elif kind == "word":
            for value in item[1]:
                number = evaluate(value, labels, item[2])
                if not 0 <= number <= 0xFFFF:
                    raise AsmError(f"line {item[2]}: word {value} is out of range")
                out.extend(number.to_bytes(2, "little"))
        else:
            _, address, mnemonic, mode, expr, line = item
            out.append(OPCODES[(mnemonic, mode)])
            if mode in ("imp", "acc"):
                continue
            if mode == "rel":
                target = evaluate(expr, labels, line)
                offset = target - (address + 2)
                if not -128 <= offset <= 127:
                    raise AsmError(f"line {line}: branch to {expr} is out of reach")
                out.append(offset & 0xFF)
                continue
            value = evaluate(expr, labels, line)
            if mode in ("zp", "zpx", "zpy", "indx", "indy", "imm"):
                if not 0 <= value <= 0xFF:
                    raise AsmError(f"line {line}: zero-page value {expr} is too large")
                out.append(value)
            else:
                if not 0 <= value <= 0xFFFF:
                    raise AsmError(f"line {line}: address {expr} is out of range")
                out.extend(value.to_bytes(2, "little"))
    return bytes(out)
