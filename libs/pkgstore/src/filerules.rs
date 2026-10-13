//! A sandboxed process's `files` rules as kernel rules (issue #832,
//! `docs/architecture/process-sandbox.md`).
//!
//! For an ordinary app label `files` is consent text and compiles to nothing
//! ([`crate::rules`]). A *process* table (`[processes.<name>]`, the label
//! `app:<id>/<name>`) is confined: the kernel's file gate judges every path it
//! opens, so its `files` compile to the rule ids the gate looks for. The
//! derivation lives in `libs/filepolicy`, which the kernel links too; this
//! module only wraps it in the wire type `pkgd` loads.
//!
//! [`compile`] is the whole interface: the result goes into the label's rule
//! list next to the rules [`crate::rules`] compiles from `interfaces` and
//! `network`, in the one `LoadLabel` call (so revoking the label revokes its
//! files too).

use alloc::string::String;
use alloc::vec::Vec;

use messenger_generated::os_lazy_messenger_policy_v1::LabelRule;

use crate::rules::ANY_METHOD;

/// Why a process table's `files` cannot be compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRuleError {
    /// Index of the refused entry.
    pub index: usize,
    /// The entry as written.
    pub rule: String,
    /// What is wrong with it.
    pub why: &'static str,
}

impl core::fmt::Display for FileRuleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "files entry {:?} {}", self.rule, self.why)
    }
}

/// The allow rules the kernel's file gate needs for `files`: the
/// confinement marker, a grant for each entry and a stat-only grant for each
/// of its ancestors. Sorted and without duplicates.
pub fn compile<S: AsRef<str>>(files: &[S]) -> Result<Vec<LabelRule>, FileRuleError> {
    let ids = filepolicy::compile(files).map_err(|(index, error)| FileRuleError {
        index,
        rule: String::from(files[index].as_ref()),
        why: error.message(),
    })?;
    Ok(ids
        .into_iter()
        .map(|interface_id| LabelRule {
            interface_id,
            method: ANY_METHOD,
            allow: true,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webkit_tables_compile_to_the_ids_the_kernel_looks_for() {
        let rules = compile(&[
            "read:/system/share/fonts",
            "write:$HOME/.apps/os.lazy.webkit",
        ])
        .unwrap();
        let ids: Vec<u64> = rules.iter().map(|rule| rule.interface_id).collect();
        let kernel = filepolicy::compile(&[
            "read:/system/share/fonts",
            "write:$HOME/.apps/os.lazy.webkit",
        ])
        .unwrap();
        assert_eq!(ids, kernel);
        assert!(rules
            .iter()
            .all(|rule| rule.allow && rule.method == ANY_METHOD));
        assert!(ids.contains(&filepolicy::confine_id()));
        // the marker; the first grant and its ancestors (`/`, `/system`,
        // `/system/share`); the second grant and its ancestors (`/` again,
        // `/home`, `$HOME`, `$HOME/.apps`)
        assert_eq!(rules.len(), 1 + 4 + 4);
    }

    #[test]
    fn an_empty_table_is_a_confined_label_with_nothing() {
        let rules = compile::<&str>(&[]).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].interface_id, filepolicy::confine_id());
    }

    #[test]
    fn a_rule_that_cannot_be_confined_is_refused_with_its_index() {
        let error = compile(&["read:/system/share/fonts", "read:/a/*/b"]).unwrap_err();
        assert_eq!(error.index, 1);
        assert_eq!(error.rule, "read:/a/*/b");
        assert!(error.to_string().contains("last segment"));
        assert!(compile(&["read:/home/user/x"]).is_err());
        assert!(compile(&["open:/x"]).is_err());
    }

    #[test]
    fn the_rule_budget_holds_for_a_realistic_table() {
        // Twenty-four entries (the manifest's limit) of depth six stay far
        // below the 256 rules a label may hold.
        let files: Vec<String> = (0..24)
            .map(|i| alloc::format!("read:/a/b/c/d/e/f{i}"))
            .collect();
        let rules = compile(&files).unwrap();
        assert!(rules.len() < crate::rules::MAX_RULES);
    }
}
