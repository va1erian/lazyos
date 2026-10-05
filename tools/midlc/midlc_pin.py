"""midlc --pin-field-ids: write every implicit field id into the source.

Part of the Messenger IDL compiler; see `midlc.py` for the CLI. A field
without `= N` takes its 1-based position, so reordering or inserting a field
silently renumbers the wire. Pinning rewrites `name: Type` as
`name: Type = <position>` in place, which keeps the wire byte-identical
(the id written is the one the field already had) and makes the next
reordering safe. Everything else in the file (comments, layout) is kept.
"""

from __future__ import annotations

from pathlib import Path

from midlc_lexer import lex
from midlc_parser import Parser


def implicit_fields(text: str):
    """`(end_offset, id)` of every implicit struct field, argument and reply
    field in `text`, the offset being just past the field's type."""
    parser = Parser(lex(text))
    found = []
    for interface in parser.parse_interfaces():
        lists = [s.fields for s in interface.structs]
        for method in interface.methods:
            lists += [method.params, method.returns]
        for fields in lists:
            found += [(f.end, f.id) for f in fields if not f.explicit]
    return found


def pin_text(text: str) -> tuple[str, int]:
    """`text` with every implicit field id written out, and how many."""
    edits = sorted(implicit_fields(text), reverse=True)
    for end, field_id in edits:
        text = f"{text[:end]} = {field_id}{text[end:]}"
    return text, len(edits)


def pin_files(paths: list[Path]) -> int:
    """Pin every file in place; returns the number of fields pinned."""
    total = 0
    for path in paths:
        text = path.read_text(encoding="utf-8")
        pinned, count = pin_text(text)
        if count:
            path.write_text(pinned, encoding="utf-8", newline="\n")
            print(f"midlc: pinned {count} field id(s) in {path}")
        total += count
    return total
