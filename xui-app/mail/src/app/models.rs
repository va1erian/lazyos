//! What the folder pane and the message list show, as `ListModel`s.

use esmail::view_model::RowModel;
use esmail_glue::{FolderRef, FolderTree, NodeId};
use xui_core::widget::ListModel;

/// The folder pane: every account's name, then its folders indented under it.
/// `targets` says which folder each row opens (none for an account row or a
/// folder that cannot be opened).
#[derive(Default, Clone)]
pub struct FolderRows {
    texts: Vec<String>,
    pub targets: Vec<Option<FolderRef>>,
}

impl FolderRows {
    pub fn from_tree(tree: &FolderTree) -> FolderRows {
        let mut rows = FolderRows::default();
        for account in tree.children(None) {
            rows.push(tree, account.id, account.text, 0);
        }
        rows
    }

    fn push(&mut self, tree: &FolderTree, id: NodeId, text: String, depth: usize) {
        self.texts.push(format!("{}{text}", "    ".repeat(depth)));
        self.targets.push(tree.selection(id));
        for child in tree.children(Some(id)) {
            self.push(tree, child.id, child.text, depth + 1);
        }
    }

    /// The row showing `folder`, to keep it selected across a refresh.
    pub fn row_of(&self, folder: &FolderRef) -> Option<usize> {
        self.targets
            .iter()
            .position(|target| target.as_ref() == Some(folder))
    }
}

impl ListModel for FolderRows {
    fn rows(&self) -> usize {
        self.texts.len()
    }

    fn cell(&self, row: usize, column: usize) -> Option<&str> {
        (column == 0)
            .then(|| self.texts.get(row).map(String::as_str))
            .flatten()
    }
}

/// The message list: sender, subject and date, unread messages marked.
pub struct MessageRows {
    cells: Vec<[String; 3]>,
}

impl MessageRows {
    pub fn new(rows: &[RowModel]) -> MessageRows {
        MessageRows {
            cells: rows.iter().map(cells).collect(),
        }
    }
}

fn cells(row: &RowModel) -> [String; 3] {
    // The list has no bold face for unread rows, so they carry a dot.
    let mark = if row.seen { "" } else { "\u{2022} " };
    let flag = if row.flagged { " \u{2605}" } else { "" };
    let date = row
        .local_date_time
        .as_ref()
        .map_or_else(String::new, |(date, time)| format!("{date} {time}"));
    [
        format!("{mark}{}", row.sender),
        format!("{}{flag}", row.subject),
        date,
    ]
}

impl ListModel for MessageRows {
    fn rows(&self) -> usize {
        self.cells.len()
    }

    fn cell(&self, row: usize, column: usize) -> Option<&str> {
        self.cells
            .get(row)
            .and_then(|cells| cells.get(column))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use esmail::imap::{MailboxInfo, SpecialUse};

    fn mailbox(name: &str) -> MailboxInfo {
        MailboxInfo {
            name: name.into(),
            delimiter: Some("/".into()),
            special_use: (name == "INBOX").then_some(SpecialUse::Inbox),
            noselect: false,
        }
    }

    #[test]
    fn folders_are_listed_under_their_account_and_open_their_mailbox() {
        let mut tree = FolderTree::new(["Work".to_string()]);
        tree.set_mailboxes(0, &[mailbox("INBOX"), mailbox("Archive")]);
        let rows = FolderRows::from_tree(&tree);
        assert_eq!(rows.cell(0, 0), Some("Work"));
        assert_eq!(rows.targets[0], None);
        let inbox = FolderRef {
            account: 0,
            mailbox: "INBOX".into(),
        };
        let row = rows.row_of(&inbox).expect("INBOX is listed");
        assert!(rows.cell(row, 0).unwrap().starts_with("    "));
    }

    #[test]
    fn unread_rows_are_marked_and_columns_filled() {
        let row = RowModel {
            sender: "Ada".into(),
            subject: "Hello".into(),
            from: String::new(),
            raw_subject: String::new(),
            raw_date: String::new(),
            local_date_time: Some(("2026-10-03".into(), "21:00".into())),
            seen: false,
            flagged: false,
        };
        let rows = MessageRows::new(&[row]);
        assert_eq!(rows.cell(0, 0), Some("\u{2022} Ada"));
        assert_eq!(rows.cell(0, 1), Some("Hello"));
        assert_eq!(rows.cell(0, 2), Some("2026-10-03 21:00"));
        assert_eq!(rows.cell(0, 3), None);
    }
}
