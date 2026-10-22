//! The events of the full interface, decoded for a test client (`FEv`), and the reader on `Fx`.

#![allow(dead_code)]

use messenger_generated::os_lazy_webd_view_v1 as wire;

use super::full::Fx;

/// An event of the full interface, decoded.
#[derive(Clone, Debug, PartialEq)]
pub enum FEv {
    Frame(wire::FrameArgs),
    State(wire::StateArgs),
    Scroll(wire::ScrollArgs),
    Link(wire::LinkClickedArgs),
    Clipboard(wire::ClipboardWriteArgs),
    Lost(wire::ViewLostArgs),
    Ping,
    Band(wire::BandArgs),
    Hover(wire::HoverArgs),
    Cursor(wire::CursorArgs),
    NewWindow(wire::NewWindowRequestedArgs),
    Navigation(wire::NavigationRequestedArgs),
    Dialog(wire::DialogArgs),
    DownloadStarted(wire::DownloadStartedArgs),
    DownloadProgress(wire::DownloadProgressArgs),
    DownloadDone(wire::DownloadDoneArgs),
    DownloadFailed(wire::DownloadFailedArgs),
    NotifyAsk(wire::NotifyPermissionRequestArgs),
    NotifyClicked(wire::NotifyClickedArgs),
    BlockingList(wire::BlockingListArgs),
    Blocked(wire::BlockedArgs),
    Responsive(wire::ResponsiveArgs),
    HandedOff(wire::HandedOffArgs),
    NotifyShow(wire::NotifyShowArgs),
    NotifyCancel(wire::NotifyCancelArgs),
    NotifyGone(wire::NotifyGoneArgs),
}

impl FEv {
    /// The view the event is about (`None` for a ping).
    pub fn view(&self) -> Option<u64> {
        Some(match self {
            FEv::Frame(e) => e.view,
            FEv::State(e) => e.view,
            FEv::Scroll(e) => e.view,
            FEv::Link(e) => e.view,
            FEv::Clipboard(e) => e.view,
            FEv::Lost(e) => e.view,
            FEv::Ping => return None,
            FEv::Band(e) => e.view,
            FEv::Hover(e) => e.view,
            FEv::Cursor(e) => e.view,
            FEv::NewWindow(e) => e.view,
            FEv::Navigation(e) => e.view,
            FEv::Dialog(e) => e.view,
            FEv::DownloadStarted(e) => e.view,
            FEv::DownloadProgress(e) => e.view,
            FEv::DownloadDone(e) => e.view,
            FEv::DownloadFailed(e) => e.view,
            FEv::NotifyAsk(e) => e.view,
            FEv::NotifyClicked(e) => e.view,
            FEv::BlockingList(e) => e.view,
            FEv::Blocked(e) => e.view,
            FEv::Responsive(e) => e.view,
            FEv::HandedOff(e) => e.view,
            FEv::NotifyShow(e) => e.view,
            FEv::NotifyCancel(e) => e.view,
            FEv::NotifyGone(e) => e.view,
        })
    }
}

pub fn decode_full_event(method: u32, body: &[u8]) -> FEv {
    match method {
        wire::METHOD_FRAME => FEv::Frame(wire::decode_frame_args(body).unwrap()),
        wire::METHOD_STATE => FEv::State(wire::decode_state_args(body).unwrap()),
        wire::METHOD_SCROLL => FEv::Scroll(wire::decode_scroll_args(body).unwrap()),
        wire::METHOD_LINKCLICKED => FEv::Link(wire::decode_link_clicked_args(body).unwrap()),
        wire::METHOD_CLIPBOARDWRITE => {
            FEv::Clipboard(wire::decode_clipboard_write_args(body).unwrap())
        }
        wire::METHOD_VIEWLOST => FEv::Lost(wire::decode_view_lost_args(body).unwrap()),
        wire::METHOD_PING => FEv::Ping,
        wire::METHOD_BAND => FEv::Band(wire::decode_band_args(body).unwrap()),
        wire::METHOD_HOVER => FEv::Hover(wire::decode_hover_args(body).unwrap()),
        wire::METHOD_CURSOR => FEv::Cursor(wire::decode_cursor_args(body).unwrap()),
        wire::METHOD_NEWWINDOWREQUESTED => {
            FEv::NewWindow(wire::decode_new_window_requested_args(body).unwrap())
        }
        wire::METHOD_NAVIGATIONREQUESTED => {
            FEv::Navigation(wire::decode_navigation_requested_args(body).unwrap())
        }
        wire::METHOD_DIALOG => FEv::Dialog(wire::decode_dialog_args(body).unwrap()),
        wire::METHOD_DOWNLOADSTARTED => {
            FEv::DownloadStarted(wire::decode_download_started_args(body).unwrap())
        }
        wire::METHOD_DOWNLOADPROGRESS => {
            FEv::DownloadProgress(wire::decode_download_progress_args(body).unwrap())
        }
        wire::METHOD_DOWNLOADDONE => {
            FEv::DownloadDone(wire::decode_download_done_args(body).unwrap())
        }
        wire::METHOD_DOWNLOADFAILED => {
            FEv::DownloadFailed(wire::decode_download_failed_args(body).unwrap())
        }
        wire::METHOD_NOTIFYPERMISSIONREQUEST => {
            FEv::NotifyAsk(wire::decode_notify_permission_request_args(body).unwrap())
        }
        wire::METHOD_NOTIFYCLICKED => {
            FEv::NotifyClicked(wire::decode_notify_clicked_args(body).unwrap())
        }
        wire::METHOD_BLOCKINGLIST => {
            FEv::BlockingList(wire::decode_blocking_list_args(body).unwrap())
        }
        wire::METHOD_BLOCKED => FEv::Blocked(wire::decode_blocked_args(body).unwrap()),
        wire::METHOD_RESPONSIVE => FEv::Responsive(wire::decode_responsive_args(body).unwrap()),
        wire::METHOD_HANDEDOFF => FEv::HandedOff(wire::decode_handed_off_args(body).unwrap()),
        wire::METHOD_NOTIFYSHOW => FEv::NotifyShow(wire::decode_notify_show_args(body).unwrap()),
        wire::METHOD_NOTIFYCANCEL => {
            FEv::NotifyCancel(wire::decode_notify_cancel_args(body).unwrap())
        }
        wire::METHOD_NOTIFYGONE => FEv::NotifyGone(wire::decode_notify_gone_args(body).unwrap()),
        other => panic!("not an event of the full interface: {other}"),
    }
}

impl Fx {
    /// The events the service sent on `channel` since the last call, each checked to have gone out
    /// as the full interface (a full view's events never use the light interface's id).
    pub fn full_events(&mut self, channel: u64) -> Vec<FEv> {
        self.rig
            .fabric
            .take_tagged(channel)
            .into_iter()
            .map(|(interface, method, body)| {
                assert_eq!(
                    interface,
                    wire::INTERFACE_ID,
                    "a full view's event, method {method}"
                );
                decode_full_event(method, &body)
            })
            .collect()
    }

    /// `full_events` without the liveness pings.
    pub fn news(&mut self, channel: u64) -> Vec<FEv> {
        let mut events = self.full_events(channel);
        events.retain(|event| *event != FEv::Ping);
        events
    }
}
