//! Net Tools' window: four group boxes (Ping, Look up a name, Fetch a web
//! page, Web server) under a one-line network headline. Mounted once; the app
//! only changes texts and list rows afterwards.

use xui_core::prelude::*;

use super::Msg;

/// The window size when a compositor lays the app out.
pub const WINDOW: (i32, i32) = (720, 590);

/// The width of the captions in front of the fields.
const CAPTION: i32 = 52;
/// The height of a one-line result.
const RESULT: i32 = 20;

/// What the app changes after start-up.
#[derive(Default)]
pub struct Widgets {
    pub headline: Handle<Label<Msg>>,
    pub host: Handle<Edit<Msg>>,
    pub ping_result: Handle<Label<Msg>>,
    pub name: Handle<Edit<Msg>>,
    pub lookup_result: Handle<Label<Msg>>,
    pub url: Handle<Edit<Msg>>,
    pub fetch_result: Handle<Label<Msg>>,
    pub preview: Handle<ListView<Msg>>,
    pub server_status: Handle<Label<Msg>>,
    pub server_button: Handle<Button<Msg>>,
    pub server_log: Handle<ListView<Msg>>,
}

/// A caption in front of a field, centred on the row.
fn caption(text: &str) -> Entry<Msg> {
    label(text).width(CAPTION).align(Align::Center)
}

/// A plain list of text rows.
fn rows(handle: &Handle<ListView<Msg>>) -> Build<ListView<Msg>, Msg> {
    list().then(|list| list.multi_select(false)).bind(handle)
}

impl Widgets {
    /// The whole window. The sizes keep the buttons and the URL field where
    /// the `net_apps` session clicks.
    pub fn layout(&self) -> Layout<Msg> {
        column()
            .padding(Insets::new(Dip(12.0), Dip(8.0), Dip(12.0), Dip(10.0)))
            .gap(8)
            .children((
                label("Reading the network stack...").bind(&self.headline),
                group(
                    "Ping",
                    column().gap(6).children((
                        row().gap(8).children((
                            caption("Host"),
                            edit()
                                .text("10.0.2.2")
                                .placeholder("address or name")
                                .bind(&self.host)
                                .width(222),
                            button("Ping").on_click(Msg::Ping).width(80),
                            button("Stop").on_click(Msg::StopPing).width(70),
                        )),
                        label("").bind(&self.ping_result).fixed(RESULT),
                    )),
                ),
                group(
                    "Look up a name",
                    row().gap(8).children((
                        caption("Name"),
                        edit()
                            .text("example.com")
                            .placeholder("host name")
                            .bind(&self.name)
                            .width(222),
                        button("Look up").on_click(Msg::Lookup).width(90),
                        label("")
                            .bind(&self.lookup_result)
                            .fill(1)
                            .align(Align::Center),
                    )),
                ),
                group(
                    "Fetch a web page",
                    column().gap(6).children((
                        row().gap(8).children((
                            caption("URL"),
                            edit()
                                .text("http://example.com/")
                                .placeholder("http://host/path")
                                .bind(&self.url)
                                .width(330),
                            button("Fetch").on_click(Msg::Fetch).width(80),
                        )),
                        label("").bind(&self.fetch_result).fixed(RESULT),
                        rows(&self.preview).fixed(68),
                    )),
                ),
                group(
                    "Web server: reach this machine from the host",
                    column().gap(6).children((
                        row().gap(8).children((
                            label("Starting...").bind(&self.server_status).fill(1),
                            button("Stop")
                                .on_click(Msg::ToggleServer)
                                .bind(&self.server_button)
                                .width(90)
                                .align(Align::Start),
                        )),
                        rows(&self.server_log).fill(1),
                    )),
                )
                .fill(1),
            ))
    }
}
