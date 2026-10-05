//! What the accounts' sessions and the cache report, applied to the window,
//! and the requests the folder pane and the message list make.

use esmail::imap::{ImapCommand, ImapEvent, MailHeader};
use esmail::render::Attachment;
use esmail::smtp::SmtpEvent;
use esmail_glue::mailbox::OpenFolder;
use esmail_glue::{CacheEvent, FolderRef};
use xui_core::app::Ui;

use super::models::MessageRows;
use super::{Mail, Msg};

/// How many of a folder's newest cached messages show before the server answers.
const CACHED_ROWS: usize = 500;

impl Mail {
    /// Applies everything the core and the cache have queued. Never blocks.
    pub(super) fn drain(&mut self, ui: &Ui<Msg>) {
        let Some(core) = self.core.as_mut() else {
            return;
        };
        let cached = core.cache().pump();
        let events = core.pump();
        let sent = core.pump_sent();
        let mut folders_changed = false;
        for event in cached {
            folders_changed |= self.handle_cache(event);
        }
        for (account, event) in events {
            folders_changed |= self.handle(ui, account, event);
        }
        for event in sent {
            self.sent(ui, event);
        }
        if folders_changed {
            self.refresh_folders();
        }
    }

    fn handle_cache(&mut self, event: CacheEvent) -> bool {
        match event {
            CacheEvent::Folders { account, mailboxes } => {
                self.folders.set_cached_mailboxes(account, &mailboxes);
                return true;
            }
            CacheEvent::Headers {
                account,
                mailbox,
                headers,
            } => {
                let Some(open) = self
                    .open
                    .as_mut()
                    .filter(|o| o.folder().account == account && o.folder().mailbox == mailbox)
                else {
                    return false;
                };
                if open.seed(headers) {
                    self.show_rows();
                }
            }
            CacheEvent::Failed(message) => self.error(&format!("cache: {message}")),
            // Drafts, the outbox and search have no UI here yet.
            _ => {}
        }
        false
    }

    /// Applies one session event. Returns whether the folder pane changed.
    fn handle(&mut self, ui: &Ui<Msg>, account: usize, event: ImapEvent) -> bool {
        match event {
            ImapEvent::Connected => {
                println!("MAIL:CONNECTED:{account}");
                self.set_status("Connected");
                self.command(account, ImapCommand::FetchMailboxes);
                if self
                    .open
                    .as_ref()
                    .is_some_and(|open| open.folder().account == account)
                {
                    self.request_refresh();
                }
            }
            ImapEvent::Disconnected => self.set_status("Disconnected, reconnecting..."),
            ImapEvent::Error(error) => {
                if let Some(open) = self.open.as_mut() {
                    open.page_failed();
                }
                self.error(&error);
                if looks_like_a_login_failure(&error) {
                    self.password_queue.push_back((account, error));
                    if !self.compose_page.is_open() {
                        self.ask_next_password(ui);
                    }
                }
            }
            ImapEvent::Mailboxes(mailboxes) => {
                println!("MAIL:FOLDERS:{account}:{}", mailboxes.len());
                self.folders.set_mailboxes(account, &mailboxes);
                let names = self.folders.mailbox_names(account);
                self.command(account, ImapCommand::FetchUnreadCounts { mailboxes: names });
                if self.open.is_none() {
                    self.open_folder(FolderRef {
                        account,
                        mailbox: "INBOX".into(),
                    });
                }
                return true;
            }
            ImapEvent::UnreadCounts(counts) => {
                self.folders.set_unread(account, counts);
                return true;
            }
            ImapEvent::NewHeaders { mailbox, .. } => {
                if self.open.as_ref().is_some_and(|open| {
                    open.folder().account == account && open.folder().mailbox == mailbox
                }) {
                    self.request_refresh();
                }
            }
            ImapEvent::Headers {
                mailbox,
                headers,
                page,
                total_pages,
                req_id,
                ..
            } => {
                println!("MAIL:HEADERS:{mailbox}:{}", headers.len());
                self.apply_headers(account, &mailbox, req_id, page, total_pages, headers);
            }
            ImapEvent::Body {
                uid,
                html,
                attachments,
                req_id,
            } => self.body_arrived(account, uid, req_id, Ok((html, attachments))),
            ImapEvent::BodyFailed { uid, req_id, error } => {
                self.body_arrived(account, uid, req_id, Err(error))
            }
            ImapEvent::MailData {
                mailbox,
                header,
                body,
                attachments,
            } => {
                self.core_cache(|cache| {
                    cache.index_mail(account, &mailbox, header, body, attachments)
                });
            }
            _ => {}
        }
        false
    }

    fn sent(&mut self, ui: &Ui<Msg>, event: SmtpEvent) {
        match event {
            SmtpEvent::Sent { .. } => {
                println!("MAIL:SEND:PASS");
                self.set_status("Message sent");
                self.close_compose(ui);
            }
            SmtpEvent::Error { error, .. } => {
                println!("MAIL:SEND:FAIL");
                self.compose_page.set_sending(ui, false);
                self.error(&format!("Could not send: {error}"));
            }
        }
    }

    fn command(&self, account: usize, command: ImapCommand) {
        let sent = self
            .core
            .as_ref()
            .is_some_and(|core| core.send(account, command));
        if !sent {
            self.set_status("That account is not connected.");
        }
    }

    fn core_cache(&self, f: impl FnOnce(&esmail_glue::Cache)) {
        if let Some(core) = self.core.as_ref() {
            f(core.cache());
        }
    }

    pub(super) fn open_folder(&mut self, folder: FolderRef) {
        self.bodies.cancel();
        self.selected = None;
        self.message_list.set_model(MessageRows::new(&[]));
        self.reader.show_notice("Select a message to read it.");
        self.set_status(&format!("Loading {}...", folder.mailbox));
        self.core_cache(|cache| {
            cache.load_folder(folder.account, folder.mailbox.clone(), CACHED_ROWS)
        });
        self.open = Some(OpenFolder::new(folder));
        self.request_page();
    }

    fn request_page(&mut self) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        let Some(request) = open.next_request() else {
            return;
        };
        let folder = open.folder().clone();
        let command = ImapCommand::FetchHeaders {
            mailbox: folder.mailbox,
            page: request.page,
            req_id: request.id,
        };
        let sent = self
            .core
            .as_ref()
            .is_some_and(|core| core.send(folder.account, command));
        if !sent && let Some(open) = self.open.as_mut() {
            open.page_failed();
        }
    }

    /// Get mail: merges the newest page into the open folder.
    pub(super) fn refresh(&mut self) {
        match self.open.as_ref().map(|open| open.folder().clone()) {
            Some(folder) if !self.request_refresh() => self.open_folder(folder),
            Some(_) => self.set_status("Checking for new mail..."),
            None => self.set_status("Select a folder first."),
        }
    }

    fn request_refresh(&mut self) -> bool {
        let Some(open) = self.open.as_mut() else {
            return false;
        };
        let Some(id) = open.refresh_request() else {
            return false;
        };
        let folder = open.folder().clone();
        self.command(
            folder.account,
            ImapCommand::FetchHeaders {
                mailbox: folder.mailbox,
                page: 1,
                req_id: id,
            },
        );
        true
    }

    fn apply_headers(
        &mut self,
        account: usize,
        mailbox: &str,
        req_id: u64,
        page: u32,
        total_pages: u32,
        headers: Vec<MailHeader>,
    ) {
        self.core_cache(|cache| cache.index_headers(account, mailbox, &headers));
        let Some(open) = self
            .open
            .as_mut()
            .filter(|o| o.folder().account == account && o.folder().mailbox == mailbox)
        else {
            return;
        };
        if open
            .apply_reply(req_id, page, total_pages, headers)
            .is_some()
        {
            self.show_rows();
        }
    }

    /// Shows the open folder's rows, keeping the selected message selected.
    fn show_rows(&mut self) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let rows = open.rows();
        let selected = self
            .selected
            .as_ref()
            .and_then(|(_, header)| open.row_of(header.uid));
        let more = if open.is_complete() {
            ""
        } else {
            " (Get mail for newer)"
        };
        let status = format!(
            "{}: {} messages{more}",
            open.folder().mailbox,
            open.loaded()
        );
        self.message_list.set_model(MessageRows::new(&rows));
        self.message_list.select(selected);
        self.set_status(&status);
    }

    pub(super) fn select_message(&mut self, row: usize) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let Some(header) = open.header(row).cloned() else {
            return;
        };
        let folder = open.folder().clone();
        if self
            .selected
            .as_ref()
            .is_some_and(|(f, h)| *f == folder && h.uid == header.uid)
        {
            return;
        }
        let key = (folder.account, folder.mailbox.clone(), header.uid);
        self.selected = Some((folder, header));
        self.selected_body.clear();
        self.set_status("Loading message...");
        if let Some(((account, mailbox, uid), req_id)) = self.bodies.want(key) {
            self.command(
                account,
                ImapCommand::FetchBody {
                    mailbox,
                    uid,
                    req_id,
                },
            );
        }
    }

    fn body_arrived(
        &mut self,
        account: usize,
        uid: u32,
        req_id: u64,
        outcome: Result<(String, Vec<Attachment>), String>,
    ) {
        let Some((folder, header)) = self.selected.clone() else {
            return;
        };
        let key = (account, folder.mailbox.clone(), uid);
        let finished = self.bodies.finished(&key, req_id);
        if finished.show && folder.account == account && header.uid == uid {
            match outcome {
                Ok((html, attachments)) => {
                    println!("MAIL:BODY:PASS:{uid}");
                    self.core_cache(|cache| {
                        cache.index_mail(
                            account,
                            &folder.mailbox,
                            header.clone(),
                            html.clone(),
                            attachments.clone(),
                        )
                    });
                    self.reader.show_message(&header, &html, &attachments);
                    self.selected_body = html;
                    self.set_status("Ready");
                }
                Err(error) => {
                    self.reader
                        .show_notice(&format!("Could not load this message: {error}"));
                    self.error(&error);
                }
            }
        }
        if let Some(((account, mailbox, uid), req_id)) = finished.next {
            self.command(
                account,
                ImapCommand::FetchBody {
                    mailbox,
                    uid,
                    req_id,
                },
            );
        }
    }
}

/// Whether a session error means the password was refused, so the user should
/// be asked again rather than the session retrying with it.
fn looks_like_a_login_failure(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "authenticationfailed",
        "authentication failed",
        "invalid credentials",
        "login failed",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::looks_like_a_login_failure;

    #[test]
    fn a_refused_login_is_told_apart_from_a_network_error() {
        assert!(looks_like_a_login_failure(
            "NO [AUTHENTICATIONFAILED] Invalid credentials (Failure)"
        ));
        assert!(!looks_like_a_login_failure("connection reset by peer"));
    }
}
