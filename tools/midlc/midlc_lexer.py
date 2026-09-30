"""midlc lexer: turns `.midl` source text into a token stream.

Part of the Messenger IDL compiler (issue #90); see `midlc.py` for the CLI and
grammar. Kept separate from the parser so the token grammar and the recursive
descent logic can be reasoned about independently.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

TOKEN = re.compile(
    r"(?P<doc>///[^\n]*)"
    r"|(?P<comment>//[^\n]*)"
    r"|(?P<string>\"(?:[^\"\\]|\\.)*\")"
    r"|(?P<number>\d+)"
    r"|(?P<arrow>->)"
    r"|(?P<ident>[A-Za-z_][A-Za-z0-9_.]*)"
    r"|(?P<punct>[{}()<>:,;=])"
)


@dataclass
class Token:
    kind: str
    text: str
    line: int


def lex(text: str) -> list[Token]:
    tokens: list[Token] = []
    for match in TOKEN.finditer(text):
        line = text.count("\n", 0, match.start()) + 1
        kind = match.lastgroup
        assert kind is not None
        if kind in ("comment",):
            continue
        tokens.append(Token(kind, match.group(), line))
    return tokens
