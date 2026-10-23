//! The typed requests of `os.lazy.webd.view.v1` a test client makes (`impl Fx`): one function per
//! call, each encoding with the generated codec and sending on the view interface.

#![allow(dead_code)]

use libmessenger::Buffer;
use messenger_generated::os_lazy_webd_view_v1 as wire;
use xui_webd::ident::Origin;
use xui_webd::mem::Shared;

use super::full::Fx;
use super::Outcome;

impl Fx {
    pub fn destroy_view(&mut self, from: Origin, view: u64) -> Outcome {
        let body = wire::encode_destroy_view_args(&wire::DestroyViewArgs { view }).unwrap();
        self.call(from, wire::METHOD_DESTROYVIEW, body)
    }

    pub fn resize_view(&mut self, from: Origin, view: u64, width: u32, height: u32) -> Outcome {
        let body = wire::encode_resize_args(&wire::ResizeArgs {
            view,
            width,
            height,
        })
        .unwrap();
        self.call(from, wire::METHOD_RESIZE, body)
    }

    /// Attach a fresh zeroed buffer of `bytes` bytes as `slot` of a full view.
    pub fn attach_slot(
        &mut self,
        from: Origin,
        view: u64,
        slot: u32,
        bytes: usize,
    ) -> (Outcome, Shared) {
        let (handle, shared) = self.rig.fabric.new_buffer(bytes);
        let args = wire::AttachSlotArgs {
            view,
            slot,
            pixels: Buffer::whole(handle, bytes as u64),
        };
        let (body, objects) = wire::encode_attach_slot_args(&args).unwrap();
        let method = wire::METHOD_ATTACHSLOT;
        let result = self.send(from, wire::INTERFACE_ID, method, body, objects);
        (result, shared)
    }

    /// Attach the three frame slots of a view of `width` x `height`.
    pub fn attach_frames(
        &mut self,
        from: Origin,
        view: u64,
        width: u32,
        height: u32,
    ) -> Vec<Shared> {
        (0..3)
            .map(|slot| {
                let (result, shared) =
                    self.attach_slot(from, view, slot, (width * height * 4) as usize);
                result.unwrap();
                shared
            })
            .collect()
    }

    pub fn release_slot(&mut self, from: Origin, view: u64, slot: u32) -> Outcome {
        let body = wire::encode_release_slot_args(&wire::ReleaseSlotArgs { view, slot }).unwrap();
        self.call(from, wire::METHOD_RELEASESLOT, body)
    }

    pub fn load_url(&mut self, from: Origin, view: u64, url: &str) -> Outcome {
        let body = wire::encode_load_args(&wire::LoadArgs {
            view,
            url: url.into(),
        })
        .unwrap();
        self.call(from, wire::METHOD_LOAD, body)
    }

    pub fn load_page(&mut self, from: Origin, view: u64, html: &[u8], base: &str) -> Outcome {
        let (handle, _) = self.rig.fabric.new_buffer_with(html.to_vec());
        self.load_buffer(from, view, Buffer::whole(handle, html.len() as u64), base)
    }

    pub fn load_buffer(&mut self, from: Origin, view: u64, html: Buffer, base: &str) -> Outcome {
        let args = wire::LoadHtmlArgs {
            view,
            html,
            base_url: base.into(),
        };
        let (body, objects) = wire::encode_load_html_args(&args).unwrap();
        let method = wire::METHOD_LOADHTML;
        self.send(from, wire::INTERFACE_ID, method, body, objects)
    }

    pub fn reload(&mut self, from: Origin, view: u64, bypass_blocking: bool) -> Outcome {
        let args = wire::ReloadArgs {
            view,
            bypass_blocking,
        };
        let body = wire::encode_reload_args(&args).unwrap();
        self.call(from, wire::METHOD_RELOAD, body)
    }

    pub fn stop(&mut self, from: Origin, view: u64) -> Outcome {
        let body = wire::encode_stop_args(&wire::StopArgs { view }).unwrap();
        self.call(from, wire::METHOD_STOP, body)
    }

    pub fn back(&mut self, from: Origin, view: u64) -> Outcome {
        let body = wire::encode_back_args(&wire::BackArgs { view }).unwrap();
        self.call(from, wire::METHOD_BACK, body)
    }

    pub fn forward(&mut self, from: Origin, view: u64) -> Outcome {
        let body = wire::encode_forward_args(&wire::ForwardArgs { view }).unwrap();
        self.call(from, wire::METHOD_FORWARD, body)
    }

    pub fn download(&mut self, from: Origin, view: u64, url: &str) -> Outcome {
        let args = wire::DownloadArgs {
            view,
            url: url.into(),
        };
        let body = wire::encode_download_args(&args).unwrap();
        self.call(from, wire::METHOD_DOWNLOAD, body)
    }

    pub fn download_cancel(&mut self, from: Origin, view: u64, download: u64) -> Outcome {
        let args = wire::DownloadCancelArgs { view, download };
        let body = wire::encode_download_cancel_args(&args).unwrap();
        self.call(from, wire::METHOD_DOWNLOADCANCEL, body)
    }

    pub fn dialog_reply(
        &mut self,
        from: Origin,
        view: u64,
        dialog: u64,
        accepted: bool,
        value: &str,
    ) -> Outcome {
        let args = wire::DialogReplyArgs {
            view,
            dialog,
            accepted,
            value: value.into(),
        };
        let body = wire::encode_dialog_reply_args(&args).unwrap();
        self.call(from, wire::METHOD_DIALOGREPLY, body)
    }

    pub fn nav_answer(&mut self, from: Origin, view: u64, request: u64, allow: bool) -> Outcome {
        let args = wire::NavigationAnswerArgs {
            view,
            request,
            allow,
        };
        let body = wire::encode_navigation_answer_args(&args).unwrap();
        self.call(from, wire::METHOD_NAVIGATIONANSWER, body)
    }

    pub fn notify_permission(
        &mut self,
        from: Origin,
        view: u64,
        origin: &str,
        answer: u32,
    ) -> Outcome {
        let args = wire::NotifyPermissionArgs {
            view,
            origin: origin.into(),
            answer,
        };
        let body = wire::encode_notify_permission_args(&args).unwrap();
        self.call(from, wire::METHOD_NOTIFYPERMISSION, body)
    }

    pub fn notify_forget(&mut self, from: Origin, view: u64, origin: &str) -> Outcome {
        let args = wire::NotifyForgetArgs {
            view,
            origin: origin.into(),
        };
        let body = wire::encode_notify_forget_args(&args).unwrap();
        self.call(from, wire::METHOD_NOTIFYFORGET, body)
    }

    pub fn notify_state(
        &mut self,
        from: Origin,
        view: u64,
        notification: u64,
        state: u32,
    ) -> Outcome {
        let args = wire::NotifyStateArgs {
            view,
            notification,
            state,
        };
        let body = wire::encode_notify_state_args(&args).unwrap();
        self.call(from, wire::METHOD_NOTIFYSTATE, body)
    }

    pub fn recover(&mut self, from: Origin, view: u64, reload: bool) -> Outcome {
        let body = wire::encode_recover_args(&wire::RecoverArgs { view, reload }).unwrap();
        self.call(from, wire::METHOD_RECOVER, body)
    }

    pub fn recover_group(&mut self, from: Origin, view: u64) -> Outcome {
        let body = wire::encode_recover_group_args(&wire::RecoverGroupArgs { view }).unwrap();
        self.call(from, wire::METHOD_RECOVERGROUP, body)
    }

    pub fn detach_slots(&mut self, from: Origin, view: u64) -> Outcome {
        let body = wire::encode_detach_slots_args(&wire::DetachSlotsArgs { view }).unwrap();
        self.call(from, wire::METHOD_DETACHSLOTS, body)
    }

    pub fn set_user_agent(&mut self, from: Origin, view: u64, agent: &str) -> Outcome {
        let args = wire::SetUserAgentArgs {
            view,
            user_agent: agent.into(),
        };
        let body = wire::encode_set_user_agent_args(&args).unwrap();
        self.call(from, wire::METHOD_SETUSERAGENT, body)
    }

    pub fn set_blocking(
        &mut self,
        from: Origin,
        view: u64,
        enabled: bool,
        exceptions: &[&str],
    ) -> Outcome {
        let args = wire::SetBlockingArgs {
            view,
            enabled,
            exceptions: exceptions.iter().map(|e| (*e).to_owned()).collect(),
        };
        let body = wire::encode_set_blocking_args(&args).unwrap();
        self.call(from, wire::METHOD_SETBLOCKING, body)
    }

    pub fn blocking_compile(
        &mut self,
        from: Origin,
        view: u64,
        list: &str,
        json: &[u8],
    ) -> Outcome {
        let (handle, _) = self.rig.fabric.new_buffer_with(json.to_vec());
        let rules = Buffer::whole(handle, json.len() as u64);
        self.blocking_compile_buffer(from, view, list, rules)
    }

    pub fn blocking_compile_buffer(
        &mut self,
        from: Origin,
        view: u64,
        list: &str,
        rules: Buffer,
    ) -> Outcome {
        let args = wire::BlockingCompileArgs {
            view,
            list: list.into(),
            rules,
        };
        let (body, objects) = wire::encode_blocking_compile_args(&args).unwrap();
        let method = wire::METHOD_BLOCKINGCOMPILE;
        self.send(from, wire::INTERFACE_ID, method, body, objects)
    }

    pub fn blocking_load(&mut self, from: Origin, view: u64, list: &str) -> Outcome {
        let args = wire::BlockingLoadArgs {
            view,
            list: list.into(),
        };
        let body = wire::encode_blocking_load_args(&args).unwrap();
        self.call(from, wire::METHOD_BLOCKINGLOAD, body)
    }

    pub fn blocking_forget(&mut self, from: Origin, view: u64, list: &str) -> Outcome {
        let args = wire::BlockingForgetArgs {
            view,
            list: list.into(),
        };
        let body = wire::encode_blocking_forget_args(&args).unwrap();
        self.call(from, wire::METHOD_BLOCKINGFORGET, body)
    }

    /// A mouse event (`kind`: 0 move, 1 press, 2 release).
    pub fn mouse(&mut self, from: Origin, view: u64, kind: u32) -> Outcome {
        let args = wire::MouseArgs {
            view,
            kind,
            button: 0,
            x: 1,
            y: 1,
            mods: 0,
        };
        let body = wire::encode_mouse_args(&args).unwrap();
        self.call(from, wire::METHOD_MOUSE, body)
    }

    pub fn key(&mut self, from: Origin, view: u64, press: bool) -> Outcome {
        let args = wire::KeyArgs {
            view,
            press,
            keysym: 0x61,
            text: "a".into(),
            mods: 0,
        };
        let body = wire::encode_key_args(&args).unwrap();
        self.call(from, wire::METHOD_KEY, body)
    }
}
