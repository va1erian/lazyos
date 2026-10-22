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
| Passwords | `secrets.rs`: a `keyring` backend that keeps a password in memory for the session and, when the account form's *Remember this password* is ticked, in `keyd`'s app secrets (`os.lazy.keyd.appsecret.v1`, docs/security-model.md section 8), sealed on disk, scoped to Mail and the user and read back at the next start; unticked, nothing is stored and a stored copy is deleted. A password is never written to a file by Mail, argv, the environment or the serial log (docs/tls-plan.md §6.3) |
| Files | `~/.config/esmail/config.toml` (accounts, no secrets) and `~/.local/share/esmail/` (the cache), under the user's home or `/transient` |
| Window | `xui-app/mail/src/app/`: folder pane, message list, reading pane (a light view of `webd`, `reader.rs`), compose and account pages |
| Reading pane | `xui_app::lightview::LightView` over `xui_webd_client::Pane` (docs/webd.md, docs/xui-docs.md): the message's HTML goes to a light view of **webd** (WebKit, no network, no scripts, no storage, locked navigation); one view shows every message |

esMail's Windows build is unchanged: its `native-tls` and `os-keyring` features
stay the defaults, and LazyOS builds it with `--no-default-features --features rustls`.

Mail is built with zig (`tools/xui/build.py --mail`), since SQLite is C (the
other apps need no zig). SQLite runs with its default rollback
journal: WAL needs shared file mappings, which LazyOS does not have yet.

## What works, what does not yet

- Accounts with a password over implicit TLS (IMAPS 993, SMTPS 465) or
  STARTTLS. OAuth sign-in (Gmail, Outlook) needs a browser and is not offered.
- Folders, paged headers, reading, reply and new messages, Get mail.
- Remote content (images, style sheets, fonts) is never loaded: a light view of
  webd has no network at all (no scheme but `data:`, `blob:` and `about:blank`
  loads, enforced in webd and by the interface Mail holds), so only inline `cid:`
  pictures (rewritten to `data:` by esMail) show; a remote picture is its `alt` text.
  esMail's sanitiser already removes scripts, frames, forms and `<meta refresh>` before
  the page leaves Mail, and the view runs with scripts off: a hostile message
  (`tools/mail/hostile.py`) must leave the harness's listener with no connection.
  A remote-content toggle would need a full web view (`os.lazy.webd.view.v1`, a later
  interface) with a policy of its own. A clicked link comes back as `LinkClicked`
  (Mail's `Msg::Link`) and is handed to `mimed` (`MAIL:LINK:OPEN`), so `http:` and `https:`
  links open in the web browser (LazyWeb+); a `mailto:` link opens a new message here.
  Nothing is ever followed in the pane.
- Mail registers `x-scheme-handler/mailto`, so a `mailto:` link in the browser,
  or `sys::mimed::open("mailto:someone@example.com?subject=Hi", "open")` from
  a `rhai` script, opens the compose window with the address, subject and
  body filled in.
- The password field is masked (xui's `Edit::password`) and its text cannot
  be copied or cut.
- The reading pane is a light view of webd. WebKit's `prefers-color-scheme` is compiled
  out of this port (docs/webd-plan.md, "P2 slice 1, engine half"), so a message that
  brings dark-mode media queries shows its light version; a message with no colours
  of its own gets the dark palette on a dark theme as before (esMail's `Palette`).
- **Mail needs webd.** webd is a resident app every desktop image ships and starts
  with the session. When it is not running the status line says so ("The web engine is
  not running: messages cannot be shown yet"), the pane stays blank and Mail keeps
  trying; the list, compose and accounts work without it. If webd loses the view (a
  crashed web process) Mail makes a new one and shows the message again. The
  reading pane is a picture at scale 1; on a HiDPI display the page is zoomed by the
  display's scale (CSS `zoom`).

## Verification

`tools/mail/run.py` builds an image with the harness's test CA and `tls.test`
mapped to the host, starts esMail's `mail-mock-server` behind TLS fronts for
`tls.test` (`tools/mail/tlsproxy.py`: IMAPS 9993, SMTPS 9465), and runs
`tools/screenshot/examples/mail_tls.json`, which adds the account, opens a
message and sends one. It passes when every marker below appears, both fronts
saw a TLS handshake for `tls.test`, and the password never reached the serial log.

Serial markers: `MAIL:UP:PASS`, `MAIL:CORE:ACCOUNTS=<n>`, `MAIL:ACCOUNT:SAVED|FAIL`,
`MAIL:CONNECTED:<account>`, `MAIL:FOLDERS:<account>:<n>`, `MAIL:HEADERS:<mailbox>:<n>`,
`MAIL:BODY:PASS:<uid>`, `MAIL:RENDER:PASS` (the pane drew the message), `MAIL:SEND:PASS|FAIL`, `MAIL:ERROR:<text>`,
and `MAIL:BIND:FAIL` / `MAIL:RUN:FAIL` from `main.rs`.

`--label-trace` builds with `LAZYOS_LABEL_TRACE=1` and lists every `LABEL:DENY`,
which is how the package's permissions (`xui-app/packages/mail/manifest.toml`) were derived.
