# The `rhai` command

`rhai` is the Rhai interpreter as an ordinary LazyOS command (`/system/bin/rhai`,
a static musl program). It runs a one-line expression, a script file, a script
piped in on standard input, or an interactive REPL. It reads standard input,
writes standard output and standard error, and sets an exit status, so it fits
into `sh` pipelines like any other tool. On LazyOS it can also call every
Messenger service ([msg](msg.md)); the same command built for another system has
only the language and the `os` functions.

```bash
rhai -e '1 + 2'
3
rhai                       # the REPL
rhai backup.rhai /home/user/Documents
```

## Usage

```text
rhai [options] [-e EXPR | SCRIPT | -] [ARG...]
rhai                      interactive REPL
rhai -e EXPR [ARG...]     evaluate EXPR and print its value
rhai SCRIPT [ARG...]      run a script file (a #! first line is ignored)
rhai - [ARG...]           run the script read from standard input
```

Options come first. They end at the first word that is not an option (the
script, or `-`, or the first argument after `-e EXPR`) or at `--`; everything
after that belongs to the script and shows up in `os::args()`. `--name=value`
is the same as `--name value`.

| Mode | Command | Prints the final value? |
|---|---|---|
| expression | `rhai -e EXPR [ARG...]` (also `--eval`) | yes, unless it is `()` |
| script file | `rhai SCRIPT [ARG...]` | no: only `print` shows anything |
| script from stdin | `rhai - [ARG...]` | no |
| REPL | `rhai` (add `-q` for batch input) | yes, after every entry |

```bash
rhai -e 'os::args()' x y
["x", "y"]
echo 'print("hello " + os::args()[0])' | rhai - world
hello world
rhai -- -odd-name.rhai arg      # a script whose name starts with a dash
```

## Options

| Option | Meaning |
|---|---|
| `-e`, `--eval EXPR` | evaluate `EXPR` and print its value unless it is `()` |
| `-q`, `--quiet` | REPL only: no banner, prompts or echo, for `... \| rhai -q` |
| `--sandbox` | switch off `eval` and `import`, so the script cannot compile code it was not given |
| `--max-ops N` | operations before a script is stopped (default 10,000,000; `0` = unlimited) |
| `--max-call-levels N` | nested function calls (default 32; 1 to 64) |
| `--max-expr-depth N` | expression nesting depth (default 64, and 32 inside functions; at least 1) |
| `--max-string N` | bytes in one string (default 4 MiB; `0` = unlimited) |
| `--max-array N` | elements in one array (default 100,000; `0` = unlimited) |
| `--max-map N` | entries in one map (default 100,000; `0` = unlimited) |
| `--max-io N` | bytes one `read`, `stdin_text` or script file may load (default 4 MiB; at least 1) |
| `-h`, `--help` | the usage text |
| `-V`, `--version` | `rhai 0.1.0 (engine 1.26.1)` |

The limits and what happens when one is reached are described in
[language](language.md) (section "Engine limits"). A bad option, a missing value or a number
that does not parse is a usage error (exit status 2) and nothing runs.

## Exit status

| Status | When |
|---|---|
| `0` | the script finished; `-h`, `-V`; a quiet stop when the reader of standard output went away (`rhai ... \| head -1`) |
| `n` | the script called `exit(n)`; the status is `n` modulo 256, so `exit(300)` gives 44 |
| `1` | a compile error, an uncaught error or `throw`, a limit reached, or a script that could not be read |
| `2` | a usage error (unknown option, bad number, missing value) |

Errors go to standard error as `rhai: <message> (line N, position M)`; for a
script file the message starts with the file name, `rhai: backup.rhai: ...`.
`exit` is the one way to leave early on purpose. It cannot be caught by `try`.

```bash
rhai -e 'exit(3)'; echo $?
3
rhai -e 'let x = ;'; echo $?
rhai: Unexpected ';' (line 1, position 9)
1
rhai --max-ops 1000 -e 'loop {}'; echo $?
rhai: Too many operations (line 1, position 12)
1
```

## Scripts and `#!`

A script is a text file, normally named `*.rhai`, in UTF-8. A first line that
starts with `#!` is ignored (its newline is kept, so error line numbers match the
file). Make it executable and run it by name; the kernel finds the interpreter
from the line:

```bash
cat > hello.rhai <<'EOF'
#!/bin/rhai
print("hello " + os::args()[0]);
exit(5);
EOF
chmod +x hello.rhai
./hello.rhai world
hello world
echo $?
5
```

`#!/usr/bin/env rhai` works too, and options may follow the interpreter
(`#!/bin/rhai --sandbox`). Arguments after the script name become `os::args()`.
A script is read through the same bounded path as `os::read`: at most
`--max-io` bytes, and it must be valid UTF-8.

`import "name"` loads `name.rhai` relative to the process's current directory,
not to the script's, so `cd` to the script's folder first, or give a full path
([language](language.md), section "Modules and import").

## The REPL

Run `rhai` with no script to get a prompt. Because the LazyOS console and the
desktop Terminal give a program raw keystrokes, `rhai` does its own echo and
line editing:

```text
rhai REPL: :help, :quit or Ctrl-D
rhai> 100 + 23
123
rhai> fn sq(n) {
  ... n * n
  ... }
rhai> sq(12)
144
rhai> [1, 2,
  ... 3]
[1, 2, 3]
rhai> missing + 1
error: Variable not found: missing (line 1, position 1)
rhai> :quit
```

- **Multi-line input.** When what you typed stops too early (an open `{`, `(` or
  `[`, a missing `;` at the end, an unterminated back-tick string) the prompt
  changes to `  ... ` and the next line is added to the entry. A real syntax
  error is reported at once. Up to 1 MiB per entry.
- **State.** Variables and functions stay defined from one entry to the next.
  Values are shown in the `debug` style (strings with quotes); `()` shows
  nothing. Errors are printed on standard error, prefixed `error:`.
- **Commands** (a line that starts with `:` when no entry is open):

  | Command | Does |
  |---|---|
  | `:help`, `:h`, `:?` | the help text |
  | `:history` | the entries so far, numbered |
  | `:cancel` | drop an unfinished multi-line entry |
  | `:reset` | forget every variable and function |
  | `:quit`, `:exit`, `:q` | leave (status 0); so do Ctrl-D and `exit(n)` |

- **History.** The last 1000 entries are kept. `!!` runs the last one again and
  `!N` runs entry `N` of `:history`. The line editor does not use the arrow keys:
  they are swallowed, not interpreted.
- **Keys.** Enter ends a line. Backspace erases a character (whole UTF-8
  characters). Ctrl-U erases the line. Ctrl-C abandons the line and any unfinished
  entry. Ctrl-D on an empty line leaves. A line is at most 1 MiB.
- **Batch mode.** `rhai -q` reads plain lines with no prompt and no echo, and
  prints only results, which makes it a pipeline filter:

```bash
printf '1+2\nlet x=5;\nx*2\n' | rhai -q
3
10
```

In batch mode a line that is not valid UTF-8 is reported and skipped, and a
line longer than 1 MiB is discarded. The session ends at end of input, at
`:quit`, or at `exit(n)`.

## The `os` module

Each function exists as `os::name(...)` and as a plain `name(...)`, so both
`os::read("f")` and `read("f")` work. Failures are ordinary catchable errors
(strings such as `os::read: /nope: No such file or directory (os error 2)`),
except `exit`.

| Function | Returns | Notes |
|---|---|---|
| `os::args()` | array of strings | the script's arguments: everything after the script name (or after `-e EXPR`) |
| `os::env(key)` | string or `()` | one environment variable; `()` when unset or not UTF-8 |
| `os::env()` | map | the whole environment (entries that are not UTF-8 are left out) |
| `os::exit()`, `os::exit(n)` | never | ends the script with status 0 or `n` (modulo 256); `try` cannot catch it |
| `os::clock()` | float | seconds since the command started (a monotonic clock) |
| `os::sleep(ms)` | `()` | blocks for `ms` milliseconds; negative is an error, more than 3,600,000 is an error |
| `os::read(path)` | string | the whole file as text; an error when it is longer than `--max-io` or not UTF-8 |
| `os::write(path, text)` | `()` | creates or truncates the file and writes the text; there is no append |
| `os::ls(path)` | array of maps | sorted by name; each map has `name`, `size` (bytes) and `kind` (`file`, `dir`, `symlink` or `other`) |
| `os::stdin_text()` | string | all of standard input as text; an error when it is longer than `--max-io` or not UTF-8; a second call returns an empty string |

```rhai
let args = os::args();
if args.len() == 0 {
    print("usage: rhai ls.rhai DIR");
    exit(2);
}
for e in ls(args[0]) {
    print(e.kind + "\t" + e.size + "\t" + e.name);
}
```

On a LazyOS session `HOME` and `USER` are set to the account's home and name, and
`PATH` names `/system/bin`, so `os::env("HOME")` and `os::env("USER")` tell a
script whose session it runs in. The file functions use the ordinary paths and
permissions of the user running the command: a script cannot read what you cannot
read. They create no directories and remove nothing; use the shell for that.

`print` and `debug` are not `os` functions but they are how output leaves a
script: `print` to standard output, `debug` to standard error (see
[language](language.md), section "Output").

## In `sh` pipelines

Standard input and output make `rhai` a filter. Use `print` for what the next
program should read, and keep diagnostics on `debug` or the exit status.

```bash
echo hi | rhai -e 'print(stdin_text().to_upper())'
HI
echo 3 | rhai -e 'let t = stdin_text(); t.trim(); print(parse_int(t) * 14)'
42
rhai -e 'for i in 1..4 { print(i) }' | tr '\n' +
1+2+3+
rhai -e 'for i in 0..100000 { print(i) }' | head -1
0
```

Notes:

- `t.trim()` changes `t` in place, so call it as a statement
  ([language](language.md), section "Strings").
- When the program on the right of the pipe exits, the next `print` stops the
  script quietly with status 0, so `| head` does not leave a message.
- A script that needs standard input must read it all with `stdin_text()` once; it
  is not available line by line.
- With `rhai -` the script itself is read from standard input, so such a script
  cannot also use `stdin_text()` for data.

A script file can be written by the shell and run at once:

```bash
cat > /tmp/count.rhai <<'EOF'
let n = 0;
for line in stdin_text().split("\n") {
    if line != "" { n += 1; }
}
print(n);
EOF
printf 'a\nb\nc\n' | rhai /tmp/count.rhai
3
```

## Running scripts from the Terminal

The desktop Terminal is the user's shell. It runs BusyBox `sh` with `PATH` set to
`/system/bin`, so you type `rhai ...` as in the examples above. Your scripts live
wherever you can write, usually under `/home/<name>`; `/tmp` works for throwaway
ones.

What a script may do is decided by the label its process carries.

| Where it runs | Label | What follows |
|---|---|---|
| the desktop Terminal and anything you type in it | `tool:os.lazy.terminal` | everything you may do through Messenger, except the label-policy loader and running a child under another label; the network services need the Terminal's network declaration, which it has; the password, key and account services judge you as the session's user |
| the console shell of a console image | none | no label rules; the services judge the caller's user id and capabilities |
| an installed LazyRAD app | `app:<system_name>` | only the interfaces and topics its manifest declares ([LazyRAD packaging](../lazyrad/packaging.md)) |

Everything a service refuses comes back as an error carrying the errno name
(`EACCES`, `EPERM`, ...), which `try`/`catch` can handle; see
[msg](msg.md), section "Errors". The kernel enforces this, not `rhai`: a script has no
authority its process does not have. Nobody is root: you cannot write `sys/**`
keys, `/conf` or `/system`, or restart a service directly.

## Elevated actions

A change that needs an administrator goes through `elevd`. A script asks for one
**operation** from a fixed table, by name and with string arguments, and `xuid`
shows a trusted prompt that names what is about to happen; an administrator types
their name and password into it. The script cannot see or supply the password. The
call returns once the prompt is answered, and every request asks every time.

```bash
rhai -e 'msg::set_timeout(0); sys::elevd::request("service.restart", ["inputd"])'
```

`msg::set_timeout(0)` matters: a call waits 5 seconds by default, which is
shorter than a person typing a password. `0` waits for the answer.

| Operation | Arguments | Does |
|---|---|---|
| `pkg.install` | path of an `.lzp` | installs a package (never over a core app) |
| `pkg.update-core` | path | replaces a core app |
| `pkg.remove` | system name | removes an app |
| `conf.set` | path, kind, value | sets any `confd` key (`sys/**` and other users' keys); kind is `bool`, `i64`, `u64`, `str` or `bytes` (hex) |
| `conf.delete` | path | deletes a key |
| `conf.get`, `conf.list` | path, prefix | reads any key |
| `time.set` | UTC seconds | sets the wall clock |
| `account.create` | name, password, `admin` or `user` | creates an account |
| `account.delete` | name, `keep`, `archive` or `remove` | deletes an account and decides about its home |
| `account.admin` | name, `1` or `0` | grants or removes administrator rights |
| `account.password` | name, password | sets anyone's password |
| `power.policy` | key, value | sets `sys/power/<key>` |
| `net.config` | card, address, gateway, dns | static network settings of one card |
| `service.restart` | service name | restarts one of `inputd`, `audiod`, `sndd`, `netd`, `netdrv`, `usbd`, `devd`, `mountd`, `printd`, `clipboardd`, `mimed`, `healthd`, `sysmond` |
| `net.wifi.system` | `store`, name, passphrase or `delete`, name | machine-wide Wi-Fi secrets |

The reply is a map with `detail` and `values` (text from the operation). The
failures are errors you can catch:

| Errno in the message | Meaning |
|---|---|
| `EINVAL` | an unknown operation or arguments that do not fit it |
| `EPERM` | the caller is outside a login session |
| `EACCES` | no administrator approved it |
| `ECANCELED` | the prompt was cancelled |
| `ETIMEDOUT` | nobody answered the prompt in time |
| `EAGAIN` | too many cancelled or timed out prompts in a row, so asking is slowed for a while; or the performing service refused |
| `EBUSY` | you already have one request waiting |
| `ENODEV` | no display, so no prompt can be shown (a console image) |

```rhai
msg::set_timeout(0);
try {
    let r = sys::elevd::request("service.restart", ["mimed"]);
    print(`restarted: ${r.detail}`);
} catch (e) {
    print(`not restarted: ${e}`);
}
```

Section "Accounts and elevation" of the [cookbook](cookbook.md) has more
examples.

## On LazyOS and elsewhere

| | LazyOS | Any other system the program is built for |
|---|---|---|
| language, `os` functions, limits, REPL | yes | yes |
| `msg` and `sys::*` | yes (the command checks the kernel name before it talks to the fabric) | no: using them is a "module not found" error |
| `#!/bin/rhai` | yes | per that system |

`rhai` is part of every image built with the `rhai` command (`python
tools/run_demo.py` builds it before each image; `--no-rhai` leaves it out, and
the developer tools need it).

## See also

- [Scripting on LazyOS](index.md): the map of the scripting documentation
- [The Rhai language](language.md): types, errors, functions, limits
- [`msg`](msg.md): Messenger services, topics and serving from scripts
- [API reference](api/index.md): one page per `sys::` module, including [elevd](api/elevd.md)
- [Cookbook](cookbook.md): worked recipes
- [Accounts and elevation](../accounts-plan.md): the design behind `elevd` and the trusted prompt
- [The Terminal](../terminal-plan.md)
