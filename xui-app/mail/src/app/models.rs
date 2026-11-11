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

/// The message list's cells, one row per message: sender, subject and
/// date, unread messages marked.
pub fn message_cells(rows: &[RowModel]) -> Vec<Vec<String>> {
    rows.iter().map(|row| cells(row).to_vec()).collect()
}

/// What the message list's columns sort by: the sender and subject without
/// the unread and flag marks (so a click on From does not put every unread
/// message first), and the date as `YYYY-MM-DD HH:MM`, which sorts as text.
pub fn message_keys(rows: &[RowModel]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| {
            let mut key = cells(row).to_vec();
            key[0] = row.sender.clone();
            key[1] = row.subject.clone();
            key
        })
        .collect()
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
        let cells = message_cells(&[row]);
        assert_eq!(cells, [["\u{2022} Ada", "Hello", "2026-10-03 21:00"]]);
    }

    #[test]
    fn sort_keys_drop_the_marks() {
        let row = RowModel {
            sender: "Ada".into(),
            subject: "Hello".into(),
            from: String::new(),
            raw_subject: String::new(),
            raw_date: String::new(),
            local_date_time: Some(("2026-10-03".into(), "21:00".into())),
            seen: false,
            flagged: true,
        };
        let rows = [row];
        assert_eq!(message_cells(&rows)[0][1], "Hello \u{2605}");
        assert_eq!(message_keys(&rows), [["Ada", "Hello", "2026-10-03 21:00"]]);
    }
}
