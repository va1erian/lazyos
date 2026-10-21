"""A hostile HTML message for the Mail harness (docs/mail.md, "Verification"): everything an HTML mail does to
reach the network, all pointed at the harness's listener. Mail shows it in a light view of webd, which has no
network at all, so the listener must see **nothing**; the heading in the page is how the harness (and the person
reading `shot_*.png`) knows the message itself did paint.

The message is put into the mock server's INBOX with an IMAP `APPEND` before the session starts
(`append`), so it needs nothing of the mock server but what its IMAP side already does.
"""

from __future__ import annotations

import imaplib
import ssl
from pathlib import Path

#: Where the guest reaches the harness's listener (`tools/webd/listener.py`, QEMU's user network).
LISTENER = "http://10.0.2.2:18080"
#: The subject the list row shows.
SUBJECT = "Hostile HTML (harness)"
#: The text of the heading; blue, so a screenshot of the pane shows that the message painted.
HEADING = "HOSTILE-MARKER"
#: The colour of the heading, as the message writes it.
HEADING_COLOUR = "#0000ff"

#: Every way the page could reach `LISTENER`: a meta refresh, a style sheet, scripts (external, inline and an
#: event handler), a picture, a frame, a form, a CSS `url()` and a link (which only a click would follow).
BODY = (
    '<html><head>'
    f'<meta http-equiv="refresh" content="0;url={LISTENER}/refresh">'
    f'<link rel="stylesheet" href="{LISTENER}/style.css">'
    f'<script src="{LISTENER}/script.js"></script>'
    f'<script>fetch("{LISTENER}/fetch"); new Image().src = "{LISTENER}/image";</script>'
    '</head>'
    f'<body onload="fetch(\'{LISTENER}/onload\')">'
    f'<h1 style="color:{HEADING_COLOUR}">{HEADING}</h1>'
    f'<img src="{LISTENER}/img.png" alt="remote picture">'
    f'<iframe src="{LISTENER}/frame"></iframe>'
    f'<form action="{LISTENER}/form" method="post"><input name="x" value="y"><input type="submit"></form>'
    f'<p style="background:url({LISTENER}/bg.png)">styled</p>'
    f'<p><a href="{LISTENER}/link">a link</a></p>'
    '</body></html>'
)


def raw_message() -> bytes:
    """The message as it goes over IMAP: CRLF lines, a date later than the mock's own so it lists first."""
    headers = [
        "From: Mallory <mallory@evil.test>",
        "To: alice@example.com",
        f"Subject: {SUBJECT}",
        "Date: Tue, 21 Oct 2026 12:00:00 +0000",
        "Message-ID: <hostile-1@evil.test>",
        "MIME-Version: 1.0",
        "Content-Type: text/html; charset=utf-8",
    ]
    return ("\r\n".join(headers) + "\r\n\r\n" + BODY + "\r\n").encode("utf-8")


def append(port: int, ca: Path, user: str, password: str, timeout: float = 15.0) -> None:
    """`APPEND` the message to INBOX of the mock server's IMAP side (TLS with its own CA, host `localhost`)."""
    context = ssl.create_default_context(cafile=str(ca))
    client = imaplib.IMAP4_SSL("localhost", port, ssl_context=context, timeout=timeout)
    try:
        client.login(user, password)
        status, data = client.append("INBOX", None, None, raw_message())
        if status != "OK":
            raise RuntimeError(f"APPEND refused: {status} {data}")
    finally:
        try:
            client.logout()
        except (imaplib.IMAP4.error, OSError):
            pass


def requests_in(text: str) -> list[str]:
    """The listener URLs `text` names: a self-check that the message really points the page at it."""
    marker = LISTENER
    found, start = [], 0
    while (at := text.find(marker, start)) != -1:
        end = at
        while end < len(text) and text[end] not in "\"') <>":
            end += 1
        found.append(text[at:end])
        start = end
    return found
