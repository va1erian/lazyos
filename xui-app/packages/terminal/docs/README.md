# Terminal

A terminal window running your shell: tabs of output, scrollback, search,
selection and links. Preferences are on `Ctrl+,`. See `docs/terminal-plan.md`.

This is a **developer tool** and **your shell**. Everything you type runs with
your own authority, and the shell and every command in it keep the label
`tool:os.lazy.terminal`. Unlike other developer tools, the label does not
refuse them the password, key and account services: those services judge a
command here as they judge you (by your user and session, so what you may not
do, a command may not do either). Package installs, `powerctl` and an
administrator's approval prompt (`elevd`) work from here as they would for you.
They do not work from the other developer tools, such as the Messenger
Playground: the Terminal is the only tool the image marks as the user's shell
(`shell = true`, which only a package the image ships can have). The label
policy loader stays refused to every tool.

Read a script before you run it, especially one you did not write.
