//! Tree-walking evaluator.
//!
//! One flat environment (no lexical scopes) keeps this small; enough for a
//! REPL. Every error is a human-readable message.

use super::parser::{BinOp, Expr, Stmt};
use super::value::Value;
use crate::sys;
use alloc::string::String;
use alloc::vec::Vec;

/// The interpreter state: variable bindings.
#[derive(Default)]
pub struct Interp {
    vars: Vec<(String, Value)>,
}

impl Interp {
    pub fn new() -> Self {
        Self::default()
    }

    /// Execute a parsed program.
    pub fn run(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        for stmt in stmts {
            self.exec(stmt)?;
        }
        Ok(())
    }

    fn exec(&mut self, stmt: &Stmt) -> Result<(), String> {
        match stmt {
            Stmt::Let(name, expr) => {
                let value = self.eval(expr)?;
                self.bind(name.clone(), value);
            }
            Stmt::Print(expr) => {
                let value = self.eval(expr)?;
                sys::write_str(&value.display());
                sys::write_str("\n");
            }
            // A bare expression prints its value (REPL-friendly).
            Stmt::Expr(expr) => {
                let value = self.eval(expr)?;
                sys::write_str(&value.display());
                sys::write_str("\n");
            }
            Stmt::Block(stmts) => {
                for stmt in stmts {
                    self.exec(stmt)?;
                }
            }
            Stmt::If(condition, then_branch, else_branch) => {
                if self.eval(condition)?.truthy() {
                    self.exec(then_branch)?;
                } else if let Some(else_branch) = else_branch {
                    self.exec(else_branch)?;
                }
            }
        }
        Ok(())
    }

    fn bind(&mut self, name: String, value: Value) {
        if let Some((_, slot)) = self.vars.iter_mut().find(|(n, _)| *n == name) {
            *slot = value;
        } else {
            self.vars.push((name, value));
        }
    }

    fn lookup(&self, name: &str) -> Result<Value, String> {
        self.vars
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, value)| value.clone())
            .ok_or_else(|| alloc::format!("undefined name '{name}'"))
    }

    fn eval(&self, expr: &Expr) -> Result<Value, String> {
        match expr {
            Expr::Num(n) => Ok(Value::Num(*n)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Var(name) => self.lookup(name),
            Expr::Array(items) => {
                let mut values = Vec::new();
                for item in items {
                    values.push(self.eval(item)?);
                }
                Ok(Value::Array(values))
            }
            Expr::Index(base, index) => index_into(self.eval(base)?, self.eval(index)?),
            Expr::Neg(inner) => match self.eval(inner)? {
                Value::Num(n) => Ok(Value::Num(-n)),
                other => Err(alloc::format!("cannot negate {}", other.type_name())),
            },
            Expr::Not(inner) => Ok(Value::Bool(!self.eval(inner)?.truthy())),
            Expr::Binary(op, left, right) => self.binary(*op, left, right),
        }
    }

    fn binary(&self, op: BinOp, left: &Expr, right: &Expr) -> Result<Value, String> {
        // Logical operators short-circuit.
        if op == BinOp::And {
            return if self.eval(left)?.truthy() {
                Ok(Value::Bool(self.eval(right)?.truthy()))
            } else {
                Ok(Value::Bool(false))
            };
        }
        if op == BinOp::Or {
            return if self.eval(left)?.truthy() {
                Ok(Value::Bool(true))
            } else {
                Ok(Value::Bool(self.eval(right)?.truthy()))
            };
        }

        let a = self.eval(left)?;
        let b = self.eval(right)?;
        match op {
            BinOp::Add => add(a, b),
            BinOp::Sub => numeric(a, b, "subtract", |x, y| x - y),
            BinOp::Mul => numeric(a, b, "multiply", |x, y| x * y),
            BinOp::Div => {
                let (x, y) = both_numbers(&a, &b, "divide")?;
                if y == 0.0 {
                    Err(String::from("division by zero"))
                } else {
                    Ok(Value::Num(x / y))
                }
            }
            BinOp::Rem => numeric(a, b, "take the remainder of", |x, y| x % y),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let (x, y) = both_numbers(&a, &b, "compare")?;
                Ok(Value::Bool(match op {
                    BinOp::Lt => x < y,
                    BinOp::Le => x <= y,
                    BinOp::Gt => x > y,
                    _ => x >= y,
                }))
            }
            BinOp::Eq => Ok(Value::Bool(equal(&a, &b))),
            BinOp::Ne => Ok(Value::Bool(!equal(&a, &b))),
            BinOp::And | BinOp::Or => unreachable!(),
        }
    }
}

/// `+` is overloaded: numbers add, strings concatenate, arrays join.
fn add(a: Value, b: Value) -> Result<Value, String> {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => Ok(Value::Num(x + y)),
        (Value::Array(mut xs), Value::Array(ys)) => {
            xs.extend(ys);
            Ok(Value::Array(xs))
        }
        (x, y) => {
            // Fall back to string concatenation (useful for `print "x = " + x`).
            let mut text = x.display();
            text.push_str(&y.display());
            Ok(Value::Str(text))
        }
    }
}

fn numeric(a: Value, b: Value, verb: &str, op: impl Fn(f64, f64) -> f64) -> Result<Value, String> {
    let (x, y) = both_numbers(&a, &b, verb)?;
    Ok(Value::Num(op(x, y)))
}

fn both_numbers(a: &Value, b: &Value, verb: &str) -> Result<(f64, f64), String> {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => Ok((*x, *y)),
        _ => Err(alloc::format!(
            "cannot {verb} {} and {}",
            a.type_name(),
            b.type_name()
        )),
    }
}

fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| equal(a, b))
        }
        _ => false,
    }
}

fn index_into(base: Value, index: Value) -> Result<Value, String> {
    let Value::Array(items) = base else {
        return Err(alloc::format!("cannot index {}", base.type_name()));
    };
    let Value::Num(n) = index else {
        return Err(String::from("array index must be a number"));
    };
    let i = n as usize;
    items
        .get(i)
        .cloned()
        .ok_or_else(|| alloc::format!("index {i} out of range (len {})", items.len()))
}
