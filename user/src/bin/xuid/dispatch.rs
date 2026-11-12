//! The entry of `xuid`'s requests: which generated `dispatch` serves a message
//! (the display interface, its cursor scope or the trusted prompt), and the
//! two servers that are not the display's own, the cursor scope and the
//! prompt, whose answer comes later.

use alloc::string::String;
use libmessenger::server::{Caller, Fault};
use libmessenger::Parcel;
use messenger_generated::os_lazy_display_cursor_v1 as cursor_wire;
use messenger_generated::os_lazy_display_prompt_v1 as prompt_wire;
use user::messenger::display::wire;
use user::messenger::Message;

use super::compositor::Compositor;
use super::cursor_request::apply_set_cursor;
use super::protocol::refuse;

/// The reply to `message`, or `None` when it is sent later (the prompt's).
pub(super) fn serve(comp: &mut Compositor, message: &Message) -> Option<Parcel> {
    match message.interface_id() {
        prompt_wire::INTERFACE_ID => {
            let mut call = PromptCall {
                comp,
                txn: message.txn,
                held: false,
            };
            let reply = prompt_wire::dispatch(&mut call, message);
            (!call.held).then_some(reply)
        }
        cursor_wire::INTERFACE_ID => Some(cursor_wire::dispatch(comp, message)),
        _ => Some(wire::dispatch(comp, message)),
    }
}

/// One `Prompt` request: `elevd` waits for the person at the screen.
struct PromptCall<'a> {
    comp: &'a mut Compositor,
    txn: Option<u64>,
    /// The reply is sent later (`take_prompt_reply`): nothing answers now.
    held: bool,
}

impl prompt_wire::Server for PromptCall<'_> {
    fn prompt(
        &mut self,
        cx: &Caller,
        summary: String,
        uid: u32,
        user: String,
        label_id: u32,
        admin: String,
        error: String,
    ) -> Result<prompt_wire::PromptReply, Fault> {
        let args = prompt_wire::PromptArgs {
            summary,
            uid,
            user,
            label_id,
            admin,
            error,
        };
        self.comp.open_prompt(cx, self.txn, args)?;
        self.held = true;
        Err(Fault::unserved())
    }
}

impl cursor_wire::Server for Compositor {
    fn set_cursor(&mut self, cx: &Caller, id: u64, shape: u32) -> Result<(), Fault> {
        if apply_set_cursor(&mut self.surfaces, cx.sender, id, shape).map_err(refuse)? {
            // The pointer may be over this surface right now.
            self.move_cursor();
        }
        Ok(())
    }
}
