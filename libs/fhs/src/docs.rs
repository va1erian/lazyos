//! Documentation trees.

/// The documentation root the `xui-docs` app's Open dialog starts in:
/// [`OS_DOCS`] from the build next to `/docs/apps/...` from `pkgd` (F4).
pub const DOCS_ROOT: &str = "/docs";

/// The OS documentation (`docs/**/*.md` plus the repository README). Written
/// by the image build.
pub const OS_DOCS: &str = "/docs/os";

/// The repository README the image build embeds in [`OS_DOCS`]. The volume is
/// case-sensitive ext2, so readers spell it exactly like this. Written by the
/// image build.
pub const README: &str = "/docs/os/README.md";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_docs_nest_under_the_root() {
        assert!(OS_DOCS.starts_with(DOCS_ROOT));
        assert!(README.starts_with(OS_DOCS));
    }
}
