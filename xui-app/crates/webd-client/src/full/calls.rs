//! The calls only the full interface has: loading and navigation, the answers to the page's
//! questions, downloads and content blocking. Each waits for webd's reply: `EACCES` is a feature
//! the view was not made with (or an address its policy refuses), `ESRCH` a request that is gone.

use libmessenger::Buffer;
use messenger_generated::os_lazy_webd_view_v1 as wire;

use super::FullWebView;
use crate::event::Error;
use crate::transport::{errno, ScopedTransport, SharedMem};

const BAD: Error = Error::Refused(errno::EINVAL);

impl<T: ScopedTransport> FullWebView<T> {
    /// Load `url`: it must pass the view's scheme policy (`EACCES` otherwise; `file:` never does).
    pub fn load(&mut self, url: &str) -> Result<(), Error> {
        let view = self.live_view()?;
        let url = url.to_owned();
        let body = wire::encode_load_args(&wire::LoadArgs { view, url }).map_err(|_| BAD)?;
        self.call(wire::METHOD_LOAD, body)
    }

    /// Replace the page with `html` at `base_url` (empty, `about:`, `data:`, or an address the
    /// policy allows).
    pub fn load_html(&mut self, html: &str, base_url: &str) -> Result<(), Error> {
        let view = self.live_view()?;
        // Shared memory cannot be empty; a blank page is a space.
        let bytes: &[u8] = if html.is_empty() {
            b" "
        } else {
            html.as_bytes()
        };
        let mut mem = self
            .transport
            .alloc(bytes.len())
            .map_err(|code| Error::Refused(-code))?;
        mem.write(bytes);
        let args = wire::LoadHtmlArgs {
            view,
            html: Buffer::whole(mem.handle(), bytes.len() as u64),
            base_url: base_url.to_owned(),
        };
        let (body, objects) = wire::encode_load_html_args(&args).map_err(|_| BAD)?;
        // webd copied the bytes before it replied; `mem` drops after the call.
        self.call_with(wire::METHOD_LOADHTML, body, objects)
    }

    /// Load the page again; `bypass_blocking` once without the content blocker.
    pub fn reload(&mut self, bypass_blocking: bool) -> Result<(), Error> {
        let view = self.live_view()?;
        let args = wire::ReloadArgs {
            view,
            bypass_blocking,
        };
        let body = wire::encode_reload_args(&args).map_err(|_| BAD)?;
        self.call(wire::METHOD_RELOAD, body)
    }

    pub fn stop(&mut self) -> Result<(), Error> {
        let view = self.live_view()?;
        let body = wire::encode_stop_args(&wire::StopArgs { view }).map_err(|_| BAD)?;
        self.call(wire::METHOD_STOP, body)
    }

    pub fn back(&mut self) -> Result<(), Error> {
        let view = self.live_view()?;
        let body = wire::encode_back_args(&wire::BackArgs { view }).map_err(|_| BAD)?;
        self.call(wire::METHOD_BACK, body)
    }

    pub fn forward(&mut self) -> Result<(), Error> {
        let view = self.live_view()?;
        let body = wire::encode_forward_args(&wire::ForwardArgs { view }).map_err(|_| BAD)?;
        self.call(wire::METHOD_FORWARD, body)
    }

    /// Save `url` as a download (a view made with `downloads`).
    pub fn download(&mut self, url: &str) -> Result<(), Error> {
        let view = self.live_view()?;
        let url = url.to_owned();
        let body =
            wire::encode_download_args(&wire::DownloadArgs { view, url }).map_err(|_| BAD)?;
        self.call(wire::METHOD_DOWNLOAD, body)
    }

    /// Stop download `download`; its `.part` file is removed.
    pub fn download_cancel(&mut self, download: u64) -> Result<(), Error> {
        let view = self.live_view()?;
        let args = wire::DownloadCancelArgs { view, download };
        let body = wire::encode_download_cancel_args(&args).map_err(|_| BAD)?;
        self.call(wire::METHOD_DOWNLOADCANCEL, body)
    }

    /// Answer `Dialog` `dialog`: `accepted` is OK, `value` a prompt's text.
    pub fn dialog_reply(&mut self, dialog: u64, accepted: bool, value: &str) -> Result<(), Error> {
        let view = self.live_view()?;
        let args = wire::DialogReplyArgs {
            view,
            dialog,
            accepted,
            value: value.to_owned(),
        };
        let body = wire::encode_dialog_reply_args(&args).map_err(|_| BAD)?;
        self.call(wire::METHOD_DIALOGREPLY, body)
    }

    /// Answer `NavigationRequested` `request`: `allow` loads it (only an address the engine can
    /// load: another scheme is the app's to open). A request answered already or expired (10
    /// seconds) is [`Error::Refused`] with `ESRCH`, and the view goes on.
    pub fn navigation_answer(&mut self, request: u64, allow: bool) -> Result<(), Error> {
        let view = self.live_view()?;
        let args = wire::NavigationAnswerArgs {
            view,
            request,
            allow,
        };
        let body = wire::encode_navigation_answer_args(&args).map_err(|_| BAD)?;
        self.call_about_request(wire::METHOD_NAVIGATIONANSWER, body)
    }

    /// Answer `NotifyPermissionRequest` for `origin` with a `PermissionAnswer`.
    pub fn notify_permission(&mut self, origin: &str, answer: u32) -> Result<(), Error> {
        let view = self.live_view()?;
        let args = wire::NotifyPermissionArgs {
            view,
            origin: origin.to_owned(),
            answer,
        };
        let body = wire::encode_notify_permission_args(&args).map_err(|_| BAD)?;
        self.call(wire::METHOD_NOTIFYPERMISSION, body)
    }

    /// Forget the profile's decision about `origin` (a view made with `notifications`): the page's
    /// `Notification.permission` is `default` again and its next request is asked again.
    pub fn notify_forget(&mut self, origin: &str) -> Result<(), Error> {
        let view = self.live_view()?;
        let args = wire::NotifyForgetArgs {
            view,
            origin: origin.to_owned(),
        };
        let body = wire::encode_notify_forget_args(&args).map_err(|_| BAD)?;
        self.call(wire::METHOD_NOTIFYFORGET, body)
    }

    /// What became of the app's toast for notification `notification` that
    /// [`crate::full::FullEvent::NotifyShow`] gave a view made with `notify_in_client`: a
    /// `NotifyStateKind` (shown, clicked: the page's click handler runs, closed). A notification
    /// that is over or not this view's is [`Error::Refused`] with `ESRCH`, and the view goes on.
    pub fn notify_state(&mut self, notification: u64, state: u32) -> Result<(), Error> {
        let view = self.live_view()?;
        let args = wire::NotifyStateArgs {
            view,
            notification,
            state,
        };
        let body = wire::encode_notify_state_args(&args).map_err(|_| BAD)?;
        self.call_about_request(wire::METHOD_NOTIFYSTATE, body)
    }

    /// The page's process does not answer ([`crate::full::FullEvent::Responsive`] `false`) and the
    /// person chose a way out: it is ended. With `reload` the page loads again in a fresh process.
    /// Without it the page stays stopped and the view ends (`ViewLost`, reason `Other`): make a
    /// new one. Nothing happens while other views share the process.
    pub fn recover(&mut self, reload: bool) -> Result<(), Error> {
        let view = self.live_view()?;
        let body =
            wire::encode_recover_args(&wire::RecoverArgs { view, reload }).map_err(|_| BAD)?;
        self.call(wire::METHOD_RECOVER, body)
    }

    /// Like [`FullWebView::recover`] without a reload, for a view whose web process other views share
    /// (tabs of one site, [`ViewConfig::process_key`]): the process is ended and every view in it is
    /// lost with reason `Other`. Make the views again.
    pub fn recover_group(&mut self) -> Result<(), Error> {
        let view = self.live_view()?;
        let body =
            wire::encode_recover_group_args(&wire::RecoverGroupArgs { view }).map_err(|_| BAD)?;
        self.call(wire::METHOD_RECOVERGROUP, body)
    }

    /// Change the user agent from the next load on; `""` restores the one the view was made with.
    /// `EACCES` unless the view was made through the browser scope with a `user_agent` or a
    /// `user_agent_suffix`.
    pub fn set_user_agent(&mut self, user_agent: &str) -> Result<(), Error> {
        let view = self.live_view()?;
        let args = wire::SetUserAgentArgs {
            view,
            user_agent: user_agent.to_owned(),
        };
        let body = wire::encode_set_user_agent_args(&args).map_err(|_| BAD)?;
        self.call(wire::METHOD_SETUSERAGENT, body)?;
        // A view made again (webd went away) is made with the agent the person last chose.
        if !user_agent.is_empty() {
            self.config.user_agent = user_agent.to_owned();
        }
        Ok(())
    }

    /// The user agent the next [`create`](FullWebView::create) asks for (`""`: the engine's own).
    pub fn set_created_user_agent(&mut self, user_agent: &str) {
        self.config.user_agent = user_agent.to_owned();
    }

    /// Content blocking on or off for the view's profile, and the hosts it is off for.
    pub fn set_blocking(&mut self, enabled: bool, exceptions: &[String]) -> Result<(), Error> {
        let view = self.live_view()?;
        let args = wire::SetBlockingArgs {
            view,
            enabled,
            exceptions: exceptions.to_vec(),
        };
        let body = wire::encode_set_blocking_args(&args).map_err(|_| BAD)?;
        self.call(wire::METHOD_SETBLOCKING, body)
    }

    /// Compile `rules` (a WebKit content rule list, JSON) as list `list` and attach it; the result
    /// comes as [`crate::full::FullEvent::BlockingList`].
    pub fn blocking_compile(&mut self, list: &str, rules: &[u8]) -> Result<(), Error> {
        let view = self.live_view()?;
        let bytes: &[u8] = if rules.is_empty() { b" " } else { rules };
        let mut mem = self
            .transport
            .alloc(bytes.len())
            .map_err(|code| Error::Refused(-code))?;
        mem.write(bytes);
        let args = wire::BlockingCompileArgs {
            view,
            list: list.to_owned(),
            rules: Buffer::whole(mem.handle(), bytes.len() as u64),
        };
        let (body, objects) = wire::encode_blocking_compile_args(&args).map_err(|_| BAD)?;
        self.call_with(wire::METHOD_BLOCKINGCOMPILE, body, objects)
    }

    /// Attach list `list` from the profile's store.
    pub fn blocking_load(&mut self, list: &str) -> Result<(), Error> {
        let view = self.live_view()?;
        let args = wire::BlockingLoadArgs {
            view,
            list: list.to_owned(),
        };
        let body = wire::encode_blocking_load_args(&args).map_err(|_| BAD)?;
        self.call(wire::METHOD_BLOCKINGLOAD, body)
    }

    /// Detach list `list` and remove it from the store.
    pub fn blocking_forget(&mut self, list: &str) -> Result<(), Error> {
        let view = self.live_view()?;
        let args = wire::BlockingForgetArgs {
            view,
            list: list.to_owned(),
        };
        let body = wire::encode_blocking_forget_args(&args).map_err(|_| BAD)?;
        self.call(wire::METHOD_BLOCKINGFORGET, body)
    }
}
