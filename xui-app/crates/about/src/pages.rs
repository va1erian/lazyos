//! What the About panel says: a few short pages, each one a fact that is
//! true of the tree this was built from (docs/platform-plan.md).

pub struct Page {
    pub title: &'static str,
    pub body: &'static str,
}

pub const PAGES: [Page; 6] = [
    Page {
        title: "An OS from scratch",
        body: "LazyOS is a hobby operating system written in Rust: a preemptive \
               multitasking x86_64 kernel, a set of userspace system services, \
               and a desktop of apps. It boots from a disk image in QEMU or on \
               a real PC.",
    },
    Page {
        title: "Messenger",
        body: "Everything talks over Messenger, a kernel-mediated, capability-\
               based IPC and pub/sub fabric. Interfaces are declared in .midl \
               files and compiled by midlc; a message is a header, a TLV body \
               and a list of channel and buffer objects.",
    },
    Page {
        title: "No root",
        body: "Services such as messengerd, init, logd, keyd, accounts, \
               clipboardd and the xuid compositor each run with only the \
               capabilities they need. Nobody is root: privileged changes go \
               through elevd and a trusted prompt.",
    },
    Page {
        title: "Files and Linux programs",
        body: "An ext2 volume at /, a read-only FAT /boot and ramfs scratch \
               space sit under one VFS. Static Linux musl binaries run \
               unmodified: dash, sqlite3, jq, ripgrep, and the ports of Doom \
               and Quake.",
    },
    Page {
        title: "This window",
        body: "The model is drawn by a small software rasteriser in pure Rust: \
               a z-buffer, perspective-correct triangles and a per-pixel \
               shader that reflects the sky. No GPU, no OpenGL: every pixel is \
               computed on the CPU, then handed to xui as an image.",
    },
    Page {
        title: "Who wrote it",
        body: "Most of LazyOS was written by Claude, an AI model made by \
               Anthropic, working with the project's maintainer through \
               issues, tests and screenshots. This About box, and the \
               renderer in it, included.",
    },
];

/// The keys, two short lines for the panel's foot.
pub const HINT: [&str; 2] = [
    "drag: spin    wheel: zoom    space: shape",
    "W: style    left, right: pages    esc: close",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_has_text_that_fits_a_panel() {
        for page in &PAGES {
            assert!(!page.title.is_empty());
            assert!((40..420).contains(&page.body.len()), "{}", page.title);
        }
    }
}
