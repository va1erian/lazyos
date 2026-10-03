//! Net Tools' window: four group boxes (Ping, Look up a name, Fetch a web
//! page, Web server) under a one-line network headline. Built once; the app
//! only changes texts and list rows afterwards.

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{Button, Edit, GroupBox, Label, ListView};
use xui_core::Rect;

use super::Msg;

/// The window size when a compositor lays the app out.
pub const WINDOW: (i32, i32) = (720, 590);

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect::new(x, y, x + w, y + h)
}

/// What the app changes after building.
pub struct Widgets {
    pub headline: Label<Msg>,
    pub host: Edit<Msg>,
    pub ping_result: Label<Msg>,
    pub name: Edit<Msg>,
    pub lookup_result: Label<Msg>,
    pub url: Edit<Msg>,
    pub fetch_result: Label<Msg>,
    pub preview: ListView<Msg>,
    pub server_status: Label<Msg>,
    pub server_button: Button<Msg>,
    pub server_log: ListView<Msg>,
    _keep: (Vec<Label<Msg>>, Vec<Button<Msg>>, Vec<GroupBox<Msg>>),
}

impl Widgets {
    pub fn build(ui: &Ui<Msg>) -> Result<Widgets> {
        let width = ui.client_rect().width().max(WINDOW.0);
        let inner = width - 24;
        let groups = vec![
            GroupBox::new(ui, rect(12, 38, inner, 92), "Ping")?,
            GroupBox::new(ui, rect(12, 138, inner, 66), "Look up a name")?,
            GroupBox::new(ui, rect(12, 212, inner, 160), "Fetch a web page")?,
            GroupBox::new(
                ui,
                rect(12, 380, inner, 200),
                "Web server: reach this machine from the host",
            )?,
        ];
        let labels = vec![
            Label::new(ui, rect(28, 66, 50, 20), "Host")?,
            Label::new(ui, rect(28, 166, 50, 20), "Name")?,
            Label::new(ui, rect(28, 240, 50, 20), "URL")?,
        ];
        let headline = Label::new(ui, rect(16, 10, inner, 22), "Reading the network stack...")?;
        let host = Edit::new(ui, rect(80, 62, 220, 26), "10.0.2.2")?.cue("address or name");
        let name = Edit::new(ui, rect(80, 162, 220, 26), "example.com")?.cue("host name");
        let url =
            Edit::new(ui, rect(80, 236, 330, 26), "http://example.com/")?.cue("http://host/path");
        let buttons = vec![
            Button::new(ui, rect(310, 61, 80, 28), "Ping")?.on_click(|| Some(Msg::Ping)),
            Button::new(ui, rect(398, 61, 70, 28), "Stop")?.on_click(|| Some(Msg::StopPing)),
            Button::new(ui, rect(310, 161, 90, 28), "Look up")?.on_click(|| Some(Msg::Lookup)),
            Button::new(ui, rect(420, 235, 80, 28), "Fetch")?.on_click(|| Some(Msg::Fetch)),
        ];
        let ping_result = Label::new(ui, rect(28, 98, inner - 32, 20), "")?;
        let lookup_result = Label::new(ui, rect(410, 166, inner - 400, 20), "")?;
        let fetch_result = Label::new(ui, rect(28, 270, inner - 32, 20), "")?;
        let preview = ListView::new(ui, rect(28, 294, inner - 32, 68), &[])?.multi_select(false);
        let server_status = Label::new(ui, rect(28, 404, inner - 140, 40), "Starting...")?;
        let server_button = Button::new(ui, rect(inner - 92, 404, 90, 28), "Stop")?
            .on_click(|| Some(Msg::ToggleServer));
        let server_log =
            ListView::new(ui, rect(28, 450, inner - 32, 120), &[])?.multi_select(false);
        Ok(Widgets {
            headline,
            host,
            ping_result,
            name,
            lookup_result,
            url,
            fetch_result,
            preview,
            server_status,
            server_button,
            server_log,
            _keep: (labels, buttons, groups),
        })
    }
}
