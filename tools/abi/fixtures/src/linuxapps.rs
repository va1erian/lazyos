//! `linuxapps` — run the real Linux programs `tools/linuxapps/build.py`
//! builds (dash, lua, sqlite3, jq, ripgrep; `LAZYOS_LINUXAPPS=1` puts them in
//! `/system/bin`) and check what each prints. One boot, one row per program:
//! each check prints `ABI:<program>:PASS` or `ABI:<program>:FAIL:<reason>`,
//! and `tools/abi/run.py` classifies the rows from the same log. The fixture
//! itself ends with `ABI:linuxapps:PASS` when every program passed.
//!
//! Each check exercises what that kind of program leans on: dash forks,
//! pipes, `$(...)` and `wait`s; lua does file I/O and string formatting;
//! sqlite3 creates, locks, writes and syncs a database on `/tmp`; jq reads a
//! pipe; rg walks a tree with worker threads (shared descriptor tables) and
//! writes through a pipe.

mod common;

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

/// Where the image puts the programs (`fhs::bin`).
const BIN: &str = "/system/bin";
const SCRATCH: &str = "/tmp/linuxapps";

/// Run `program args`, feeding `stdin`; `(status ok, stdout, stderr)`.
fn run(
    program: &str,
    args: &[&str],
    stdin: Option<&str>,
) -> Result<(bool, String, String), String> {
    let mut child = Command::new(format!("{BIN}/{program}"))
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn: {error}"))?;
    if let Some(input) = stdin {
        let mut pipe = child.stdin.take().ok_or("no stdin pipe")?;
        pipe.write_all(input.as_bytes())
            .map_err(|error| format!("stdin: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait: {error}"))?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// Run and require success with exactly `want` on stdout.
fn expect(program: &str, args: &[&str], stdin: Option<&str>, want: &str) -> Result<(), String> {
    let (ok, out, err) = run(program, args, stdin)?;
    if !ok {
        return Err(format!("exit failure, stderr {:?}", err.trim()));
    }
    if out != want {
        return Err(format!("got {:?} want {:?}", out, want));
    }
    Ok(())
}

fn dash() -> Result<(), String> {
    // Arithmetic, a loop, a function, command substitution, a pipeline into
    // a BusyBox applet, a background job and `wait`, and a subshell's status.
    let script = "i=0; while [ $i -lt 200 ]; do i=$((i+1)); done; \
        f() { echo \"fn:$1\"; }; f $i; echo \"sub:$(echo inner)\"; \
        echo pipeline | tr a-z A-Z; \
        (exit 3); echo \"status:$?\"; \
        sleep 0 & wait $!; echo \"waited:$?\"; \
        case lazyos in lazy*) echo glob;; esac";
    expect(
        "dash",
        &["-c", script],
        None,
        "fn:200\nsub:inner\nPIPELINE\nstatus:3\nwaited:0\nglob\n",
    )?;
    // A script file with positional parameters.
    let path = format!("{SCRATCH}/args.sh");
    fs::write(
        &path,
        "for a in \"$@\"; do printf '[%s]' \"$a\"; done; echo\n",
    )
    .map_err(|error| format!("write script: {error}"))?;
    expect(
        "dash",
        &[&path, "one", "two words"],
        None,
        "[one][two words]\n",
    )
}

fn lua() -> Result<(), String> {
    let path = format!("{SCRATCH}/data.txt");
    let code = format!(
        "local f = assert(io.open('{path}', 'w')) \
         for i = 1, 5 do f:write(i * i, '\\n') end f:close() \
         local sum = 0 for line in io.lines('{path}') do sum = sum + tonumber(line) end \
         local t = {{}} for w in ('lazy os on lua'):gmatch('%a+') do t[#t + 1] = w:upper() end \
         print(sum, table.concat(t, '-'), string.format('%.4f', math.pi), os.time() > 0, _VERSION)"
    );
    expect(
        "lua",
        &["-e", &code],
        None,
        "55\tLAZY-OS-ON-LUA\t3.1416\ttrue\tLua 5.4\n",
    )
}

fn sqlite3() -> Result<(), String> {
    let db = format!("{SCRATCH}/test.db");
    let _ = fs::remove_file(&db);
    let create = "create table t(a integer, b text); \
        insert into t values (1, 'x'), (2, 'y'), (3, 'z'); \
        create index ti on t(b);";
    expect("sqlite3", &[&db, create], None, "")?;
    // A second process reopens the file: the data reached the disk.
    expect(
        "sqlite3",
        &[
            &db,
            "select sum(a), group_concat(b, ',') from t; pragma integrity_check;",
        ],
        None,
        "6|x,y,z\nok\n",
    )?;
    expect(
        "sqlite3",
        &[":memory:", "with recursive n(i) as (select 1 union all select i+1 from n where i < 100) select count(*), sum(i) from n;"],
        None,
        "100|5050\n",
    )
}

fn jq() -> Result<(), String> {
    let input = r#"{"items":[{"n":"a","v":1},{"n":"b","v":2},{"n":"c","v":3}],"meta":{"ok":true}}"#;
    expect(
        "jq",
        &[
            "-c",
            "{total: ([.items[].v] | add), names: [.items[].n], ok: .meta.ok}",
        ],
        Some(input),
        "{\"total\":6,\"names\":[\"a\",\"b\",\"c\"],\"ok\":true}\n",
    )?;
    expect(
        "jq",
        &["-n", "[range(5)] | map(. * 10) | @csv"],
        None,
        "\"0,10,20,30,40\"\n",
    )
}

fn rg() -> Result<(), String> {
    let tree = format!("{SCRATCH}/tree");
    fs::create_dir_all(format!("{tree}/sub/deeper")).map_err(|error| format!("mkdir: {error}"))?;
    for (name, body) in [
        ("a.txt", "alpha\nneedle one\nomega\n"),
        ("sub/b.rs", "fn main() {}\n// needle two\n"),
        ("sub/deeper/c.md", "nothing here\n"),
        ("sub/deeper/d.txt", "needle three\nneedle four\n"),
    ] {
        fs::write(format!("{tree}/{name}"), body)
            .map_err(|error| format!("write {name}: {error}"))?;
    }
    let (ok, out, err) = run(
        "rg",
        &["--no-heading", "-n", "--sort", "path", "needle", &tree],
        None,
    )?;
    if !ok {
        return Err(format!("search failed: {:?}", err.trim()));
    }
    let want = format!(
        "{tree}/a.txt:2:needle one\n{tree}/sub/b.rs:2:// needle two\n\
         {tree}/sub/deeper/d.txt:1:needle three\n{tree}/sub/deeper/d.txt:2:needle four\n"
    );
    if out != want {
        return Err(format!("search got {out:?}"));
    }
    // Several worker threads on the parallel walker, counting per file.
    let (ok, out, _) = run("rg", &["-c", "-j", "4", "needle", &tree], None)?;
    let mut counts: Vec<&str> = out.lines().collect();
    counts.sort_unstable();
    if !ok || counts.len() != 3 {
        return Err(format!("parallel count got {out:?}"));
    }
    // Reading a pipe: stdin is searched when it is not a terminal.
    expect("rg", &["-o", "[0-9]+"], Some("a1b22c333\n"), "1\n22\n333\n")
}

fn main() {
    if let Err(error) = fs::create_dir_all(SCRATCH) {
        common::fail("linuxapps", &format!("scratch dir: {error}"));
    }
    let checks: [(&str, fn() -> Result<(), String>); 5] = [
        ("dash", dash),
        ("lua", lua),
        ("sqlite3", sqlite3),
        ("jq", jq),
        ("rg", rg),
    ];
    let mut failed = Vec::new();
    for (name, check) in checks {
        match check() {
            Ok(()) => common::pass(name),
            Err(reason) => {
                println!("ABI:{name}:FAIL:{}", reason.replace('\n', "\\n"));
                failed.push(name);
            }
        }
    }
    if failed.is_empty() {
        common::pass("linuxapps");
    } else {
        common::fail("linuxapps", &failed.join(","));
    }
}
