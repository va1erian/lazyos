//! Documentation trees.

/// The documentation root the `xui-docs` app's Open dialog starts in:
/// [`OS_DOCS`] from the build next to [`DOCS_APPS`] from `pkgd`.
pub const DOCS_ROOT: &str = "/docs";

/// The OS documentation (`docs/**/*.md` plus the repository README). Written
/// by the image build.
pub const OS_DOCS: &str = "/docs/os";

/// The repository README the image build embeds in [`OS_DOCS`]. The volume is
/// case-sensitive ext2, so readers spell it exactly like this. Written by the
/// image build.
pub const README: &str = "/docs/os/README.md";

/// Installed apps' documentation, one `<system_name>/` directory each: the
/// package's `docs/*.md`, replaced as a whole on upgrade and deleted on
/// removal. 0755 root; written only by `pkgd`.
pub const DOCS_APPS: &str = "/docs/apps";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_docs_nest_under_the_root() {
        assert!(OS_DOCS.starts_with(DOCS_ROOT));
        assert!(README.starts_with(OS_DOCS));
        assert!(DOCS_APPS.starts_with(DOCS_ROOT));
    }
}
