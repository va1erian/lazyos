//! The engine's rules, one key sequence at a time.

use super::*;

/// The engine after `keys`, written as on the keypad: digits, `.`, `+-*/`,
/// `=`, `%`, `n` (±), `<` (backspace), `c` (C/AC), `a` (AC); spaces are
/// ignored.
fn run(keys: &str) -> Engine {
    let mut engine = Engine::new();
    for key in keys.chars() {
        let press = match key {
            '0'..='9' => Press::Digit(key as u8 - b'0'),
            '.' => Press::Point,
            '+' => Press::Op(Op::Add),
            '-' => Press::Op(Op::Sub),
            '*' => Press::Op(Op::Mul),
            '/' => Press::Op(Op::Div),
            '=' => Press::Equals,
            '%' => Press::Percent,
            'n' => Press::Negate,
            '<' => Press::Backspace,
            'c' => Press::Clear,
            'a' => Press::AllClear,
            ' ' => continue,
            other => panic!("no key {other:?}"),
        };
        engine.press(press);
    }
    engine
}

/// What the display shows after `keys`.
fn shown(keys: &str) -> String {
    run(keys).display().to_owned()
}

#[test]
fn starts_at_zero() {
    let engine = Engine::new();
    assert_eq!(engine.display(), "0");
    assert_eq!(engine.expression(), "");
    assert!(!engine.clears_entry());
}

#[test]
fn digits_build_a_number() {
    assert_eq!(shown("123"), "123");
    assert_eq!(shown("007"), "7");
    assert_eq!(shown("0"), "0");
}

#[test]
fn the_point_is_typed_once() {
    assert_eq!(shown("."), "0.");
    assert_eq!(shown("1.5.2"), "1.52");
    assert_eq!(shown("0.050"), "0.050");
}

#[test]
fn typing_stops_at_twelve_digits() {
    assert_eq!(shown("1234567890123456"), "123456789012");
    assert_eq!(shown("0.12345678901234"), "0.12345678901");
}

#[test]
fn the_four_operations() {
    assert_eq!(shown("12+7="), "19");
    assert_eq!(shown("12-7="), "5");
    assert_eq!(shown("7-12="), "-5");
    assert_eq!(shown("12*7="), "84");
    assert_eq!(shown("7/2="), "3.5");
}

#[test]
fn operations_chain_left_to_right() {
    // A pocket calculator, not operator precedence: (12 + 7) × 3.
    assert_eq!(shown("12+7*3="), "57");
    // The running total shows as soon as the next operator is pressed.
    assert_eq!(shown("12+7*"), "19");
    assert_eq!(shown("2+3*4-5/3="), "5");
}

#[test]
fn a_second_operator_replaces_the_first() {
    assert_eq!(shown("12+*3="), "36");
    assert_eq!(shown("12+-*/4="), "3");
    assert_eq!(run("12+*").expression(), "12 \u{d7}");
}

#[test]
fn equals_repeats_the_last_operation() {
    assert_eq!(shown("2+3="), "5");
    assert_eq!(shown("2+3=="), "8");
    assert_eq!(shown("2+3==="), "11");
    assert_eq!(shown("10-1==="), "7");
    assert_eq!(shown("2*3=="), "18");
    // A new number then `=` applies the remembered `+ 3` to it.
    assert_eq!(shown("2+3=10="), "13");
}

#[test]
fn equals_with_no_right_operand_uses_the_shown_value() {
    assert_eq!(shown("5+="), "10");
    assert_eq!(shown("5*=="), "125");
}

#[test]
fn equals_alone_tidies_the_entry() {
    assert_eq!(shown("12.="), "12");
    assert_eq!(shown("1.500="), "1.5");
    assert_eq!(shown("="), "0");
}

#[test]
fn a_result_feeds_the_next_operation() {
    assert_eq!(shown("2+3=*4="), "20");
    // A digit after a result starts a new calculation.
    assert_eq!(shown("2+3=7"), "7");
    assert_eq!(shown("2+3=7*2="), "14");
}

#[test]
fn no_binary_noise() {
    assert_eq!(shown("0.1+0.2="), "0.3");
    assert_eq!(shown("1.1*1.1="), "1.21");
    assert_eq!(shown("0.3-0.1="), "0.2");
    assert_eq!(shown("1/3*3="), "1");
    assert_eq!(shown("2/3="), "0.666666666667");
}

#[test]
fn division_by_zero_is_an_error() {
    let engine = run("5/0=");
    assert_eq!(engine.display(), "Error");
    assert!(engine.is_error());
    // Operators, equals, ± and % do nothing in the error state.
    assert_eq!(shown("5/0=+="), "Error");
    assert_eq!(shown("5/0=n%<"), "Error");
    // Chaining into a division by zero errors at the next operator.
    assert_eq!(shown("5/0+"), "Error");
}

#[test]
fn an_error_clears_with_c_or_a_new_number() {
    assert_eq!(shown("5/0=c"), "0");
    assert_eq!(shown("5/0=a"), "0");
    assert_eq!(shown("5/0=7"), "7");
    assert_eq!(shown("5/0=7+1="), "8");
    assert_eq!(shown("5/0=."), "0.");
    assert_eq!(run("5/0=c").expression(), "");
}

#[test]
fn overflow_is_an_error() {
    let big = "999999999999";
    let keys = format!("{big}*{big}={big}*=");
    let mut engine = run(&keys);
    for _ in 0..30 {
        engine.press(Press::Equals);
    }
    assert!(engine.is_error(), "{}", engine.display());
}

#[test]
fn a_percent_that_overflows_is_an_error() {
    // 999999999999 to the 14th power (about 1e168) is finite; its percent of
    // itself is not.
    let keys = format!("999999999999*{}+%", "=".repeat(13));
    let engine = run(&keys);
    assert!(engine.is_error(), "{}", engine.display());
}

#[test]
fn large_results_go_scientific() {
    assert_eq!(shown("999999999999+1="), "1e12");
    assert_eq!(shown("123456789*1000000="), "1.2345679e14");
}

#[test]
fn clear_clears_the_entry_then_everything() {
    let engine = run("12+34");
    assert!(engine.clears_entry());
    // C drops only the 34: the pending `12 +` survives.
    assert_eq!(shown("12+34c5="), "17");
    assert!(!run("12+34c").clears_entry());
    // A second C clears everything.
    assert_eq!(shown("12+34cc5="), "5");
    // C with nothing typed is AC.
    assert_eq!(shown("12+c3="), "3");
}

#[test]
fn all_clear_resets() {
    let engine = run("12+34a");
    assert_eq!(engine.display(), "0");
    assert_eq!(engine.expression(), "");
    assert_eq!(shown("2+3=a="), "0");
}

#[test]
fn backspace_edits_only_typed_numbers() {
    assert_eq!(shown("123<"), "12");
    assert_eq!(shown("1<"), "0");
    assert_eq!(shown("1<<<"), "0");
    assert_eq!(shown("1.5<"), "1.");
    assert_eq!(shown("5n<"), "0");
    // A result is not editable.
    assert_eq!(shown("12+3=<"), "15");
}

#[test]
fn negate_toggles_the_sign() {
    assert_eq!(shown("5n"), "-5");
    assert_eq!(shown("5nn"), "5");
    assert_eq!(shown("5n+3="), "-2");
    assert_eq!(shown("0.5n"), "-0.5");
    // ± after an operator starts a negative right operand.
    assert_eq!(shown("12+n"), "-0");
    assert_eq!(shown("12+n4="), "8");
    // ± on a result negates it, and it can be used as an operand.
    assert_eq!(shown("2+3=n"), "-5");
    assert_eq!(shown("2+3=n*2="), "-10");
    assert_eq!(shown("2+3=n7"), "7");
}

#[test]
fn percent_follows_pocket_calculator_rules() {
    assert_eq!(shown("50%"), "0.5");
    // + and − take the percentage of the left operand.
    assert_eq!(shown("200+10%"), "20");
    assert_eq!(shown("200+10%="), "220");
    assert_eq!(shown("200-10%="), "180");
    // × and ÷ use the percentage as a fraction.
    assert_eq!(shown("200*10%="), "20");
    assert_eq!(shown("200/50%="), "400");
    // A percentage counts as an operand: the next operator computes.
    assert_eq!(shown("200+10%+"), "220");
}

#[test]
fn the_expression_line_tracks_the_operation() {
    assert_eq!(run("12+").expression(), "12 +");
    assert_eq!(run("12+7*").expression(), "19 \u{d7}");
    assert_eq!(run("12+7*3=").expression(), "19 \u{d7} 3 =");
    assert_eq!(run("2+3==").expression(), "5 + 3 =");
    assert_eq!(run("8/2-").expression(), "4 \u{2212}");
    // A new number after a result starts over.
    assert_eq!(run("2+3=7").expression(), "");
    // A number typed after an operator keeps it.
    assert_eq!(run("12+7").expression(), "12 +");
}

#[test]
fn results_keep_full_precision_between_steps() {
    // 1/7 shows 12 digits but the next step works on the full value.
    assert_eq!(shown("1/7=*7="), "1");
    // Scientific display (8 digits) does not truncate the operand either.
    assert_eq!(shown("123456789012*1000=/1000="), "123456789012");
}

#[test]
fn a_negative_zero_entry_types_a_negative_number() {
    assert_eq!(shown("n"), "0");
    assert_eq!(shown("0n"), "-0");
    assert_eq!(shown("0n5"), "-5");
}
