//! `pkgd`'s view of `pkgstore::inspect`: the assessment that feeds `Inspect`
//! and `Install` (moved to the library so the LazyRAD IDE pre-checks with the
//! same code).

pub(crate) use pkgstore::inspect::assess;
