//! Grammar, compilation and the path judgement, against a reference matcher
//! written independently of the id scheme.

use std::collections::HashSet;

use fuzzkit::{for_seeds, Rng};

use crate::*;

fn ids(rules: &[&str]) -> HashSet<u64> {
    compile(rules).expect("rules compile").into_iter().collect()
}

fn ok(rules: &[&str], path: &str, access: Access) -> bool {
    let granted = ids(rules);
    allows(path, access, &|id| granted.contains(&id))
}

#[test]
fn grammar_accepts_what_the_manifest_grammar_accepts() {
    for rule in [
        "read:/system/share/fonts",
        "read:/system/share/fonts/*",
        "write:$HOME/.apps/os.lazy.webkit",
        "write:$HOME/.apps/os.lazy.webkit/*",
        "read:/etc/ssl/certs/ca-certificates.crt",
        "read:$HOME/Pictures",
    ] {
        assert!(parse(rule).is_ok(), "{rule}");
    }
    let rule = parse("write:$HOME/.apps/x/*").unwrap();
    assert_eq!(rule.grant, Grant::Write);
    assert!(rule.home);
    assert_eq!(rule.segments, [".apps", "x"]);
    assert_eq!(rule.subject(), "$HOME/.apps/x");
}

#[test]
fn grammar_refuses_what_it_cannot_confine() {
    let cases: &[(&str, RuleError)] = &[
        ("open:/a", RuleError::Shape),
        ("read:a/b", RuleError::Shape),
        ("read:$HOME", RuleError::Shape),
        ("read:$HOMEX/a", RuleError::Shape),
        ("read:/x/$HOME/y", RuleError::Shape),
        ("read:$HOME/$HOME/y", RuleError::Shape),
        ("read:/", RuleError::Root),
        ("read:/*", RuleError::Root),
        ("read:$HOME/*", RuleError::Root),
        ("read:/a/*/b", RuleError::Wildcard),
        ("read:/a/**", RuleError::Wildcard),
        ("read:/a/../b", RuleError::Segment),
        ("read:/a/./b", RuleError::Segment),
        ("read:/a//b", RuleError::Segment),
        ("read:/a/b c", RuleError::Segment),
        ("read:/home/user/x", RuleError::AbsoluteHome),
        ("write:/home", RuleError::AbsoluteHome),
    ];
    for (rule, want) in cases {
        assert_eq!(parse(rule), Err(*want), "{rule}");
    }
    let deep = format!("read:/{}", vec!["a"; MAX_SEGMENTS + 1].join("/"));
    assert_eq!(parse(&deep), Err(RuleError::TooLong));
    let long = format!("read:/{}", "a".repeat(MAX_PATH));
    assert_eq!(parse(&long), Err(RuleError::TooLong));
}

#[test]
fn compile_is_sorted_deduped_and_carries_the_marker() {
    let a = compile(&["read:/system/share/fonts", "read:/system/share/fonts/*"]).unwrap();
    let mut sorted = a.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(a, sorted);
    assert!(a.contains(&confine_id()));
    // read grant + trav for `/`, `/system`, `/system/share` + the marker
    assert_eq!(a.len(), 5);
    // Order and repetition of rules do not matter.
    let b = compile(&["read:/system/share/fonts/*"]).unwrap();
    assert_eq!(a, b);
    // No rules is still a confined (empty) label.
    assert_eq!(compile::<&str>(&[]).unwrap(), vec![confine_id()]);
    assert_eq!(compile(&["read:/a", "x"]), Err((1, RuleError::Shape)));
}

#[test]
fn a_grant_covers_its_subtree_and_nothing_else() {
    let rules = ["read:/system/share/fonts"];
    for path in [
        "/system/share/fonts",
        "/system/share/fonts/liberation",
        "/system/share/fonts/liberation/LiberationSans-Regular.ttf",
    ] {
        assert!(ok(&rules, path, Access::Read), "{path}");
        assert!(ok(&rules, path, Access::Stat), "{path}");
        assert!(!ok(&rules, path, Access::Write), "{path} write");
    }
    for path in [
        "/system/share/fontsX",
        "/system/share/font",
        "/system/share",
        "/system/bin/busybox",
        "/home/user/.ssh/id_rsa",
        "/conf/confd.db",
    ] {
        assert!(!ok(&rules, path, Access::Read), "{path}");
    }
}

#[test]
fn ancestors_can_be_looked_at_but_not_read() {
    let rules = ["read:/system/share/fonts"];
    for path in ["/", "/system", "/system/share"] {
        assert!(ok(&rules, path, Access::Stat), "{path}");
    }
    for path in ["/system", "/system/share"] {
        assert!(!ok(&rules, path, Access::Read), "{path} read");
        assert!(!ok(&rules, path, Access::Write), "{path} write");
    }
    // A sibling of an ancestor is not visible.
    assert!(!ok(&rules, "/system/bin", Access::Stat));
    assert!(!ok(&rules, "/system/etc", Access::Stat));
    // Looking at an ancestor does not extend to what is below it.
    assert!(!ok(&rules, "/system/share/other", Access::Stat));
}

#[test]
fn write_includes_read_and_stat() {
    let rules = ["write:/tmp/work"];
    for access in [Access::Read, Access::Stat, Access::Write] {
        assert!(ok(&rules, "/tmp/work/a/b", access));
        assert!(ok(&rules, "/tmp/work", access));
    }
    assert!(!ok(&rules, "/tmp", Access::Write));
    assert!(!ok(&rules, "/tmp/other", Access::Read));
}

#[test]
fn home_rules_follow_any_user_home_and_only_homes() {
    let rules = ["write:$HOME/.apps/os.lazy.webkit/*", "read:$HOME/Pictures"];
    for user in ["user", "admin", "x"] {
        let own = format!("/home/{user}/.apps/os.lazy.webkit/cookie.jar.db");
        assert!(ok(&rules, &own, Access::Write), "{own}");
        let pictures = format!("/home/{user}/Pictures/a.png");
        assert!(ok(&rules, &pictures, Access::Read));
        assert!(!ok(&rules, &pictures, Access::Write));
        let secret = format!("/home/{user}/.ssh/id_rsa");
        assert!(!ok(&rules, &secret, Access::Read), "{secret}");
        assert!(!ok(&rules, &secret, Access::Stat), "{secret}");
        let apps = format!("/home/{user}/.apps");
        assert!(ok(&rules, &apps, Access::Stat), "{apps}");
        assert!(!ok(&rules, &apps, Access::Read), "{apps}");
        let other_app = format!("/home/{user}/.apps/org.lazy.mail/db");
        assert!(!ok(&rules, &other_app, Access::Read), "{other_app}");
    }
    // `$HOME` is a home, not a place anywhere.
    assert!(!ok(&rules, "/tmp/.apps/os.lazy.webkit/x", Access::Read));
    assert!(!ok(
        &rules,
        "/srv/user/.apps/os.lazy.webkit/x",
        Access::Read
    ));
    assert!(ok(&rules, "/home", Access::Stat));
    assert!(!ok(&rules, "/home", Access::Read));
}

#[test]
fn the_baseline_is_fixed_and_read_only_apart_from_the_sinks() {
    let none: [&str; 0] = [];
    for path in ["/dev/null", "/dev/zero", "/dev/full"] {
        assert!(ok(&none, path, Access::Write), "{path}");
        assert!(ok(&none, path, Access::Read), "{path}");
    }
    for path in [
        "/dev/random",
        "/dev/urandom",
        "/proc/self/maps",
        "/proc/meminfo",
    ] {
        assert!(ok(&none, path, Access::Read), "{path}");
        assert!(!ok(&none, path, Access::Write), "{path} write");
    }
    for path in [
        "/dev/tty",
        "/dev/console",
        "/dev/ptmx",
        "/dev/pts/0",
        "/dev/shm/x",
        "/tmp",
    ] {
        assert!(!ok(&none, path, Access::Read), "{path}");
        assert!(!ok(&none, path, Access::Stat), "{path}");
    }
    assert!(ok(&none, "/", Access::Stat));
    assert!(!ok(&none, "/", Access::Read), "the root is not listed");
    assert!(!ok(&none, "/", Access::Write));
}

#[test]
fn malformed_paths_are_refused_before_any_rule_is_consulted() {
    let everything = |_: u64| true;
    for path in [
        "relative/path",
        "",
        "/a/../b",
        "/a/./b",
        "/a//b",
        "/a/b/",
        "/a\0b",
        "/system/share/fonts/../../../home/user/x",
    ] {
        assert!(!allows(path, Access::Read, &everything), "{path:?}");
    }
    let long = format!("/{}", "a".repeat(MAX_PATH_BYTES));
    assert!(!allows(&long, Access::Stat, &everything));
    // A deep path is fine while it stays under the limit.
    let deep = format!("/{}", vec!["d"; 200].join("/"));
    assert!(allows(&deep, Access::Read, &everything));
}

#[test]
fn rule_ids_are_stable_and_distinct_per_kind_and_subject() {
    // Pinned: the kernel and the package manager must derive the same ids, and
    // a change here silently un-grants every installed sandbox.
    assert_eq!(rule_id(Kind::Read, "/a"), rule_id(Kind::Read, "/a"));
    assert_ne!(rule_id(Kind::Read, "/a"), rule_id(Kind::Write, "/a"));
    assert_ne!(rule_id(Kind::Read, "/a"), rule_id(Kind::Trav, "/a"));
    assert_ne!(rule_id(Kind::Read, "/a"), rule_id(Kind::Read, "/b"));
    assert_ne!(rule_id(Kind::Read, "$HOME/a"), rule_id(Kind::Read, "/a"));
    assert_ne!(confine_id(), rule_id(Kind::Read, ""));
    assert_ne!(rule_id(Kind::Read, "/a"), u64::MAX);
}

/// A reference matcher that never hashes: the rules as segment lists.
struct Reference {
    rules: Vec<(Grant, bool, Vec<String>)>,
}

impl Reference {
    fn new(texts: &[String]) -> Reference {
        let rules = texts
            .iter()
            .map(|text| {
                let rule = parse(text).unwrap();
                (rule.grant, rule.home, rule.segments)
            })
            .collect();
        Reference { rules }
    }

    /// `path` below `prefix` (or equal), where a home prefix matches
    /// `/home/<any>/prefix...`.
    fn inside(path: &[&str], home: bool, prefix: &[String]) -> bool {
        let rest = if home {
            match path {
                [first, _, rest @ ..] if *first == "home" => rest,
                _ => return false,
            }
        } else {
            path
        };
        rest.len() >= prefix.len() && rest.iter().zip(prefix).all(|(a, b)| *a == b.as_str())
    }

    /// Whether `path` is a proper ancestor of the rule's path.
    fn ancestor_of(path: &[&str], home: bool, prefix: &[String]) -> bool {
        let mut full: Vec<&str> = Vec::new();
        if home {
            // `/` and `/home` and `/home/<any>` and below
            if path.is_empty() || path == ["home"] {
                return true;
            }
            match path {
                [first, _, rest @ ..] if *first == "home" => {
                    return rest.len() < prefix.len()
                        && rest.iter().zip(prefix).all(|(a, b)| *a == b.as_str());
                }
                _ => return false,
            }
        }
        full.extend(prefix.iter().map(String::as_str));
        path.len() < full.len() && path.iter().zip(&full).all(|(a, b)| a == b)
    }

    fn allows(&self, path: &str, access: Access) -> bool {
        let segments: Vec<&str> = if path == "/" {
            Vec::new()
        } else {
            path.strip_prefix('/').unwrap().split('/').collect()
        };
        if baseline(&segments, access) {
            return true;
        }
        for (grant, home, prefix) in &self.rules {
            let covers = Self::inside(&segments, *home, prefix);
            match access {
                Access::Write if covers && *grant == Grant::Write => return true,
                Access::Read | Access::Stat if covers => return true,
                Access::Stat if Self::ancestor_of(&segments, *home, prefix) => return true,
                _ => {}
            }
        }
        false
    }
}

fn random_rules(rng: &mut Rng, pool: &[&str]) -> Vec<String> {
    (0..rng.range(0, 5))
        .map(|_| {
            let depth = rng.range(1, 4);
            let segments: Vec<&str> = (0..depth).map(|_| *rng.pick(pool)).collect();
            let kind = if rng.one_in(2) { "read" } else { "write" };
            let base = if rng.one_in(3) { "$HOME" } else { "" };
            let star = if rng.one_in(4) { "/*" } else { "" };
            let rule = format!("{kind}:{base}/{}{star}", segments.join("/"));
            // `/home/...` as an absolute rule is refused; regenerate without it.
            if parse(&rule).is_err() {
                format!("{kind}:$HOME/{}{star}", segments.join("/"))
            } else {
                rule
            }
        })
        .collect()
}

fn random_path(rng: &mut Rng, pool: &[&str]) -> String {
    if rng.one_in(10) {
        return String::from("/");
    }
    let depth = rng.range(1, 6);
    let mut segments: Vec<&str> = Vec::new();
    if rng.one_in(3) {
        segments.push("home");
        segments.push(*rng.pick(&["u", "v", "w"]));
    }
    for _ in 0..depth {
        segments.push(*rng.pick(pool));
    }
    format!("/{}", segments.join("/"))
}

#[test]
fn judgement_equals_the_reference_matcher() {
    const POOL: &[&str] = &["a", "b", "c", "home", "x", ".apps", "dev", "proc", "null"];
    for_seeds("filepolicy_judgement", |_, rng| {
        let rules = random_rules(rng, POOL);
        let reference = Reference::new(&rules);
        let granted: HashSet<u64> = compile(&rules).unwrap().into_iter().collect();
        for _ in 0..40 {
            let path = random_path(rng, POOL);
            for access in [Access::Stat, Access::Read, Access::Write] {
                let got = allows(&path, access, &|id| granted.contains(&id));
                let want = reference.allows(&path, access);
                assert_eq!(got, want, "rules {rules:?} path {path} {access:?}");
            }
        }
    });
}

#[test]
fn more_rules_never_take_access_away() {
    const POOL: &[&str] = &["a", "b", "c", "x", ".apps"];
    for_seeds("filepolicy_monotone", |_, rng| {
        let rules = random_rules(rng, POOL);
        let extra = random_rules(rng, POOL);
        let mut more = rules.clone();
        more.extend(extra);
        let small: HashSet<u64> = compile(&rules).unwrap().into_iter().collect();
        let big: HashSet<u64> = compile(&more).unwrap().into_iter().collect();
        for _ in 0..40 {
            let path = random_path(rng, POOL);
            for access in [Access::Stat, Access::Read, Access::Write] {
                if allows(&path, access, &|id| small.contains(&id)) {
                    assert!(
                        allows(&path, access, &|id| big.contains(&id)),
                        "{path} {access:?} {rules:?} -> {more:?}"
                    );
                }
            }
        }
    });
}

#[test]
fn nothing_outside_the_rules_is_reachable_by_dot_dot_or_prefix_games() {
    // Whatever the grants, `allows` only ever sees normalised paths; the
    // library still refuses the unnormalised ones, and a name that merely
    // starts with a granted name is not inside it.
    let rules = ["write:/data/app", "read:$HOME/docs"];
    for path in [
        "/data/application",
        "/data/app2/x",
        "/data/ap",
        "/home/u/documents",
        "/home/u/docs2",
        "/home/docs",
        "/hom/u/docs",
    ] {
        assert!(!ok(&rules, path, Access::Read), "{path}");
    }
}
