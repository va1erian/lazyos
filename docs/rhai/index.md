# Scripting on LazyOS

Rhai is the scripting language of LazyOS. You use it to automate the desktop and
the system without writing Rust: a one-line command in the Terminal, a script that
reacts to settings or devices, a tool you call from the shell, the logic behind a
LazyRAD form, even a small service of your own. The same language and the same
calls work in all of those, because every service of the system is scriptable
through Messenger. This page says where Rhai is used, how to find your way in the
scripting documentation, and shows a first script.

## Where Rhai is used

| Where | What it is | Start here |
|---|---|---|
| the `rhai` command | an interpreter you run from the Terminal or the console: a REPL, `rhai -e '1 + 2'`, `rhai script.rhai`, scripts with a `#!/bin/rhai` line | [command](command.md) |
| LazyRAD forms | the event handlers of a form designed in the LazyRAD IDE are Rhai, with the same `msg` and `sys::*` calls; installed apps get only what their manifest declares | [LazyRAD](../lazyrad/index.md) |
| session scripts | the screenshot and test harness types `rhai -e '...'` into the Terminal to poke the system (set a key, post a toast, launch an app) | `tools/screenshot/examples` |
| services written in Rhai | `msg::serve` answers calls to any interface from a script | [cookbook](cookbook.md) |

Everything a script does reaches the system through Messenger: it calls services
(`confd` for settings, `notify` for toasts, `mimed` to open files, `init` to launch
apps, `pkgd` for packages, `elevd` for administrator actions, ...), listens to
topics, or serves a name. A script has the authority of the process it runs in and
no more: the Terminal's scripts act as you, an installed app's scripts only reach
what the app's manifest lists, and nothing is root.

## The documentation

| Page | Read it for |
|---|---|
| [The Rhai language](language.md) | a compact tour of the language as it behaves here: types, strings, arrays and maps, functions and closures, errors, modules, engine limits, and the surprises |
| [The `rhai` command](command.md) | usage, every option, the REPL, exit codes, the `os` module (arguments, files, environment, stdin), pipelines and elevated actions |
| [`msg`](msg.md) | how a script calls services, listens to topics, serves interfaces and handles errors |
| [API reference](api/index.md) | one page per `sys::` module, listing every function with its arguments, reply and documentation |
| [Cookbook](cookbook.md) | about seventy worked recipes: settings, notifications, apps and files, logs, system and network information, accounts and elevation, topics, services, discovery, error handling, command-line tools, long-running scripts |
| [LazyRAD](../lazyrad/index.md) and [its scripting](../lazyrad/scripting.md) | forms and the controls a script drives |
| [Rhai plan](../rhai-plan.md) | the design and the history of how Rhai came to LazyOS |

New to Rhai: read the first half of the language tour, then run the first script
below, then pick recipes from the cookbook. Looking for one call: start from the
API reference, where each service has its own page.

## A first script

Save this as `hello.rhai` in the Terminal (for example with `cat > hello.rhai`)
and run `rhai hello.rhai Ada`:

```rhai
// hello.rhai: rhai hello.rhai [name]
let name = if os::args().len() > 0 { os::args()[0] } else { os::env("USER") };
print(`hello ${name}`);

let theme = sys::confd::get("sys/ui/theme");
print(`the desktop theme is ${if theme == () { "not set" } else { theme.str_value }}`);

let id = sys::notify::notify("Info", "Hello", `Hi ${name}, from a script`, (), [], (), (), ());
print(`posted toast ${id}`);
print(`${msg::services().len()} Messenger services are running`);
```

It prints a greeting, reads a setting from `confd`, shows a toast in the corner of
the desktop and counts the services Messenger knows. Try the pieces one at a time
in the REPL:

```text
$ rhai
rhai REPL: :help, :quit or Ctrl-D
rhai> sys::confd::get("sys/ui/theme")
rhai> msg::describe("os.lazy.notify.v1")
rhai> :quit
```

`msg::describe(name)` returns an interface's documentation and methods as text (the REPL prints it), which is
the quickest way to find out how to call something; the [API
reference](api/index.md) has the same information with examples.

## What to know first

- A script file prints nothing unless it calls `print`; `rhai -e EXPR` prints the
  value of the expression.
- Functions in Rhai do not see the variables of the script around them, many string
  and array methods change their receiver in place, and `try` is a statement. The
  [language tour](language.md) lists these in one table.
- Services refuse what the caller may not do. The refusal is an error you can
  `try`/`catch`, with the errno name in the text ([msg](msg.md), section "Errors").
- A call waits at most five seconds for its reply by default (`msg::set_timeout`).
  A call that waits for a person, such as an administrator's approval through
  `elevd`, needs `msg::set_timeout(0)`.
- The `msg` and `sys` modules exist only on LazyOS. The language, the `os`
  functions and the REPL work wherever the `rhai` program is built.

## See also

- [The Rhai language](language.md)
- [The `rhai` command](command.md)
- [`msg`: Messenger from Rhai](msg.md)
- [Cookbook](cookbook.md)
- [Messenger](../messenger.md): the fabric every call goes through
- [MIDL](../midl.md): the interface language the API reference is generated from
