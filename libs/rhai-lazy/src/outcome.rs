//! Running a script and turning Rhai's result into what the host needs: a
//! value to print, an exit status, or an error message.

use alloc::string::{String, ToString};

use rhai::{Dynamic, Engine, EvalAltResult, Scope};

use crate::engine::OUTPUT_CLOSED;

/// The result of running a script.
#[derive(Debug)]
pub enum Outcome {
    /// Finished; the value of the last expression (`()` if none).
    Value(Dynamic),
    /// The script called `exit(n)`; the status is already reduced to 0..=255.
    Exit(i32),
    /// A compile or runtime error, with position information.
    Failed(String),
    /// Standard output closed under the script; stop without a message.
    OutputClosed,
}

/// Classify Rhai's result. Terminations are not errors: `exit(n)` carries an
/// integer, the closed-output stop carries a marker string.
pub fn classify(result: Result<Dynamic, alloc::boxed::Box<EvalAltResult>>) -> Outcome {
    match result {
        Ok(value) => Outcome::Value(value),
        Err(error) => match *error {
            EvalAltResult::ErrorTerminated(token, _) => match token.as_int() {
                Ok(code) => Outcome::Exit(code.rem_euclid(256) as i32),
                Err(_) if token.into_string().as_deref() == Ok(OUTPUT_CLOSED) => {
                    Outcome::OutputClosed
                }
                Err(_) => Outcome::Failed("script terminated".to_string()),
            },
            other => Outcome::Failed(other.to_string()),
        },
    }
}

/// Blank a leading `#!` line (Rhai's tokenizer rejects it) but keep its newline,
/// so error line numbers still match the file.
pub fn strip_shebang(source: &str) -> &str {
    if source.starts_with("#!") {
        source.find('\n').map_or("", |end| &source[end..])
    } else {
        source
    }
}

/// Compile and run `source` in `scope`; a leading `#!` line is ignored.
pub fn eval_source(engine: &Engine, scope: &mut Scope, source: &str) -> Outcome {
    match engine.compile_with_scope(scope, strip_shebang(source)) {
        Ok(ast) => classify(engine.eval_ast_with_scope::<Dynamic>(scope, &ast)),
        Err(error) => Outcome::Failed(error.to_string()),
    }
}
