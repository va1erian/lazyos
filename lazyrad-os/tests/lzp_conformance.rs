//! LazyRAD-produced `.lzp` packages against LazyOS's own reader and builder.
//!
//! `lazyrad-packager` tests verify every package with an independent verifier;
//! this suite closes the loop with the real thing: every package is re-opened
//! with `lazypkg::Package::open` (the reader `pkgd` uses), and the same tree is
//! fed to `tools/pkg/build.py` (the host builder), which must accept it and
//! produce a package with identical contents.
//!
//! Projects: the directories in `LAZYRAD_SAMPLES` (the same path list the image
//! build uses) when set, plus generated ones. Needs `libs/lazypkg` (PR #431) and
//! `python` (the builder cross-check is skipped with a note when absent).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use lazypkg::Package;
use lazyrad_os::platform::{Home, LazyOsPlatform};
use lazyrad_packager::lzp::{build_package, BuiltPackage, HostPermissions, PackageRequest};
use lazyrad_runtime::platform::Platform;

fn fake_player(extra: usize) -> Vec<u8> {
    let mut elf = vec![0u8; 64 + extra];
    elf[..4].copy_from_slice(b"\x7fELF");
    elf[4] = 2;
    elf[5] = 1;
    elf[18..20].copy_from_slice(&0x3Eu16.to_le_bytes());
    elf
}

/// The real player when `tools/lazyrad/build.py` has built it (`target/lazyrad/lrplay.elf`),
/// else a synthetic ELF. The real one exercises deflate on a multi-megabyte entry.
fn player() -> Vec<u8> {
    let built = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/lazyrad/lrplay.elf");
    fs::read(built).unwrap_or_else(|_| fake_player(50_000))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lazyrad-os-lzp-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A generated project with `modules` script files; returns its `.lrp`.
fn generated(dir: &Path, name: &str, modules: usize, storage: bool) -> PathBuf {
    let mut lrp = format!("name = \"{name}\"\nversion = \"2.1\"\nstartup = \"m0\"\n");
    for n in 0..modules {
        lrp.push_str(&format!(
            "\n[[items]]\nkind = \"module\"\nname = \"m{n}\"\ncode = \"m{n}.rhai\"\n"
        ));
        let body = if storage && n == 0 {
            "fn save() { file_write_text(\"n.txt\", \"x\"); }\n".to_owned()
        } else {
            format!("fn f{n}() {{ {n} }}\n")
        };
        fs::write(dir.join(format!("m{n}.rhai")), body).unwrap();
    }
    let path = dir.join(format!("{name}.lrp"));
    fs::write(&path, lrp).unwrap();
    path
}

/// The `.lrp` files of the sample directories named by `LAZYRAD_SAMPLES`.
fn sample_projects() -> Vec<PathBuf> {
    let Some(list) = std::env::var_os("LAZYRAD_SAMPLES") else {
        return Vec::new();
    };
    std::env::split_paths(&list)
        .map(|dir| {
            // A configured sample that cannot be read must fail the run, not
            // silently shrink it to the generated projects.
            let entries = fs::read_dir(&dir)
                .unwrap_or_else(|e| panic!("LAZYRAD_SAMPLES entry {}: {e}", dir.display()));
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|x| x == "lrp"))
                .unwrap_or_else(|| panic!("LAZYRAD_SAMPLES entry {} has no .lrp", dir.display()))
        })
        .collect()
}

/// A fresh subdirectory of `dir`, so generated projects never share files.
fn subdir(dir: &Path, name: &str) -> PathBuf {
    let sub = dir.join(name);
    fs::create_dir_all(&sub).unwrap();
    sub
}

fn package(lrp: &Path, player: &[u8], author: &str) -> BuiltPackage {
    build_package(&PackageRequest {
        project: lrp,
        player,
        author,
        system_name: None,
        description: Some("conformance"),
        icons: None,
        check: None,
        permissions: Some(&lazyos_permissions),
    })
    .unwrap_or_else(|e| panic!("{}: {e}", lrp.display()))
}

/// What Make LazyOS App asks: the LazyOS platform's derivation from the
/// scripts (`rhai_lazy::msg::permissions`).
fn lazyos_permissions(scripts: &[&str]) -> HostPermissions {
    let found = LazyOsPlatform::ide(
        Home::from_var(Some(OsStr::new("/home/user"))),
        Path::new("/apps/os.lazy.lazyrad/0.1.0-abcd1234/bin/lazyrad.elf"),
    )
    .script_permissions(scripts);
    HostPermissions {
        interfaces: found.interfaces,
        topics: found.topics,
    }
}

/// Opens `built` with the LazyOS reader and checks it says what we wrote.
fn open_with_lazypkg(built: &BuiltPackage) -> BTreeMap<String, Vec<u8>> {
    let pkg = Package::open(&built.bytes).unwrap_or_else(|e| panic!("lazypkg refused: {e}"));
    let manifest = pkg.manifest();
    assert_eq!(manifest.app.system_name, built.system_name);
    assert_eq!(manifest.app.version, built.version);
    assert_eq!(manifest.entry.binary, "bin/lrplay.elf");
    assert_eq!(manifest.entry.args, ["--project", "resources/project"]);
    assert!(manifest.permissions.network.is_empty());
    let digest = pkg.digest();
    assert!(
        pkg.install_dir()
            .starts_with(&format!("{}/{}-", built.system_name, built.version)),
        "install dir {}",
        pkg.install_dir()
    );
    assert_eq!(pkg.digest(), digest, "the digest is stable");
    let mut files = BTreeMap::new();
    for entry in pkg.entries() {
        if !entry.is_dir {
            files.insert(
                entry.name.to_owned(),
                pkg.read(entry.name).expect("entry reads"),
            );
        }
    }
    assert_eq!(files.len(), built.entries);
    files
}

#[test]
fn every_project_is_accepted_by_lazypkg() {
    let player = player();
    let dir = scratch("reader");
    let mut projects = sample_projects();
    let plain = generated(&subdir(&dir, "plain"), "plain", 3, false);
    let storage = generated(&subdir(&dir, "stores"), "stores", 2, true);
    projects.extend([plain, storage]);
    for lrp in &projects {
        let built = package(lrp, &player, "Ada Lovelace");
        let files = open_with_lazypkg(&built);
        assert_eq!(files["bin/lrplay.elf"], player);
        let pkg = Package::open(&built.bytes).unwrap();
        if lrp.ends_with("stores.lrp") {
            // The app's own folder in the running user's home
            // (`$HOME/.apps/<id>`), never inside `/apps` (filesystem plan F4),
            // spelled with `$HOME` as the F5 grammar requires (#509).
            let id = &pkg.manifest().app.system_name;
            let data = fhs::app_data_dir(lazypkg::HOME_VAR, id);
            assert_eq!(
                pkg.manifest().permissions.files,
                [format!("read:{data}"), format!("write:{data}")]
            );
        }
    }
}

#[test]
fn derived_messenger_permissions_pass_the_lazyos_reader() {
    let dir = scratch("messenger");
    let sub = subdir(&dir, "watcher");
    let lrp = generated(&sub, "watcher", 2, false);
    fs::write(
        sub.join("m0.rhai"),
        r#"fn form_load() {
    let t = sys::confd::get("sys/ui/theme");
    sys::confd::on_changed("sys/ui/#", |e| ());
    msg::publish("app/user.conformance.watcher/hello", "hi");
}
fn save() { file_write_text("n.txt", "x"); }
"#,
    )
    .unwrap();
    let built = package(&lrp, &fake_player(0), "Conformance");
    open_with_lazypkg(&built);
    let pkg = Package::open(&built.bytes).unwrap();
    let permissions = &pkg.manifest().permissions;
    assert_eq!(
        permissions.interfaces,
        ["os.lazy.display.v1", "os.lazy.confd.v1", "os.lazy.input.v1"]
    );
    assert_eq!(
        permissions.topics,
        [
            "publish:app/user.conformance.watcher/hello",
            "subscribe:system/confd/changed/#",
        ]
    );
    // Next to the derived Messenger rules, the storage rule keeps the F5
    // grammar: `$HOME` as the first segment only, never an absolute home
    // (lazypkg's REJECT_ABSOLUTE_HOME already refused anything else above).
    let data = fhs::app_data_dir(lazypkg::HOME_VAR, &pkg.manifest().app.system_name);
    assert_eq!(
        permissions.files,
        [format!("read:{data}"), format!("write:{data}")]
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn lazypkg_refuses_what_the_packager_refuses() {
    // The packager's own limit checks mirror the reader: a package at exactly
    // MAX_ENTRIES opens; both sides agree on the boundary.
    let dir = scratch("edge");
    let lrp = generated(&dir, "edge", lazypkg::MAX_ENTRIES - 6, false);
    let built = package(&lrp, &fake_player(0), "Ada");
    assert_eq!(built.entries, lazypkg::MAX_ENTRIES);
    Package::open(&built.bytes).expect("a package at MAX_ENTRIES opens");
}

fn python() -> Option<String> {
    ["python", "python3", "py"]
        .iter()
        .find(|cmd| {
            Command::new(cmd)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
        .map(|cmd| (*cmd).to_owned())
}

#[test]
fn tools_pkg_build_accepts_the_same_tree() {
    let Some(python) = python() else {
        eprintln!("skipping: no python on PATH");
        return;
    };
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let builder = repo.join("tools/pkg/build.py");
    if !builder.is_file() {
        eprintln!("skipping: tools/pkg/build.py is not in this checkout (PR #431)");
        return;
    }
    let dir = scratch("builder");
    let mut projects = sample_projects();
    projects.push(generated(&dir, "viapy", 4, true));
    for lrp in &projects {
        let built = package(lrp, &fake_player(2_000), "Ada");
        let ours = open_with_lazypkg(&built);

        // Extract the package to a source tree exactly as `build.py` expects.
        let tree = scratch(&format!("tree-{}", built.system_name));
        for (name, data) in &ours {
            let path = tree.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, data).unwrap();
        }
        let out = tree.join("dist");
        let run = Command::new(&python)
            .arg(&builder)
            .arg(&tree)
            .arg("--out")
            .arg(&out)
            .output()
            .expect("python runs");
        assert!(
            run.status.success(),
            "build.py refused {}:\n{}{}",
            built.system_name,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        let rebuilt = fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|x| x == "lzp"))
            .expect("build.py wrote a package");
        assert_eq!(
            rebuilt.file_name().unwrap().to_string_lossy(),
            built.file_name(),
            "both builders name the file alike"
        );
        let theirs_bytes = fs::read(&rebuilt).unwrap();
        let theirs = Package::open(&theirs_bytes).expect("lazypkg opens build.py's output");
        let mut names: Vec<&str> = theirs.entries().map(|e| e.name).collect();
        let mut ours_names: Vec<&str> = ours.keys().map(String::as_str).collect();
        names.sort_unstable();
        ours_names.sort_unstable();
        assert_eq!(names, ours_names, "the same entries");
        for (name, data) in &ours {
            assert_eq!(&theirs.read(name).unwrap(), data, "{name}");
        }
    }
}

/// Builds and opens many packages with the real reader.
#[test]
fn soak_lazypkg_opens_every_package() {
    let dir = scratch("soak");
    for round in 0..120usize {
        let project_dir = dir.join(format!("p{round}"));
        fs::create_dir_all(&project_dir).unwrap();
        let lrp = generated(
            &project_dir,
            &format!("soak{round}"),
            1 + round % 30,
            round % 4 == 0,
        );
        let built = package(
            &lrp,
            &fake_player(round * 517 % 40_000),
            &format!("Author {}", round % 7),
        );
        let files = open_with_lazypkg(&built);
        assert!(files.contains_key("manifest.toml"));
        fs::remove_dir_all(&project_dir).unwrap();
    }
}
