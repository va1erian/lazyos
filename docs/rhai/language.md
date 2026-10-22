# The Rhai language on LazyOS

LazyOS scripts are written in Rhai (rhai.rs), a small embedded
language with a JavaScript and Rust flavoured syntax. This page is a tour of the
language as it behaves in the `rhai` command (Rhai 1.26.1, default `std`
features, no language feature removed; LazyRAD forms use the same language), with the places where it surprises
people who know another language. Every snippet was run, so you can paste it
into `rhai` and get the output shown.

Try the examples with `rhai -e '...'` for one expression, or start the REPL
with a bare `rhai` ([command](command.md)). The `msg` and `sys` modules that
talk to LazyOS services are covered in [msg](msg.md) and the
[API reference](api/index.md); this page is only the language.

## Values and types

| Type | `type_of` | Literal | Notes |
|---|---|---|---|
| integer | `i64` | `42`, `1_000`, `0xff`, `0b101`, `0o17` | always 64-bit signed; overflow is an error, not a wrap |
| float | `f64` | `3.5`, `1e3`, `2.0` | |
| string | `string` | `"text"`, `` `text ${x}` `` | Unicode; `len()` counts characters |
| character | `char` | `'a'`, `'\n'` | |
| boolean | `bool` | `true`, `false` | |
| unit | `()` | `()` | "nothing": a missing value, and what most statements return |
| array | `array` | `[1, "a", 2.5]` | any mix of types |
| object map | `map` | `#{ name: "x", "odd key": 1 }` | string keys, kept in sorted key order |
| blob | `blob` | `blob(4)`, `"ab".to_blob()` | a byte array |
| range | `range`, `range=` | `1..4`, `1..=4` | |
| function pointer | `Fn` | `\|x\| x + 1`, `Fn("name")` | |

```rhai
print(type_of(7));          // i64
print(type_of(7.0));        // f64
print(type_of("s"));        // string
print(type_of(()));         // ()
print(type_of([1]));        // array
print(type_of(#{}));        // map
print(type_of(1..3));       // range
print(type_of(|x| x));      // Fn
```

There are no `i64::MAX` style constants and no `is` operator; compare
`type_of(x) == "string"` instead.

## Variables and constants

```rhai
let x = 5;          // a variable
x += 1;
let x = x * 2;      // shadowing: a new variable of the same name
const LIMIT = 100;  // a constant; assigning to it is a compile error
{
    let x = "inner";    // a block has its own scope
    print(x);
}
print(x);           // 12
```

`let` without a value gives `()`. Statements end with `;`, and the last
expression of a block, a function or a script is its value, so `if`, `switch`
and blocks work as expressions:

```rhai
let size = if 3 > 2 { "big" } else { "small" };
print(size);        // big
```

## Operators

| Group | Operators |
|---|---|
| arithmetic | `+ - * / % **` |
| comparison | `== != < <= > >=` |
| logic | `&& \|\| !` (short-circuit) |
| bits | `& \| ^ << >>` |
| assignment | `= += -= *= /= %= **= <<= >>= &= \|= ^=` |
| membership | `x in array`, `key in map`, `"sub" in "string"`, `n in 1..5` |
| null handling | `a ?? b` (b when a is `()`), `map.a?.b` (stops at `()`) |

Numbers behave as in Rust, with these rules worth knowing:

- Integer division truncates toward zero: `7 / 2` is `3` and `-7 / 2` is `-3`.
  Make one side a float for a real quotient: `7.0 / 2` or `7.to_float() / 2` is `3.5`.
- `int op float` works for arithmetic and comparison (`1 + 1.5` is `2.5`, `5 == 5.0`
  is `true`), but not everywhere: `1 << 2.0` and indexing with a float are errors.
- Overflow and division by zero raise errors (`9223372036854775807 + 1` fails
  with "Addition overflow"; `1 / 0` fails; `1.0 / 0.0` is `inf`).
- `+` with a string on either side concatenates, whatever the other side is:
  `5 + "5"` is `"55"`. `()` and `bool` have no `+` with numbers.
- Convert explicitly: `to_float()`, `to_int()`, `to_string()`, `parse_int("42")`,
  `parse_float("1.5")`, `to_hex()`; `parse_int` of bad text is an error you can catch.

```rhai
print(7 / 2);                   // 3
print(7.0 / 2);                 // 3.5
print(2 ** 10);                 // 1024
print(5 + "5");                 // 55
print(parse_int("42") + 1);     // 43
print(3.7.to_int());            // 3
print(2.5.round());             // 3.0
print(25.0.sqrt());             // 5.0
```

## Strings

Strings are Unicode. `len()` is the number of characters, and `bytes()` the
number of UTF-8 bytes. Indexing gives a character (negative counts from the
end) and a range gives a sub-string:

```rhai
let name = "LazyOS";
let n = 3;
print(`hello ${name}, ${n * 2} items`);   // hello LazyOS, 6 items

let s = "h\u00e9llo w\u00f6rld";   // written with unicode escapes
print(s.len());                 // 11
print(s[1]);                    // the letter e with an acute accent
print(s[-1]);                   // d
print(s[0..3]);                 // three characters
print(s.to_upper());            // the same text in capitals
print(s.contains("w\u00f6r"));   // true
print(s.index_of("w"));         // 6 (and -1 when absent)
print("a,b,,c".split(","));     // ["a", "b", "", "c"]
print("key=val".split("=", 2)); // ["key", "val"]
```

Text in backticks is a *template*: `${expr}` is replaced by the value of the
expression, and a template may span lines. Plain `"..."` strings understand
`\n`, `\t`, `\"`, `\\`, `\xNN` and `\uNNNN`; **a template does not**, it keeps a backslash
as it is (`` `a\tb` `` prints `a\tb`). For a tab or a newline next to a value, add a plain
string: `` `${name}` + "\t" + `${size}` ``.

**Many string methods change the string in place and return `()`.** This is the
most common way to get an empty result:

| Changes the string, returns `()` | Returns a new string |
|---|---|
| `trim()`, `replace(a, b)`, `remove(x)`, `crop(n)`, `pad(n, ch)`, `make_upper()`, `make_lower()`, `truncate(n)`, `clear()` | `to_upper()`, `to_lower()`, `sub_string(start, len)`, `s[a..b]`, `+`, `to_string()` |

```rhai
let s = "  Hello  ";
let t = s.to_string();
t.trim();                       // changes t in place
print(`[${t}] [${s}]`);         // [Hello] [  Hello  ]

let path = "a-b-c";
path.replace("-", "/");
print(path);                    // a/b/c
print("abc".to_upper() + "!");  // ABC!
```

There is no `join` for arrays and no `repeat` for strings. Write the helper
once:

```rhai
fn join(items, sep) {
    let out = "";
    let first = true;
    for item in items {
        if !first { out += sep; }
        out += item;
        first = false;
    }
    out
}
print(join(["a", "b", "c"], ", "));   // a, b, c
```

## Arrays

```rhai
let a = [3, 1, 2];
a.push(9);
a += [7, 8];                    // append another array
print(a.len());                 // 6
print(a[0] + a[-1]);            // 11
a.sort();                       // in place: [1, 2, 3, 7, 8, 9]
a.reverse();
print(a.map(|x| x * 2));        // [18, 16, 14, 6, 4, 2]
print(a.filter(|x| x > 3));     // [9, 8, 7]
print(a.reduce(|sum, x| sum + x, 0));   // 30
print(a.some(|x| x == 9));      // true
print(a.all(|x| x > 0));        // true
print(a.contains(7));           // true
print(a.index_of(7));           // 2
print(a.find(|x| x < 5));       // 3
print(a.extract(1, 2));         // [8, 7]
a.sort(|x, y| if x < y { 1 } else if x > y { -1 } else { 0 });   // custom order
print(a.pop());                 // removes and returns the last element
print(a.shift());               // removes and returns the first one
```

- Out-of-range `a[5]` is an error; `a.get(5)` returns `()` instead.
- Arrays are values: `let b = a;` copies, so changing `b` never changes `a`,
  and passing an array to a function passes a copy (see "Functions").
- There is no `sum`, `min`, `max`, `zip`, `chunks` or slicing by range
  (`a[1..3]` is an error): use `reduce`, a loop, or `extract(start, len)`.
- `for x in a`, and `for (x, i) in a` for the element and its index.
- `a.remove(i)`, `a.insert(i, x)`, `a.truncate(n)`, `a.clear()`, `a.dedup()`,
  `a.splice(range, other)`, `a.drain(|x| ...)` all change the array.

## Object maps

A map is a set of named values, the shape of a LazyOS struct such as a `confd`
value or a service reply:

```rhai
let m = #{ name: "ada", "odd key": 1 };
m.age = 36;                     // add a field
m["city"] = "Paris";            // same, with a computed key
print(m.name);                  // ada
print(m.nope);                  // prints an empty line: a missing field is () , not an error
print("age" in m);              // true
print(m.keys());                // ["age", "city", "name", "odd key"]
print(m.values().len());        // 4
m.remove("odd key");
m += #{ extra: true };          // merge another map
print(m.len());                 // 4

let outer = #{ list: [10, 20] };
print(outer.list[1]);           // 20
print(outer?.missing?.deeper);  // () : `?.` stops at the first ()

for k in m.keys() {
    print(`${k} = ${m[k]}`);
}
print(#{ a: 1 }.to_json());     // {"a":1}
print(parse_json("{\"a\": [1, 2]}").a[1]);   // 2
```

Keys are always strings and iterate in sorted order. `for (v, k) in map` does
not work; loop over `m.keys()`. `to_json()` exists on maps only (not on arrays
or strings); `parse_json(text)` makes a map or array from JSON text.

## Blobs

A blob is an array of bytes, which is what services use for addresses and
payloads (`Bytes` in an IDL file):

```rhai
let b = blob();
b += 10;                        // append a byte
b += 0;
b += 2;
b += 2;
print(b);                       // [0a000202]
print(b[0]);                    // 10
print(b.len());                 // 4
print("AB".to_blob());          // [4142]
print("AB".to_blob().as_string());   // AB
```

## Ranges and loops

```rhai
for i in 0..3 { print(i); }         // 0 1 2
for i in 0..=3 { print(i); }        // 0 1 2 3
for ch in "ab".to_chars() { print(ch); }

let i = 0;
while i < 3 {
    i += 1;
    if i == 2 { continue; }
    print(`i=${i}`);                // i=1 i=3
}

let found = loop {                  // loop is an expression: break gives its value
    i += 1;
    if i > 5 { break i * 10; }
};
print(found);                       // 60

do { i -= 1; } while i > 0;
do { i += 1; } until i >= 2;
```

A range has `start` and `end` and works with `in`, but it has no `map` or
`to_array`: iterate it with `for`. A `for` loop over a large range still counts
operations against `--max-ops`.

## Branching: `if` and `switch`

```rhai
fn kind(x) {
    switch x {
        1 => "one",
        2 | 3 => "few",
        4..=9 => "several",
        "s" => "string",
        () => "unit",
        _ => "other"
    }
}
print([kind(1), kind(3), kind(7), kind("s"), kind(()), kind(50)]);
// ["one", "few", "several", "string", "unit", "other"]

let sc = 7;
let r = switch sc { 7 if sc > 5 => "seven and big", 7 => "seven", _ => "no" };
print(r);                           // seven and big
```

A `switch` arm may be a value, several values with `|`, a range, a literal array
or map, or `()`. A guard (`if ...`) goes after a value, never after the default
`_`.

## Functions

```rhai
fn fact(n) {
    if n <= 1 { 1 } else { n * fact(n - 1) }
}
print(fact(10));                    // 3628800

fn greet(name) { `hi ${name}` }
fn greet(name, punct) { `hi ${name}${punct}` }   // overloaded by argument count
print(greet("a"));                  // hi a
print(greet("a", "!"));             // hi a!
```

The rules that differ from most languages:

- **A function cannot see variables or constants of the script around it.**
  It sees only its parameters, its own locals, other functions and imported
  modules. Pass what it needs as arguments.
- **Functions are defined at the top level** of a script (not inside blocks or
  other functions), and can be used before their definition.
- **Arguments are passed by value.** A function that pushes onto its array
  parameter changes only its copy. Return the result, or use the method form.
- **Method call syntax** `x.f(a)` calls `f(x, a)`, and inside the function
  `this` is the receiver, which can be changed.
- `return value;` leaves early; without it the last expression is the result.
- There are no default arguments; use overloads.

```rhai
let outer = 10;
fn bad() { outer + 1 }
try {
    bad();
} catch (e) {
    print(e.message);       // Variable not found: outer (line 2, position 12) ...
}
fn good(n) { n + 1 }
print(good(outer));         // 11

fn push_twice_copy(list, x) { list.push(x); list.push(x); }
let l = [];
push_twice_copy(l, 1);
print(l);                   // [] : the function changed its own copy

fn push_twice(x) { this.push(x); this.push(x); }
l.push_twice(2);
print(l);                   // [2, 2]

fn double() { this * 2 }
print(21.double());         // 42

fn first_even(list) {
    for x in list { if x % 2 == 0 { return x; } }
    ()
}
print(first_even([1, 3, 4, 5]));    // 4
```

## Closures and function pointers

An anonymous function `|a, b| expr` is a value. It *does* see the variables
around it, and shares them (a later change to the variable is seen by the
closure). Call one stored in a variable with `.call(...)`; `f(...)` would look
for a function named `f`:

```rhai
let base = 100;
let add_base = |x| x + base;
print(add_base.call(1));            // 101
base = 200;
print(add_base.call(1));            // 201

let counter = 0;
let inc = || { counter += 1; };
inc.call();
inc.call();
print(counter);                     // 2

fn apply(f, x) { f.call(x) }
fn square(n) { n * n }
print(apply(|v| v * 3, 5));         // 15
print(apply(Fn("square"), 5));      // 25
```

`Fn("name")` makes a pointer to a script function by name, `call(f, args...)`
is the same as `f.call(args...)`, `.curry(args...)` binds leading arguments,
and `is_def_fn("name", arity)` tests that a function exists. A closure stored
in a map field is a method and gets `this`:

```rhai
fn add3(a, b, c) { a + b + c }
let add_three_to_ten = Fn("add3").curry(1, 2);
print(add_three_to_ten.call(10));   // 13

let obj = #{ n: 3, get: || this.n };
print(obj.get());                   // 3

let ops = #{ add: |a, b| a + b, mul: |a, b| a * b };
for name in ops.keys() {
    print(`${name}: ${ops[name].call(3, 4)}`);
}
```

Handlers for topics (`msg::on`) and services (`msg::serve`) are closures or
function pointers of this kind ([msg](msg.md)).

## Errors: `throw`, `try` and `catch`

```rhai
try {
    throw "boom";
} catch (e) {
    print(`caught ${e}`);           // caught boom
}

try {
    throw #{ code: 7, why: "x" };   // any value can be thrown
} catch (e) {
    print(e.code);                  // 7
}

try {
    read("/definitely/not/here");   // a failed os:: call
} catch (e) {
    print(type_of(e));              // string
}

try {
    1 / 0;
} catch (e) {
    print(type_of(e));              // map
    print(e.error);                 // ErrorArithmetic
    print(e.message);               // Division by zero: 1 / 0
}
```

What `e` holds depends on who raised the error:

| Raised by | `e` is |
|---|---|
| your `throw` | exactly the value you threw |
| an `os::` function, or a service call (`msg`, `sys::*`) | a string, e.g. `os::read: /x: No such file or directory (os error 2)` or `os.lazy.confd.v1.Get: ... (EACCES, code 13)` |
| the language (bad index, missing function or variable, arithmetic, parse errors) | a map with `error` (the kind), `message`, `line` and `position` |

To handle both kinds in one place:

```rhai
fn describe(e) {
    if type_of(e) == "map" && "message" in e { e.message } else { e.to_string() }
}
try { [1][5]; } catch (e) { print(describe(e)); }
try { read("/nope"); } catch (e) { print(describe(e)); }
```

`try` is a statement: you cannot write `let x = try { ... } catch ...`, and there
is no `finally`. A `catch` without a variable is allowed (`catch { ... }`).
A re-throw is `throw e;`.

Some stops cannot be caught, because they are the host ending the script:
`exit(n)`, running out of operations (`Too many operations`), stack overflow,
and compile errors. They end the script with a message and an exit status
([command](command.md)).

## Output: `print` and `debug`

`print(x)` writes the value and a newline to standard output (a unit prints as
an empty line, strings without quotes, maps and arrays in `#{"a": 1}` and
`[1, "a"]` form). `debug(x)` writes to **standard error**, prefixed with the
position, and shows strings with quotes:

```bash
rhai -e 'print("out"); debug("err")' 2>/dev/null
out
```

`rhai -e EXPR` also prints the value of the whole expression, unless it is `()`;
a script file does not (only `print` shows anything). In the REPL values are
shown in the `debug` style, so a string has quotes.

## Modules and `import`

A script can load another file as a module. `import "util" as u;` finds
`util.rhai` relative to the **current directory of the process** (not the
script's), and the module's exported items are reached as `u::name`:

```rhai
// util.rhai
export const VERSION = "1.2";
let hidden = 41;
export let answer = hidden + 1;

fn shout(s) { s.to_upper() + "!" }
fn helper() { 7 }
fn uses_helper() { helper() * 2 }
private fn secret() { 1 }
```

```rhai
import "util" as u;
print(u::VERSION);                  // 1.2
print(u::answer);                   // 42
print(u::shout("hi"));              // HI!
print(u::uses_helper());            // 14
```

Functions of a module see the module's other functions, but not its
variables or constants (the same rule as everywhere); `private fn` is not
visible from outside. `rhai --sandbox` turns
`import` and `eval` off, so a script cannot load code it was not given
([command](command.md)). The `msg` and `sys` namespaces are always there and
need no import.

`eval("1 + 1")` compiles and runs a string as a script in the current scope
(unless `--sandbox`).

## Time and a few handy functions

```rhai
let t = timestamp();            // a monotonic timer
sleep(10);                      // milliseconds; see the os module
print(t.elapsed < 5.0);         // true : seconds since the timestamp
print(clock() >= 0.0);          // seconds since the command started
print(abs(-3) + max(3, 9) + min(3, 9));   // 15
```

`sleep`, `clock`, `read`, `write`, `ls`, `env`, `exit` and `stdin_text` come from
the `os` module, which is also reachable as `os::name`
([command](command.md)).

## Engine limits

Scripts run by the `rhai` command have limits that stop a runaway program, set
with the options below. A LazyRAD form runs in an engine the player builds
itself, which does not take these options.

| Limit | Default | Option | Error when exceeded |
|---|---|---|---|
| operations (one per evaluated step) | 10,000,000 | `--max-ops N` (`0` = unlimited) | `Too many operations` |
| nested function calls | 32 | `--max-call-levels N` (1 to 64) | `Stack overflow` |
| expression nesting (top level and in functions) | 64 and 32 | `--max-expr-depth N` (sets both) | a compile error |
| bytes in one string | 4 MiB | `--max-string N` (`0` = unlimited) | `Length of string too large` |
| elements in one array or blob | 100,000 | `--max-array N` (`0` = unlimited) | `Size of array/BLOB too large` |
| entries in one map | 100,000 | `--max-map N` (`0` = unlimited) | `Size of object map too large` |
| bytes one `read`, `stdin_text` or script file may load | 4 MiB | `--max-io N` (at least 1) | an error from the call |
| one `sleep(ms)` | 3,600,000 ms | none | `os::sleep: ... is longer than the ... limit` |

```bash
rhai --max-ops 1000 -e 'let i = 0; loop { i += 1; }'
rhai: Too many operations (line 1, position 24)
```

The call depth is small on purpose: the program's stack is 1 MiB. A recursive
algorithm that needs more should be a loop.

## What to remember

| Gotcha | What to do |
|---|---|
| a function cannot see script variables or constants | pass them as arguments |
| `trim`, `replace`, `sort`, `reverse`, `dedup`, `pad`, `crop` change in place | copy first with `to_string()` or `let b = a;`, and do not `print` their result |
| no `join`, `sum`, `min`/`max` of an array, `zip`, `repeat`, array slicing | a small helper function or `reduce` |
| `try` is a statement, there is no `finally` | set a variable inside the `try` and `catch` blocks |
| stored closures are called with `.call()` | `f.call(x)`, not `f(x)` |
| integer `/` truncates, `+` with a string concatenates | `to_float()`, `to_string()` where you mean it |
| `for (v, k) in map` is not valid | `for k in m.keys()` |
| engine errors are maps, service and `os` errors are strings | the `describe` helper above |
| `import` resolves against the current directory | run scripts from their own directory, or use a full path |

## See also

- [Scripting on LazyOS](index.md): what Rhai is used for here
- [The `rhai` command](command.md): modes, options, the `os` module, exit codes
- [`msg`](msg.md): calling Messenger services from scripts
- [Cookbook](cookbook.md): worked recipes
- [LazyRAD scripting](../lazyrad/scripting.md): the same language inside forms
