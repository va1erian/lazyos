//! The calls only the full interface has: navigation, the answers to the page's questions, downloads,
//! content blocking and `Recover`. Each is about one of the caller's full views (`ENOENT` otherwise) and checks
//! what the view was made with (`EACCES` for a feature it was not given).

use libmessenger::Object;
use messenger_generated::os_lazy_webd_view_v1 as wire;

use super::calls::{live, Reply, MALFORMED};
use super::{claim, Service};
use crate::config::{c_string_safe, Refusal};
use crate::engine::Engine;
use crate::events::rules::BlockSettings;
use crate::fabric::{errno, Fabric};
use crate::fullconfig::{self, Rules};
use crate::ident::ClientKey;
use crate::limits::{
    EVENT_TEXT_BYTES, EXCEPTIONS_MAX, EXCEPTION_BYTES_MAX, LIST_NAME_BYTES_MAX, RULES_BYTES_MAX,
    URL_BYTES_MAX,
};

const NOT_GIVEN: Refusal = Refusal::new(errno::EACCES, "the view was not made with that");

fn check_url(url: &str) -> Result<(), Refusal> {
    if url.len() > URL_BYTES_MAX || !c_string_safe(url) {
        return Err(Refusal::new(errno::EINVAL, "bad address"));
    }
    Ok(())
}

fn check_list(list: &str) -> Result<(), Refusal> {
    let fine = !list.is_empty()
        && list.len() <= LIST_NAME_BYTES_MAX
        && list
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    fine.then_some(())
        .ok_or(Refusal::new(errno::EINVAL, "bad list name"))
}

/// A blocking exception: a host name, lower case.
fn exception(host: &str) -> Result<String, Refusal> {
    let fine = !host.is_empty()
        && host.len() <= EXCEPTION_BYTES_MAX
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'));
    fine.then(|| host.to_ascii_lowercase()).ok_or(Refusal::new(
        errno::EINVAL,
        "an exception is not a host name",
    ))
}

impl<E: Engine, F: Fabric> Service<E, F> {
    /// The rules of the caller's live full view `id`.
    fn rules_of(&mut self, client: ClientKey, id: u64) -> Result<&mut Rules, Refusal> {
        let view = live(&mut self.views, client, true, id)?;
        // `live` with `full` found a full view.
        Ok(&mut view.full.as_mut().ok_or(MALFORMED)?.rules)
    }

    pub(super) fn full_call(
        &mut self,
        client: ClientKey,
        method: u32,
        body: &[u8],
        pending: &mut Vec<Object>,
    ) -> Reply {
        let bad = |_| MALFORMED;
        match method {
            wire::METHOD_LOAD => {
                let args = wire::decode_load_args(body).map_err(bad)?;
                check_url(&args.url)?;
                let rules = self.rules_of(client, args.view)?;
                if !rules.policy.client_may_load(&args.url) {
                    return Err(Refusal::new(
                        errno::EACCES,
                        "the view may not load that address",
                    ));
                }
                self.engine.load(args.view, &args.url);
            }
            wire::METHOD_RELOAD => {
                let args = wire::decode_reload_args(body).map_err(bad)?;
                let blocking = self.rules_of(client, args.view)?.blocking;
                if args.bypass_blocking && blocking {
                    let page = self.full_of(args.view).map(|full| full.committed.clone());
                    self.queue
                        .rules()
                        .bypass_blocking(args.view, &page.unwrap_or_default());
                }
                self.engine.reload(args.view);
            }
            wire::METHOD_STOP => {
                let args = wire::decode_stop_args(body).map_err(bad)?;
                self.rules_of(client, args.view)?;
                self.engine.stop(args.view);
            }
            wire::METHOD_BACK => {
                let args = wire::decode_back_args(body).map_err(bad)?;
                self.rules_of(client, args.view)?;
                self.engine.back(args.view);
            }
            wire::METHOD_FORWARD => {
                let args = wire::decode_forward_args(body).map_err(bad)?;
                self.rules_of(client, args.view)?;
                self.engine.forward(args.view);
            }
            wire::METHOD_DOWNLOAD => {
                let args = wire::decode_download_args(body).map_err(bad)?;
                check_url(&args.url)?;
                let rules = self.rules_of(client, args.view)?;
                if rules.downloads.is_none() {
                    return Err(NOT_GIVEN);
                }
                if !rules.policy.client_may_load(&args.url) {
                    return Err(Refusal::new(
                        errno::EACCES,
                        "the view may not load that address",
                    ));
                }
                self.engine.download(args.view, &args.url);
            }
            wire::METHOD_RECOVER => {
                let args = wire::decode_recover_args(body).map_err(bad)?;
                self.rules_of(client, args.view)?;
                self.engine.recover(args.view, args.reload);
            }
            wire::METHOD_RECOVERGROUP => {
                let args = wire::decode_recover_group_args(body).map_err(bad)?;
                self.rules_of(client, args.view)?;
                self.engine.recover_group(args.view);
            }
            wire::METHOD_DETACHSLOTS => {
                let args = wire::decode_detach_slots_args(body).map_err(bad)?;
                let view = live(&mut self.views, client, true, args.view)?;
                // As `Resize` does, without a new size: the picture is whole again when the app attaches
                // slots and asks for it. Frames up to the last one sent were painted into the slots that
                // are gone, and may still be queued for the app, which is told which they are.
                view.slots.detach_all();
                view.want_frame();
                let seq = view.seq;
                return wire::encode_detach_slots_reply(&wire::DetachSlotsReply { seq })
                    .map_err(|_| MALFORMED);
            }
            wire::METHOD_SETUSERAGENT => {
                let args = wire::decode_set_user_agent_args(body).map_err(bad)?;
                fullconfig::user_agent(&args.user_agent)?;
                if !self.rules_of(client, args.view)?.agent_settable {
                    return Err(NOT_GIVEN);
                }
                self.engine.set_user_agent(args.view, &args.user_agent);
            }
            wire::METHOD_DOWNLOADCANCEL => {
                let args = wire::decode_download_cancel_args(body).map_err(bad)?;
                if self.rules_of(client, args.view)?.downloads.is_none() {
                    return Err(NOT_GIVEN);
                }
                self.engine.download_cancel(args.view, args.download);
            }
            wire::METHOD_DIALOGREPLY => {
                let args = wire::decode_dialog_reply_args(body).map_err(bad)?;
                if args.value.len() > EVENT_TEXT_BYTES || !c_string_safe(&args.value) {
                    return Err(MALFORMED);
                }
                if !self.rules_of(client, args.view)?.dialogs {
                    return Err(NOT_GIVEN);
                }
                self.engine
                    .dialog_reply(args.view, args.dialog, args.accepted, &args.value);
            }
            wire::METHOD_NAVIGATIONANSWER => {
                let args = wire::decode_navigation_answer_args(body).map_err(bad)?;
                let view = live(&mut self.views, client, true, args.view)?;
                let held = view
                    .full
                    .as_mut()
                    .and_then(|full| full.held.remove(&args.request))
                    .ok_or(Refusal::new(
                        errno::ESRCH,
                        "no such request: answered or expired",
                    ))?;
                self.engine
                    .navigation_answer(args.view, args.request, args.allow && held.loadable);
            }
            wire::METHOD_NOTIFYPERMISSION => self.notify_permission_call(client, body)?,
            wire::METHOD_NOTIFYFORGET => self.notify_forget_call(client, body)?,
            wire::METHOD_NOTIFYSTATE => self.notify_state_call(client, body)?,
            wire::METHOD_SETBLOCKING => {
                let args = wire::decode_set_blocking_args(body).map_err(bad)?;
                if args.exceptions.len() > EXCEPTIONS_MAX {
                    return Err(Refusal::new(errno::EINVAL, "too many exceptions"));
                }
                let exceptions = args
                    .exceptions
                    .iter()
                    .map(|host| exception(host))
                    .collect::<Result<Vec<_>, _>>()?;
                let rules = self.rules_of(client, args.view)?;
                if !rules.blocking {
                    return Err(NOT_GIVEN);
                }
                let profile = rules.profile.clone();
                self.queue.rules().set_blocking(
                    profile,
                    BlockSettings {
                        enabled: args.enabled,
                        exceptions,
                    },
                );
            }
            wire::METHOD_BLOCKINGCOMPILE => {
                let args = wire::decode_blocking_compile_args(body, pending).map_err(bad)?;
                check_list(&args.list)?;
                if !self.rules_of(client, args.view)?.blocking {
                    return Err(NOT_GIVEN);
                }
                if args.rules.len > RULES_BYTES_MAX {
                    return Err(Refusal::new(errno::E2BIG, "rule list is too large"));
                }
                // `read` takes the handle over, also when it fails.
                claim(pending, Object::Buffer(args.rules.handle));
                let json = self
                    .fabric
                    .read(args.rules)
                    .map_err(|_| Refusal::new(errno::EINVAL, "rule buffer cannot be read"))?;
                self.engine.blocking_compile(args.view, &args.list, &json);
            }
            wire::METHOD_BLOCKINGLOAD => {
                let args = wire::decode_blocking_load_args(body).map_err(bad)?;
                check_list(&args.list)?;
                if !self.rules_of(client, args.view)?.blocking {
                    return Err(NOT_GIVEN);
                }
                self.engine.blocking_load(args.view, &args.list);
            }
            wire::METHOD_BLOCKINGFORGET => {
                let args = wire::decode_blocking_forget_args(body).map_err(bad)?;
                check_list(&args.list)?;
                if !self.rules_of(client, args.view)?.blocking {
                    return Err(NOT_GIVEN);
                }
                self.engine.blocking_forget(args.view, &args.list);
            }
            _ => return Err(Refusal::new(errno::ENOTSUP, "unknown method")),
        }
        Ok(Vec::new())
    }
}
