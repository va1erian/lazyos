# Mail (esMail on LazyOS)

`xui-mail` (`os.lazy.mail`, `xui-app/mail/`) is [esMail](https://github.com/va1erian/esmail)
on the LazyOS desktop: the same IMAP/SMTP core, SQLite cache and HTML
sanitiser as the Windows client, behind a xui window. It is an optional core
package: images built with `LAZYOS_MAIL=1` ship it and `pkgd` installs it at boot.

```bash
python tools/run_demo.py --mail      # desktop + networking + HTTPS + Mail
python tools/mail/run.py             # build, run against a mock server over TLS, judge
```

The GUI launcher has it as *Mail* on the Simple tab (Desktop extras) and
*Mail app* on the Advanced tab; either turns HTTPS and networking on.

## How it is put together

| Piece | Where |
|---|---|
| Mail core (IMAP sessions, SMTP, cache, rendering to sanitised HTML) | `esmail` and `esmail-glue` crates, git dependency pinned in `xui-app/mail/Cargo.toml` |
| TLS | rustls through esMail's `rustls` feature; the crypto is `nettls-crypto` (RustCrypto, docs/tls-plan.md §3.2), installed as the process default in `main.rs` |
| Trust roots | `/etc/ssl/certs/ca-certificates.crt`, the bundle `LAZYOS_TLS=1` ships (`rustls-native-certs` finds it) |
| Passwords | `secrets.rs`: an in-memory `keyring` backend, so a password is asked once per session and never written to disk, argv, the environment or the serial log (docs/tls-plan.md §6.3) |
| Files | `~/.config/esmail/config.toml` (accounts, no secrets) and `~/.local/share/esmail/` (the cache), under the user's home or `/transient` |
| Window | `xui-app/mail/src/app/`: folder pane, message list, reading pane (litehtml `HtmlView`), compose and account pages |

esMail's Windows build is unchanged: its `native-tls` and `os-keyring` features
stay the defaults, and LazyOS builds it with `--no-default-features --features rustls`.

Mail is built like the Docs app, with zig (`tools/xui/build.py --mail`), since
SQLite and litehtml are C and C++. SQLite runs with its default rollback
journal: WAL needs shared file mappings, which LazyOS does not have yet.

## What works, what does not yet

- Accounts with a password over implicit TLS (IMAPS 993, SMTPS 465) or
  STARTTLS. OAuth sign-in (Gmail, Outlook) needs a browser and is not offered.
- Folders, paged headers, reading, reply and new messages, Get mail.
- Remote images are not loaded. A clicked link is handed to `mimed`
  (`MAIL:LINK:OPEN`), so `http:` and `https:` links open in LazyWeb; a
  `mailto:` link opens a new message here.
- Mail registers `x-scheme-handler/mailto`, so `messengerctl open
  mailto:someone@example.com?subject=Hi` (or a `mailto:` link in LazyWeb)
  opens the compose window with the address, subject and body filled in.
- The password field is masked (xui's `Edit::password`) and its text cannot
  be copied or cut.
- The reading pane is litehtml for now; NetSurf (`xui-netsurf`) is the planned
  replacement once the licence question (esMail is GPL-3.0-only, NetSurf
  GPL-2.0-only) is settled.

## Verification

`tools/mail/run.py` builds an image with the harness's test CA and `tls.test`
mapped to the host, starts esMail's `mail-mock-server` behind TLS fronts for
`tls.test` (`tools/mail/tlsproxy.py`: IMAPS 9993, SMTPS 9465), and runs
`tools/screenshot/examples/mail_tls.json`, which adds the account, opens a
message and sends one. It passes when every marker below appears, both fronts
saw a TLS handshake for `tls.test`, and the password never reached the serial log.

Serial markers: `MAIL:UP:PASS`, `MAIL:CORE:ACCOUNTS=<n>`, `MAIL:ACCOUNT:SAVED|FAIL`,
`MAIL:CONNECTED:<account>`, `MAIL:FOLDERS:<account>:<n>`, `MAIL:HEADERS:<mailbox>:<n>`,
`MAIL:BODY:PASS:<uid>`, `MAIL:RENDER:PASS`, `MAIL:SEND:PASS|FAIL`, `MAIL:ERROR:<text>`,
and `MAIL:BIND:FAIL` / `MAIL:RUN:FAIL` from `main.rs`.

`--label-trace` builds with `LAZYOS_LABEL_TRACE=1` and lists every `LABEL:DENY`,
which is how the package's permissions (`xui-app/packages/mail/manifest.toml`) were derived.
