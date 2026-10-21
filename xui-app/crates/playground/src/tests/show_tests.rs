use rhai::{Array, Dynamic, Engine, Map};

use super::zero_lab;
use crate::prelude;
use crate::show::{blob_line, same, to_rhai, tree, tree_lines};
use crate::testkit::{cases, seed, Rng};

const NL: &str = "\n";

fn eval(engine: &Engine, text: &str) -> Dynamic {
    engine
        .eval::<Dynamic>(text)
        .unwrap_or_else(|e| panic!("`{text}` does not evaluate: {e}"))
}

fn random_string(rng: &mut Rng) -> String {
    const PARTS: &[&str] = &[
        "a",
        "Z",
        "0",
        " ",
        "_",
        "-",
        "/",
        "\"",
        "\\",
        "\n",
        "\t",
        "\r",
        "\u{0}",
        "\u{7f}",
        "\u{e9}",
        "\u{4e2d}",
        "\u{1f600}",
        "${x}",
        "`",
        "'",
        "#{",
        "//",
    ];
    (0..rng.below(8)).map(|_| *rng.pick(PARTS)).collect()
}

fn random_float(rng: &mut Rng) -> f64 {
    match rng.below(10) {
        0 => f64::NAN,
        1 => f64::INFINITY,
        2 => f64::NEG_INFINITY,
        3 => -0.0,
        4 => f64::MIN_POSITIVE,
        5 => 1e300,
        6 => (rng.below(2000) as f64 - 1000.0) / 8.0,
        7 => f64::from_bits(rng.next()),
        8 => 0.1,
        _ => 123456789.125,
    }
}

fn random_key(rng: &mut Rng) -> String {
    match rng.below(4) {
        0 => "key".to_string(),
        1 => format!("k{}", rng.below(100)),
        2 => "has space".to_string(),
        _ => random_string(rng),
    }
}

/// A value of a type the codec produces.
fn random_value(rng: &mut Rng, depth: u32) -> Dynamic {
    let leaf = depth == 0 || rng.chance(3);
    if leaf {
        return match rng.below(6) {
            0 => Dynamic::UNIT,
            1 => Dynamic::from_bool(rng.chance(2)),
            2 => Dynamic::from_int(match rng.below(4) {
                0 => i64::MIN,
                1 => i64::MAX,
                2 => rng.next() as i64,
                _ => rng.below(100) as i64 - 50,
            }),
            3 => Dynamic::from_float(random_float(rng)),
            4 => Dynamic::from(random_string(rng)),
            _ => {
                let len = rng.below(50) as usize;
                let bytes = if rng.chance(2) {
                    rng.bytes(len)
                } else {
                    (0..len).map(|_| 0x20 + rng.below(0x5f) as u8).collect()
                };
                Dynamic::from_blob(bytes)
            }
        };
    }
    if rng.chance(2) {
        let items: Array = (0..rng.below(4))
            .map(|_| random_value(rng, depth - 1))
            .collect();
        Dynamic::from_array(items)
    } else {
        let mut map = Map::new();
        for _ in 0..rng.below(4) {
            map.insert(random_key(rng).into(), random_value(rng, depth - 1));
        }
        Dynamic::from_map(map)
    }
}

#[test]
fn the_tree_reads_like_this() {
    let engine = Engine::new();
    let value = eval(
        &engine,
        r#"#{ path: "sys/x", deleted: false, items: ["a", 2, 1.5], none: (), blob: "hi".to_blob(), nested: #{} }"#,
    );
    assert_eq!(
        tree(&value),
        [
            "map(6)",
            "  blob: blob(2) 68 69",
            "  deleted: false",
            "  items: array(3)",
            "    [0]: \"a\"",
            "    [1]: 2",
            "    [2]: 1.5",
            "  nested: map(0)",
            "  none: ()",
            "  path: \"sys/x\"",
        ]
        .join("\n")
    );
    assert_eq!(tree(&Dynamic::from_int(5)), "5");
    assert_eq!(tree(&Dynamic::from("a\"b".to_string())), "\"a\\\"b\"");
    assert_eq!(tree_lines(&Dynamic::UNIT), ["()"]);
    assert_eq!(tree(&eval(&engine, "[]")), "array(0)");
}

#[test]
fn long_blobs_are_cut_and_deep_values_stop() {
    assert_eq!(blob_line(&[1, 255]), "blob(2) 01 ff");
    let long = blob_line(&[7; 40]);
    assert!(long.starts_with("blob(40) 07 07"));
    assert!(long.ends_with(" ..."));
    assert_eq!(long.matches("07").count(), 32);

    let mut value = Dynamic::from_int(1);
    for _ in 0..30 {
        value = Dynamic::from_array(vec![value]);
    }
    let lines = tree_lines(&value);
    assert_eq!(lines.len(), 22);
    assert_eq!(lines.last().unwrap().trim(), "...");
}

#[test]
fn copy_as_rhai_evaluates_back_to_an_equal_value() {
    let engine = Engine::new();
    let mut rng = Rng(seed(0x5ca1_ab1e));
    for _ in 0..cases(4_000) {
        let value = random_value(&mut rng, 4);
        let text = to_rhai(&value);
        let back = eval(&engine, &text);
        assert!(
            same(&value, &back),
            "{text}\n  was {value:?}\n  got {back:?}"
        );
        // Idempotent: the text of the copy is the text.
        assert_eq!(to_rhai(&back), text);
    }
}

#[test]
fn copy_as_rhai_special_cases() {
    let engine = Engine::new();
    for (value, want) in [
        (Dynamic::from_int(i64::MIN), "(-9223372036854775807 - 1)"),
        (Dynamic::from_float(f64::NAN), "(0.0 / 0.0)"),
        (Dynamic::from_float(f64::NEG_INFINITY), "(-1.0 / 0.0)"),
        (Dynamic::from_blob(vec![]), "blob()"),
        (
            Dynamic::from_blob(b"hi \"x\"".to_vec()),
            "\"hi \\\"x\\\"\".to_blob()",
        ),
        (Dynamic::UNIT, "()"),
    ] {
        let text = to_rhai(&value);
        assert_eq!(text, want);
        assert!(same(&value, &eval(&engine, &text)));
    }
    let binary = to_rhai(&Dynamic::from_blob(vec![0, 1, 255]));
    assert!(binary.contains("[0, 1, 255]"));
    assert!(same(
        &Dynamic::from_blob(vec![0, 1, 255]),
        &eval(&engine, &binary)
    ));
    // Keys that are not identifiers are quoted; reserved words too.
    let mut map = Map::new();
    map.insert("has space".into(), Dynamic::from_int(1));
    map.insert("fn".into(), Dynamic::from_int(2));
    map.insert("ok_1".into(), Dynamic::from_int(3));
    // Rhai refuses `_0` and `__` as bare names but takes `_a0`.
    map.insert("_0".into(), Dynamic::from_int(4));
    map.insert("__".into(), Dynamic::from_int(5));
    map.insert("_a0".into(), Dynamic::from_int(6));
    let value = Dynamic::from_map(map);
    let text = to_rhai(&value);
    assert_eq!(
        text,
        "#{ \"_0\": 4, \"__\": 5, _a0: 6, \"fn\": 2, \"has space\": 1, ok_1: 3 }"
    );
    assert!(same(&value, &eval(&engine, &text)));
}

#[test]
fn wide_values_break_over_lines_and_still_evaluate() {
    let engine = Engine::new();
    let mut map = Map::new();
    for i in 0..12 {
        map.insert(
            format!("field_{i}").into(),
            Dynamic::from("some longer text".to_string()),
        );
    }
    let value = Dynamic::from_map(map);
    let text = to_rhai(&value);
    assert!(text.lines().count() > 5, "{text}");
    assert!(same(&value, &eval(&engine, &text)));
}

#[test]
fn same_distinguishes_what_it_should() {
    assert!(same(&Dynamic::from_int(1), &Dynamic::from_int(1)));
    assert!(!same(&Dynamic::from_int(1), &Dynamic::from_int(2)));
    assert!(!same(&Dynamic::from_int(1), &Dynamic::from_float(1.0)));
    assert!(!same(&Dynamic::UNIT, &Dynamic::from_int(0)));
    assert!(same(
        &Dynamic::from_float(f64::NAN),
        &Dynamic::from_float(f64::NAN)
    ));
    assert!(!same(
        &Dynamic::from("a".to_string()),
        &Dynamic::from("b".to_string())
    ));
}

#[test]
fn the_prelude_prints_the_same_tree_as_the_rust_side() {
    let lab = zero_lab();
    let mut rng = Rng(seed(0xdec0_ded0));
    let engine = Engine::new();
    for _ in 0..cases(600) {
        let value = random_value(&mut rng, 4);
        let text = to_rhai(&value);
        let script = format!("{}pg_show({text});\n", prelude::SOURCE);
        let ran = lab.run(&script);
        ran.ok();
        // A key or string may hold a newline: compare the text, not the lines.
        // (The test's line splitting also drops a carriage return.)
        let expected = tree_lines(&eval(&engine, &text))
            .join(NL)
            .replace("\r\n", NL);
        assert_eq!(ran.lines.join(NL), expected, "value {text}");
    }
}

#[test]
fn the_prelude_stops_at_the_same_depth() {
    let lab = zero_lab();
    let build = "let v = 1; for i in 0..30 { v = [v]; }";
    let ran = lab.run(&format!(
        "{}{build}
pg_show(v);
",
        prelude::SOURCE
    ));
    ran.ok();
    let engine = Engine::new();
    let value = eval(&engine, "1");
    let mut nested = value;
    for _ in 0..30 {
        nested = Dynamic::from_array(vec![nested]);
    }
    assert_eq!(ran.lines, tree_lines(&nested));
    assert_eq!(ran.lines.last().unwrap().trim(), "...");
}

#[test]
fn the_prelude_counts_its_own_lines() {
    assert_eq!(prelude::lines(), prelude::SOURCE.lines().count());
    assert!(prelude::SOURCE.ends_with('\n'));
    assert!(prelude::wanted("pg_show(x)"));
    assert!(!prelude::wanted("print(x)"));
}
