//! The calculator's state machine: a pocket calculator's immediate-execution
//! rules, with no knowledge of widgets.
//!
//! Operators chain left to right as they are pressed (`12 + 7 × 3 =` is
//! `57`, not `33`); a second operator in a row replaces the first; `=`
//! repeats the last operation (`2 + 3 = = =` shows `5`, `8`, `11`); `%`
//! takes a percentage of the left operand for `+` and `−` (`200 + 10 %` is
//! `20`) and divides by a hundred otherwise; a division by zero (or an
//! overflow) shows an error that only a clear or a new number leaves.

use crate::number::{self, digit_count, SIGNIFICANT};

/// A binary operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

impl Op {
    /// The symbol the keypad and the expression line show.
    pub fn symbol(self) -> &'static str {
        match self {
            Op::Add => "+",
            Op::Sub => "\u{2212}",
            Op::Mul => "\u{d7}",
            Op::Div => "\u{f7}",
        }
    }

    /// `lhs op rhs` at the working precision; `None` when it has no finite value
    /// (a division by zero, an overflow).
    fn apply(self, lhs: f64, rhs: f64) -> Option<f64> {
        let value = match self {
            Op::Add => lhs + rhs,
            Op::Sub => lhs - rhs,
            Op::Mul => lhs * rhs,
            Op::Div if rhs == 0.0 => return None,
            Op::Div => lhs / rhs,
        };
        value.is_finite().then(|| number::round(value))
    }
}

/// One key of the calculator, from a button or the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// A digit, `0..=9`.
    Digit(u8),
    /// The decimal point.
    Point,
    Op(Op),
    Equals,
    /// Clears the number being typed, or everything when none is (`C`/`AC`).
    Clear,
    /// Clears everything (`AC`, Escape).
    AllClear,
    /// Deletes the last typed character.
    Backspace,
    /// Changes the sign (`±`).
    Negate,
    Percent,
}

/// What the display holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Input {
    /// A number being typed: digits append to it.
    Typing,
    /// A computed operand (after `±` or `%` on a result): it counts as the
    /// right operand, but a digit starts a new number.
    Operand,
    /// The left operand or a result: the next operator replaces a pending
    /// one instead of computing, and a digit starts a new number.
    Waiting,
}

/// The calculator.
#[derive(Clone, Debug)]
pub struct Engine {
    /// The display text: a typed number as typed, a value as [`number::format`]
    /// writes it.
    entry: String,
    /// The shown value at full precision while nothing is being typed (the
    /// display may show it in fewer digits).
    shown: f64,
    input: Input,
    /// The left operand of `pending`.
    acc: f64,
    pending: Option<Op>,
    /// The operator and right operand a bare `=` repeats.
    repeat: Option<(Op, f64)>,
    /// The expression line above the display (`12 +`, `19 × 3 =`).
    expression: String,
    error: bool,
}

impl Default for Engine {
    fn default() -> Engine {
        Engine {
            entry: "0".to_owned(),
            shown: 0.0,
            input: Input::Waiting,
            acc: 0.0,
            pending: None,
            repeat: None,
            expression: String::new(),
            error: false,
        }
    }
}

impl Engine {
    pub fn new() -> Engine {
        Engine::default()
    }

    /// The main display: the number, or `Error`.
    pub fn display(&self) -> &str {
        if self.error {
            "Error"
        } else {
            &self.entry
        }
    }

    /// The expression line: what the pending or last operation was.
    pub fn expression(&self) -> &str {
        &self.expression
    }

    /// Whether the display shows an error.
    pub fn is_error(&self) -> bool {
        self.error
    }

    /// Whether [`Press::Clear`] would clear only the typed number (the key
    /// reads `C`) rather than everything (`AC`).
    pub fn clears_entry(&self) -> bool {
        !self.error && self.input == Input::Typing && self.entry != "0"
    }

    /// Applies one key.
    pub fn press(&mut self, press: Press) {
        if self.error {
            match press {
                // A new number starts over, as on a pocket calculator.
                Press::Digit(_) | Press::Point => *self = Engine::default(),
                Press::Clear | Press::AllClear => {
                    *self = Engine::default();
                    return;
                }
                _ => return,
            }
        }
        match press {
            Press::Digit(digit) => self.digit(digit),
            Press::Point => self.point(),
            Press::Op(op) => self.operator(op),
            Press::Equals => self.equals(),
            Press::Clear if self.clears_entry() => self.entry = "0".to_owned(),
            Press::Clear | Press::AllClear => *self = Engine::default(),
            Press::Backspace => self.backspace(),
            Press::Negate => self.negate(),
            Press::Percent => self.percent(),
        }
    }

    /// The display's numeric value.
    fn value(&self) -> f64 {
        match self.input {
            Input::Typing => self.entry.parse().unwrap_or(0.0),
            Input::Operand | Input::Waiting => self.shown,
        }
    }

    /// Shows `value` (not typed).
    fn set_value(&mut self, value: f64) {
        self.shown = value;
        self.entry = number::format(value);
    }

    /// Starts a new number unless one is being typed; a new number after a
    /// result (`=`) also clears the finished expression.
    fn begin_typing(&mut self) -> bool {
        if self.input == Input::Typing {
            return false;
        }
        if self.pending.is_none() {
            self.expression.clear();
        }
        self.input = Input::Typing;
        true
    }

    fn digit(&mut self, digit: u8) {
        let digit = char::from(b'0' + digit.min(9));
        if self.begin_typing() {
            self.entry = digit.to_string();
        } else if digit_count(&self.entry) >= SIGNIFICANT {
            // The display holds no more: further digits are ignored.
        } else if self.entry == "0" || self.entry == "-0" {
            self.entry.pop();
            self.entry.push(digit);
        } else {
            self.entry.push(digit);
        }
    }

    fn point(&mut self) {
        if self.begin_typing() {
            self.entry = "0.".to_owned();
        } else if !self.entry.contains('.') {
            self.entry.push('.');
        }
    }

    fn operator(&mut self, op: Op) {
        match (self.pending, self.input) {
            // Two operators in a row: the later one wins.
            (Some(_), Input::Waiting) => {}
            (Some(pending), _) => match self.show(pending.apply(self.acc, self.value())) {
                Some(result) => self.acc = result,
                None => return,
            },
            (None, _) => {
                self.acc = self.value();
                self.set_value(self.acc);
            }
        }
        self.pending = Some(op);
        self.repeat = None;
        self.input = Input::Waiting;
        self.expression = format!("{} {}", number::format(self.acc), op.symbol());
    }

    fn equals(&mut self) {
        let (op, lhs, rhs) = match (self.pending.take(), self.repeat) {
            // `5 + =` adds the shown value to itself.
            (Some(op), _) => (op, self.acc, self.value()),
            (None, Some((op, rhs))) => (op, self.value(), rhs),
            (None, None) => {
                self.set_value(self.value());
                self.input = Input::Waiting;
                return;
            }
        };
        self.repeat = Some((op, rhs));
        self.expression = format!(
            "{} {} {} =",
            number::format(lhs),
            op.symbol(),
            number::format(rhs)
        );
        if let Some(result) = self.show(op.apply(lhs, rhs)) {
            self.acc = result;
            self.input = Input::Waiting;
        }
    }

    fn backspace(&mut self) {
        if self.input != Input::Typing {
            return;
        }
        self.entry.pop();
        if matches!(self.entry.as_str(), "" | "-") {
            self.entry = "0".to_owned();
        }
    }

    fn negate(&mut self) {
        match self.input {
            // `12 + ±` starts typing a negative right operand.
            Input::Waiting if self.pending.is_some() => {
                self.entry = "-0".to_owned();
                self.input = Input::Typing;
            }
            Input::Typing => match self.entry.strip_prefix('-') {
                Some(positive) => self.entry = positive.to_owned(),
                None => self.entry.insert(0, '-'),
            },
            _ => {
                self.set_value(-self.value());
                self.input = Input::Operand;
            }
        }
    }

    fn percent(&mut self) {
        let value = self.value();
        let result = match self.pending {
            Some(Op::Add | Op::Sub) => self.acc * value / 100.0,
            _ => value / 100.0,
        };
        if self.show(Some(number::round(result))).is_some() {
            self.input = Input::Operand;
        }
    }

    /// Shows a result, or the error state for `None`; passes the result on.
    fn show(&mut self, result: Option<f64>) -> Option<f64> {
        match result {
            Some(value) => self.set_value(value),
            None => {
                self.error = true;
                self.pending = None;
                self.repeat = None;
            }
        }
        result
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
