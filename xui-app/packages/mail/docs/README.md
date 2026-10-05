# Mail

Mail is esMail on LazyOS: it reads your mail over IMAP and sends it over SMTP,
always over TLS, with the server's certificate checked against the system's
trusted roots.

- **Add an account** with *Accounts*: your address, the password, and the
  servers (filled in for well-known providers). Gmail, Outlook and iCloud need
  an *app password*; Google sign-in needs a web browser, which LazyOS does not
  have yet.
- **Passwords are kept for the session only.** LazyOS has no secrets service
  yet, so Mail asks for each account's password again when it next starts.
- **Read** by picking a folder, then a message. HTML mail is shown sanitised,
  without running scripts and without loading remote images.
- **Write** with *New* or *Reply*.
