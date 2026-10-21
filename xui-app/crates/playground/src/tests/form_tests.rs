use rhai::{Dynamic, Engine};
use rhai_lazy::msg::codec;
use rhai_lazy::msg::schema::{self, Interface, Method, Ty};

use crate::form::{BytesMode, Control, Form};
use crate::form_parse::{format_hex, int_literal, int_range, parse_float, parse_hex, parse_int};
use crate::show;
use crate::snippet::{self, CallOptions};
use crate::testkit::{cases, seed, Rng};

fn eval(engine: &Engine, text: &str) -> Dynamic {
    engine
        .eval::<Dynamic>(text)
        .unwrap_or_else(|e| panic!("`{text}` does not evaluate: {e}"))
}

#[test]
fn integers_parse_with_prefixes_signs_and_ranges() {
    assert_eq!(parse_int("42", 0, 100), Ok(42));
    assert_eq!(parse_int("  -7 ", -10, 10), Ok(-7));
    assert_eq!(parse_int("+7", 0, 10), Ok(7));
    assert_eq!(parse_int("0xff", 0, 255), Ok(255));
    assert_eq!(parse_int("0b101", 0, 255), Ok(5));
    assert_eq!(parse_int("0o17", 0, 255), Ok(15));
    assert_eq!(parse_int("1_000", 0, 5000), Ok(1000));
    assert!(parse_int("", 0, 1).unwrap_err().contains("enter a number"));
    assert!(parse_int("abc", 0, 1).is_err());
    assert!(parse_int("0x", 0, 1).is_err());
    assert!(parse_int("1.5", 0, 10).is_err());
    assert!(parse_int("256", 0, 255)
        .unwrap_err()
        .contains("out of range"));
    assert!(parse_int("-1", 0, 255)
        .unwrap_err()
        .contains("out of range"));
    assert!(parse_int(&"9".repeat(60), 0, 255)
        .unwrap_err()
        .contains("too large"));
    assert_eq!(int_range(Ty::U32), Some((0, u32::MAX as i128)));
    assert_eq!(int_range(Ty::String), None);
}

#[test]
fn u64_above_i64_max_is_written_as_its_bit_pattern() {
    let (min, max) = int_range(Ty::U64).unwrap();
    let big = parse_int("18446744073709551615", min, max).unwrap();
    assert_eq!(int_literal(big), "-1");
    assert_eq!(int_literal(i64::MIN as i128), "(-9223372036854775807 - 1)");
    assert_eq!(int_literal(5), "5");
    assert!(parse_int("18446744073709551616", min, max).is_err());
}

#[test]
fn floats_and_hex_parse() {
    assert_eq!(parse_float("1.5"), Ok(1.5));
    assert_eq!(parse_float("1e3"), Ok(1000.0));
    assert!(parse_float("inf").unwrap().is_infinite());
    assert!(parse_float("nan").unwrap().is_nan());
    assert!(parse_float("").is_err());
    assert!(parse_float("x").is_err());
    assert_eq!(parse_hex("de ad BE:ef"), Ok(vec![0xde, 0xad, 0xbe, 0xef]));
    assert_eq!(parse_hex("0x0102"), Ok(vec![1, 2]));
    assert_eq!(parse_hex(""), Ok(vec![]));
    assert!(parse_hex("abc").unwrap_err().contains("pairs"));
    assert!(parse_hex("zz").unwrap_err().contains("hex digit"));
    assert_eq!(format_hex(&[1, 255]), "01 ff");
}

fn random_text(rng: &mut Rng) -> String {
    const ALPHABET: &[&str] = &[
        "0",
        "1",
        "7",
        "9",
        "a",
        "f",
        "x",
        "b",
        "o",
        "-",
        "+",
        "_",
        ".",
        "e",
        "E",
        " ",
        ",",
        ":",
        "\u{e9}",
        "\u{1f600}",
        "inf",
        "nan",
        "0x",
        "0b",
        "\"",
        "\\",
        "\n",
        "\t",
        "\u{0}",
        "z",
    ];
    (0..rng.below(14)).map(|_| *rng.pick(ALPHABET)).collect()
}

#[test]
fn text_parsing_never_panics_and_accepted_values_round_trip() {
    let mut rng = Rng(seed(0x5eed_f0f0));
    for _ in 0..cases(40_000) {
        let text = random_text(&mut rng);
        for ty in [Ty::I32, Ty::U32, Ty::I64, Ty::U64] {
            let (min, max) = int_range(ty).unwrap();
            if let Ok(value) = parse_int(&text, min, max) {
                assert!((min..=max).contains(&value));
                assert_eq!(
                    parse_int(&value.to_string(), min, max),
                    Ok(value),
                    "{text:?}"
                );
            }
        }
        if let Ok(f) = parse_float(&text) {
            let again = parse_float(&f.to_string()).unwrap();
            assert!(
                f.to_bits() == again.to_bits() || (f.is_nan() && again.is_nan()),
                "{text:?}"
            );
        }
        if let Ok(bytes) = parse_hex(&text) {
            assert_eq!(parse_hex(&format_hex(&bytes)), Ok(bytes), "{text:?}");
        }
    }
}

/// Every method of the table that a script can call.
fn callable() -> Vec<(&'static Interface, &'static Method)> {
    schema::interfaces()
        .iter()
        .flat_map(|i| i.methods.iter().map(move |m| (i, m)))
        .filter(|(_, m)| m.objects.is_empty())
        .collect()
}

#[test]
fn a_default_form_of_every_method_is_sendable_and_the_codec_takes_it() {
    let engine = Engine::new();
    let mut checked = 0;
    for (interface, method) in callable() {
        let form = Form::for_method(interface, method);
        assert_eq!(form.blocked(), None, "{}.{}", interface.name, method.name);
        let texts = form
            .argument_texts()
            .unwrap_or_else(|e| panic!("{}.{}: {e}", interface.name, method.name));
        let values: Vec<Dynamic> = texts.iter().map(|t| eval(&engine, t)).collect();
        codec::encode_positional(interface, method.params, &values)
            .unwrap_or_else(|e| panic!("{}.{}: {e}", interface.name, method.name));
        checked += 1;
    }
    assert!(checked > 200, "{checked}");
}

#[test]
fn methods_that_carry_objects_have_disabled_fields_with_the_reason() {
    let mut found = 0;
    for interface in schema::interfaces() {
        for method in interface.methods.iter().filter(|m| !m.objects.is_empty()) {
            let form = Form::for_method(interface, method);
            let reason = form
                .blocked()
                .unwrap_or_else(|| panic!("{}.{}", interface.name, method.name));
            assert!(reason.contains("kernel object"), "{reason}");
            assert!(form.argument_texts().is_err());
            found += 1;
        }
    }
    assert!(found > 3);
}

/// Change the control to something a person might type.
fn mutate(rng: &mut Rng, control: &mut Control) {
    match control {
        Control::Text(text) => *text = random_text(rng),
        Control::Bool(b) => *b = rng.chance(2),
        Control::Int { text, .. } => {
            *text = match rng.below(5) {
                0 => random_text(rng),
                1 => rng.next().to_string(),
                2 => format!("-{}", rng.next() >> rng.below(60)),
                3 => format!("{:#x}", rng.next() >> rng.below(60)),
                _ => (rng.below(300) as i64 - 100).to_string(),
            }
        }
        Control::Float(text) => {
            *text = match rng.below(4) {
                0 => random_text(rng),
                1 => format!("{}", f64::from_bits(rng.next())),
                2 => format!("{}.{}", rng.below(1000), rng.below(1000)),
                _ => "1e300".to_string(),
            }
        }
        Control::Bytes { text, mode } => {
            *mode = if rng.chance(2) {
                BytesMode::Hex
            } else {
                BytesMode::Utf8
            };
            *text = match mode {
                BytesMode::Hex if rng.chance(3) => random_text(rng),
                BytesMode::Hex => {
                    let n = rng.below(6) as usize;
                    format_hex(&rng.bytes(n))
                }
                BytesMode::Utf8 => random_text(rng),
            };
        }
        Control::Enum {
            variants, selected, ..
        } => *selected = rng.below(variants.len() as u64) as usize,
        Control::Option { on, inner } => {
            *on = rng.chance(2);
            mutate(rng, inner);
        }
        Control::Array { items, .. } => {
            items.clear();
            for _ in 0..rng.below(3) {
                control_row(rng, control);
            }
        }
        Control::Group(fields) => {
            for field in fields {
                mutate(rng, &mut field.control);
            }
        }
        Control::Disabled { .. } => {}
    }
}

fn control_row(rng: &mut Rng, control: &mut Control) {
    control.add_row();
    if let Control::Array { items, .. } = control {
        if let Some(last) = items.last_mut() {
            mutate(rng, last);
        }
    }
}

#[test]
fn whatever_a_form_accepts_evaluates_and_encodes() {
    let engine = Engine::new();
    let mut rng = Rng(seed(0xf0a4_2024));
    let methods = callable();
    let mut accepted = 0u64;
    let mut refused = 0u64;
    for _ in 0..cases(6_000) {
        let (interface, method) = *rng.pick(&methods);
        let mut form = Form::for_method(interface, method);
        for field in &mut form.fields {
            mutate(&mut rng, &mut field.control);
        }
        match form.argument_texts() {
            Ok(texts) => {
                let values: Vec<Dynamic> = texts.iter().map(|t| eval(&engine, t)).collect();
                codec::encode_positional(interface, method.params, &values).unwrap_or_else(|e| {
                    panic!(
                        "{}.{} accepted by the form, refused by the codec: {e}\n{texts:?}",
                        interface.name, method.name
                    )
                });
                accepted += 1;
            }
            Err(e) => {
                assert!(!e.message.is_empty());
                refused += 1;
            }
        }
    }
    assert!(
        accepted > 500 && refused > 100,
        "accepted {accepted}, refused {refused}"
    );
}

#[test]
fn form_text_is_canonical_after_a_round_trip_through_the_engine() {
    let engine = Engine::new();
    let mut rng = Rng(seed(0xc0de_cafe));
    let methods = callable();
    let mut rounds = 0;
    for _ in 0..cases(3_000) {
        let (interface, method) = *rng.pick(&methods);
        let mut form = Form::for_method(interface, method);
        for field in &mut form.fields {
            mutate(&mut rng, &mut field.control);
        }
        let Ok(texts) = form.argument_texts() else {
            continue;
        };
        for (field, text) in form.fields.iter().zip(&texts) {
            let value = eval(&engine, text);
            let back = Control::from_value(interface, field.ty, &value);
            let again = back.to_rhai().unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(
                &again, text,
                "{}.{} {}",
                interface.name, method.name, field.name
            );
            // The text is the "copy as Rhai" of the value, too.
            assert!(show::same(&value, &eval(&engine, &again)));
        }
        rounds += 1;
    }
    assert!(rounds > 500, "{rounds}");
}

#[test]
fn a_filled_form_makes_a_call_that_compiles() {
    let confd = schema::interface("os.lazy.confd.v1").unwrap();
    let set = confd.method("Set").unwrap();
    let mut form = Form::for_method(confd, set);
    form.fields[0].control = Control::Text("sys/test/x".into());
    if let Control::Group(fields) = &mut form.fields[1].control {
        for f in fields.iter_mut() {
            match f.name.as_str() {
                "kind" => {
                    f.control = Control::Int {
                        text: "3".into(),
                        min: 0,
                        max: u32::MAX.into(),
                    }
                }
                "str_value" => {
                    f.control = Control::Option {
                        on: true,
                        inner: Box::new(Control::Text("hi".into())),
                    }
                }
                _ => {}
            }
        }
    } else {
        panic!("value is a group");
    }
    let texts = form.argument_texts().unwrap();
    assert_eq!(texts[0], "\"sys/test/x\"");
    assert!(texts[1].contains("kind: 3"), "{}", texts[1]);
    assert!(texts[1].contains("str_value: \"hi\""), "{}", texts[1]);
    let call = snippet::call_with(confd, set, &CallOptions::default(), &texts).unwrap();
    let lab = super::zero_lab();
    lab.compile(&call.text).unwrap();
    lab.run(&call.text).ok();
}

#[test]
fn the_topic_forms_are_structs_enums_or_raw_text() {
    let timed = schema::interface("os.lazy.timed.v1").unwrap();
    let mut form = Form::for_topic(timed, &timed.topics[0]);
    assert!(!form.bare);
    assert_eq!(form.fields.len(), 3);
    if let Control::Int { text, .. } = &mut form.fields[0].control {
        *text = "1700000000".into();
    }
    let payload = form.payload_text().unwrap();
    assert!(payload.contains("unix: 1700000000"), "{payload}");
    let engine = Engine::new();
    let value = eval(&engine, &payload);
    codec::encode_payload(timed, "Tick", &value).unwrap();

    let raw = Form::raw(timed);
    assert!(raw.bare);
    assert_eq!(raw.payload_text().unwrap(), "\"\"");

    // An enum topic, if the table has one.
    for interface in schema::interfaces() {
        for topic in interface.topics {
            if interface.find_enum(topic.payload).is_some() {
                let form = Form::for_topic(interface, topic);
                assert!(form.bare);
                let value = eval(&engine, &form.payload_text().unwrap());
                codec::encode_payload(interface, topic.payload, &value).unwrap();
            }
        }
    }
}

#[test]
fn every_declared_topic_form_encodes() {
    let engine = Engine::new();
    for interface in schema::interfaces() {
        for topic in interface.topics {
            let form = Form::for_topic(interface, topic);
            let value = eval(
                &engine,
                &form
                    .payload_text()
                    .unwrap_or_else(|e| panic!("{}: {e}", topic.pattern)),
            );
            codec::encode_payload(interface, topic.payload, &value)
                .unwrap_or_else(|e| panic!("{} {}: {e}", interface.name, topic.pattern));
        }
    }
}

#[test]
fn options_arrays_and_rows_behave() {
    let registry = schema::interface("os.lazy.messenger.registry.v1").unwrap();
    let register = registry.method("Register").unwrap();
    let mut form = Form::for_method(registry, register);
    // interfaces: Array<U64>
    let interfaces = form
        .fields
        .iter_mut()
        .find(|f| f.name == "interfaces")
        .unwrap();
    interfaces.control.add_row();
    interfaces.control.add_row();
    if let Control::Array { items, .. } = &mut interfaces.control {
        items[1] = Control::Int {
            text: "0xff".into(),
            min: i64::MIN.into(),
            max: u64::MAX.into(),
        };
    }
    interfaces.control.remove_row(0);
    interfaces.control.remove_row(9);
    let texts = form.argument_texts().unwrap();
    let at = form
        .fields
        .iter()
        .position(|f| f.name == "interfaces")
        .unwrap();
    assert_eq!(texts[at], "[255]");
    let endpoint = form
        .fields
        .iter()
        .position(|f| f.name == "endpoint")
        .unwrap();
    assert_eq!(texts[endpoint], "()");
    // A bad row names its place.
    let interfaces = form
        .fields
        .iter_mut()
        .find(|f| f.name == "interfaces")
        .unwrap();
    if let Control::Array { items, .. } = &mut interfaces.control {
        items[0] = Control::Int {
            text: "nope".into(),
            min: 0,
            max: 1,
        };
    }
    let e = form.argument_texts().unwrap_err();
    assert_eq!(e.path, "interfaces[0]");
    assert!(e.to_string().starts_with("interfaces[0]: "));
}

#[test]
fn filling_a_form_from_a_map_sets_its_controls() {
    let echo = schema::interface("os.lazy.echo.v1").unwrap();
    let mut form = Form::for_method(echo, echo.method("Echo").unwrap());
    let engine = Engine::new();
    let map = eval(&engine, "#{ text: \"ahoy\", count: 3 }").cast::<rhai::Map>();
    form.fill_from(&map);
    assert_eq!(form.argument_texts().unwrap(), ["\"ahoy\"", "3"]);
}
