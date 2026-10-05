"""midlc conformance: the language-neutral corpus in `idl/conformance/`.

Part of the Messenger IDL compiler; see `conformance.py` for the CLI and
`docs/midl.md` ("Conformance corpus") for the layout. Each `valid/<case>.midl`
has a `<case>.vectors.json` of sample values; `midlc` writes
`<case>.expected.json` with the parsed model (every id resolved), the
warnings, and the bytes of each sample (`midlc_wire`). Each
`invalid/<case>.midl` gets the exact error `midlc` rejects it with. Another
implementation of MIDL (a Rust `libs/midl`, a debugger) proves itself by
reproducing the expected files; the generated Rust codecs are held to the
same bytes by `libs/generated/tests/conformance.rs` (`midlc_conformance_rust`).
"""

from __future__ import annotations

import json
from pathlib import Path

from midlc_lexer import lex
from midlc_model import ERROR_FIELD, Interface, MidlError, Param
from midlc_parser import Parser
from midlc_rings import manifest_rings
from midlc_transfers import check_channel_targets
from midlc_wire import encode_error, encode_message

# `vectors.json` entries whose `message` is this encode the standard error.
ERROR_MESSAGE = "$error"


def parse_case(text: str) -> tuple[list[Interface], list[str]]:
    parser = Parser(lex(text))
    interfaces = parser.parse_interfaces()
    check_channel_targets(interfaces)
    return interfaces, parser.warnings


def field_rows(fields: list[Param]) -> list[dict]:
    return [{"name": f.name, "type": str(f.ty), "id": f.id, "explicit": f.explicit} for f in fields]


def transfer_row(transfer) -> dict:
    row = {"name": transfer.name, "kind": transfer.kind, "slot": transfer.index}
    if transfer.interface:
        row["interface"] = transfer.interface
    if transfer.rings:
        row["rings"] = transfer.rings
    return row


def model(interface: Interface) -> dict:
    """The parsed interface as plain data, every id resolved."""
    return {
        "name": interface.name,
        "id": f"{interface.id:#018x}",
        "doc": interface.docs,
        "methods": [
            {
                "name": m.name,
                "id": m.method_id,
                "oneway": m.oneway,
                "params": field_rows(m.params),
                "returns": field_rows(m.returns),
                "transfers": [transfer_row(t) for t in m.transfers],
            }
            for m in interface.methods
        ],
        "structs": [{"name": s.name, "fields": field_rows(s.fields)} for s in interface.structs],
        "enums": [
            {"name": e.name, "variants": [{"name": v, "value": i} for i, v in enumerate(e.variants)]}
            for e in interface.enums
        ],
        "topics": [
            {"pattern": t.name, "source": t.source, "payload": t.payload, "qos": t.qos, "retained": t.retained}
            for t in interface.topics
        ],
        "rings": manifest_rings(interface),
    }


def encode_vector(interfaces: list[Interface], vector: dict) -> bytes:
    if vector["message"] == ERROR_MESSAGE:
        return encode_error(ERROR_FIELD, vector["value"])
    name = vector.get("interface", interfaces[0].name)
    interface = next((i for i in interfaces if i.name == name), None)
    if interface is None:
        raise MidlError(f"vector {vector['name']!r}: no interface {name!r}")
    return encode_message(interface, vector["message"], vector["value"])


def expected_valid(midl: Path) -> dict:
    """The expected file of one valid case."""
    interfaces, warnings = parse_case(midl.read_text(encoding="utf-8"))
    vectors_path = midl.with_suffix(".vectors.json")
    vectors = json.loads(vectors_path.read_text(encoding="utf-8")) if vectors_path.is_file() else []
    out_vectors = []
    for vector in vectors:
        row = dict(vector)
        row["bytes"] = encode_vector(interfaces, vector).hex()
        out_vectors.append(row)
    return {
        "error_field": ERROR_FIELD,
        "interfaces": [model(i) for i in interfaces],
        "warnings": warnings,
        "vectors": out_vectors,
    }


def expected_invalid(midl: Path) -> dict:
    try:
        parse_case(midl.read_text(encoding="utf-8"))
    except MidlError as error:
        return {"error": str(error)}
    raise MidlError(f"{midl}: an invalid case parsed without error")


def render(data: dict) -> str:
    return json.dumps(data, indent=2, ensure_ascii=False) + "\n"


def expected_files(corpus: Path) -> dict[Path, str]:
    """Every expected file of the corpus and its content."""
    files: dict[Path, str] = {}
    for midl in sorted((corpus / "valid").glob("*.midl")):
        files[midl.with_suffix(".expected.json")] = render(expected_valid(midl))
    for midl in sorted((corpus / "invalid").glob("*.midl")):
        files[midl.with_suffix(".expected.json")] = render(expected_invalid(midl))
    return files


def stale_expected(corpus: Path, files: dict[Path, str]) -> list[Path]:
    """Expected files whose `.midl` is gone."""
    found = set(corpus.glob("*/*.expected.json"))
    return sorted(found - set(files))
