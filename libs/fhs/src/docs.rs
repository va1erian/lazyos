//! Documentation trees.

/// The documentation tree the `xui-docs` app browses. Written by the image
/// build. Target (F4): `"/docs"` (`/docs/os` from the build, `/docs/apps/...`
/// from `pkgd`).
pub const DOCS_ROOT: &str = "/docs";

/// The repository README the image build embeds in the docs tree. The root is
/// case-sensitive ext2, so readers spell it exactly like this. Written by the
/// image build. Target (F4): `"/docs/os/README.md"`.
pub const README: &str = "/docs/README.md";
