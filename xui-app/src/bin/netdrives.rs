//! `xui-netdrives`: the Network Drives app. The top half connects to an FTP
//! server (host, port, user, empty for the anonymous login, and password) or
//! an SMB share (the same plus the share's name; docs/smb-plan.md F3), and
//! names the folder it is served at, `/mnt/<name>`. The bottom half lists every mount
//! the network mount service (`mountd`, `os.lazy.mount.v1`) knows about,
//! refreshed every second, with its state (connecting, mounted, or failed
//! and why); "Open in Files" (or a double-click) opens a mounted folder,
//! "Unmount" stops one.
//!
//! The app holds no filesystem authority: `mountd` starts the `ftpfuse` or
//! `smbfuse` daemon that serves the folder (docs/smb-plan.md §3.4), and the
//! folder's
//! files are owned by the user who ran the app. The form is checked with
//! `mountd`'s own rules first (`xui_app::net::drives`), so a request the
//! service would refuse is refused here with the reason.
//!
//! Serial evidence: `NETDRIVES:UP:PASS` after the first frame;
//! `NETDRIVES:MOUNT:REQUESTED name=<n> path=<p>` or
//! `NETDRIVES:MOUNT:REFUSED <why>` on Mount; `NETDRIVES:MOUNT:PASS name=<n>
//! path=<p>` once a mount is up and `NETDRIVES:MOUNT:FAIL name=<n>
//! reason=<why>` once one failed; `NETDRIVES:UNMOUNT:PASS|FAIL name=<n>`;
//! `NETDRIVES:OPEN:PASS|FAIL path=<p>`; `NETDRIVES:LIST:NOSERVICE` once
//! when `mountd` is absent; `NETDRIVES:CLOSE:PASS` on close. With the UI
//! probe on, `UI:WIDGET` lines name `kind_ftp`, `kind_smb`, `host`, `port`,
//! `share`, `user`, `password`, `name`, `mount_button`, `mounts`,
//! `open_button` and `unmount_button`.

use std::collections::HashMap;

use mounttable::Kind;
use xui_app::launch;
use xui_app::net::drives::{self, Form};
use xui_app::net::mounts::{self, MountInfo};
use xui_app::probe;
use xui_core::prelude::*;

/// The window title, which the UI probe lines name.
const TITLE: &str = "Network Drives";
/// The window size when a compositor lays the app out.
const WINDOW: (i32, i32) = (640, 580);
/// The protocol choices, in radio order.
const KINDS: [Kind; 2] = [Kind::Ftp, Kind::Smb];
/// How often the mount list refreshes.
const REFRESH_MILLIS: u32 = 1000;

#[derive(Clone)]
enum Msg {
    Tick,
    Kind(usize),
    Mount,
    Open,
    Unmount,
    Close,
}

/// Every widget the app reads or changes after start-up.
#[derive(Default)]
struct Widgets {
    kind: Handle<RadioGroup<Msg>>,
    host: Handle<Edit<Msg>>,
    port: Handle<Edit<Msg>>,
    share: Handle<Edit<Msg>>,
    user: Handle<Edit<Msg>>,
    password: Handle<Edit<Msg>>,
    name: Handle<Edit<Msg>>,
    mount: Handle<Button<Msg>>,
    mounts: Handle<ListView<Msg>>,
    open: Handle<Button<Msg>>,
    unmount: Handle<Button<Msg>>,
    message: Handle<Label<Msg>>,
}

/// A caption and its field, one form row.
fn field(caption: &str, edit: Build<Edit<Msg>, Msg>, width: i32) -> Entry<Msg> {
    row()
        .gap(8)
        .children((
            label(caption).width(80).align(Align::Center),
            edit.width(width),
        ))
        .into_entry()
}

impl Widgets {
    fn layout(&self) -> Layout<Msg> {
        let server = column().gap(8).children((
            row().gap(8).children((
                label("Protocol").width(80),
                radio_group(&["FTP server", "SMB share (Windows, Samba)"])
                    .on_select(Msg::Kind)
                    .bind(&self.kind),
            )),
            row().gap(16).children((
                field(
                    "Server",
                    edit().placeholder("name or address").bind(&self.host),
                    220,
                ),
                field("Port", edit().placeholder("21/445").bind(&self.port), 80),
            )),
            field(
                "Share",
                edit().placeholder("SMB shares only").bind(&self.share),
                220,
            ),
            row().gap(16).children((
                field(
                    "User",
                    edit().placeholder("empty: anonymous FTP").bind(&self.user),
                    220,
                ),
                field("Password", edit().password().bind(&self.password), 160),
            )),
            row().gap(16).children((
                field(
                    "Name",
                    edit().placeholder("ftp, or the share").bind(&self.name),
                    220,
                ),
                button("Mount")
                    .on_click(Msg::Mount)
                    .bind(&self.mount)
                    .width(100),
            )),
        ));
        column()
            .padding(Insets::new(Dip(12.0), Dip(8.0), Dip(12.0), Dip(8.0)))
            .gap(8)
            .children((
                group("Connect to a server", server),
                group(
                    "Mounted folders",
                    column().gap(8).children((
                        list()
                            .column("Name", 90)
                            .column("Server", 190)
                            .column("Folder", 110)
                            .column("State", Fill)
                            .on_activate(|_| Msg::Open)
                            .bind(&self.mounts)
                            .fill(1),
                        row().gap(8).children((
                            button("Open in Files")
                                .on_click(Msg::Open)
                                .bind(&self.open)
                                .width(120),
                            button("Unmount")
                                .on_click(Msg::Unmount)
                                .bind(&self.unmount)
                                .width(100),
                        )),
                    )),
                )
                .fill(1),
                label("").bind(&self.message).fixed(40),
            ))
    }

    fn kind(&self) -> Kind {
        KINDS
            .get(self.kind.get().selected())
            .copied()
            .unwrap_or(Kind::Ftp)
    }

    fn form(&self) -> Form {
        Form {
            kind: self.kind(),
            host: self.host.get().text(),
            port: self.port.get().text(),
            share: self.share.get().text(),
            user: self.user.get().text(),
            password: self.password.get().text(),
            name: self.name.get().text(),
        }
    }

    /// Print where the named controls are (UI probe images only).
    fn probe(&self, ui: &Ui<Msg>) {
        let kinds = self.kind.get().ids();
        let rects = [
            ("kind_ftp", ui.bounds(kinds[0])),
            ("kind_smb", ui.bounds(kinds[1])),
            ("host", ui.bounds(self.host.get().id())),
            ("port", ui.bounds(self.port.get().id())),
            ("share", ui.bounds(self.share.get().id())),
            ("user", ui.bounds(self.user.get().id())),
            ("password", ui.bounds(self.password.get().id())),
            ("name", ui.bounds(self.name.get().id())),
            ("mount_button", ui.bounds(self.mount.get().id())),
            ("mounts", ui.bounds(self.mounts.get().id())),
            ("open_button", ui.bounds(self.open.get().id())),
            ("unmount_button", ui.bounds(self.unmount.get().id())),
        ];
        for (name, r) in rects {
            probe::widget(
                TITLE,
                name,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
            );
        }
    }

    fn say(&self, text: &str) {
        self.message.get().set_text(text);
    }
}

struct Netdrives {
    w: Widgets,
    /// The table as last listed, in row order.
    mounts: Vec<MountInfo>,
    /// Each mount's state as last reported on serial.
    reported: HashMap<String, String>,
    /// Whether the service was reachable at the last refresh.
    reachable: Option<bool>,
    probed: bool,
}

impl App for Netdrives {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick => {
                self.refresh();
                if !self.probed {
                    // After the first frame, once the layout has its sizes.
                    self.w.probe(ui);
                    self.probed = true;
                }
            }
            Msg::Kind(index) => {
                println!("NETDRIVES:KIND {}", KINDS[index.min(1)].name());
            }
            Msg::Mount => self.mount(),
            Msg::Open => self.open(),
            Msg::Unmount => self.unmount(),
            Msg::Close => {
                println!("NETDRIVES:CLOSE:PASS");
                ui.quit();
            }
        }
    }
}

impl Netdrives {
    /// List the mounts, keep the selection on the same name, and report
    /// every state change once.
    fn refresh(&mut self) {
        let mounts = match mounts::list() {
            Ok(mounts) => mounts,
            Err(error) => {
                if self.reachable != Some(false) {
                    println!("NETDRIVES:LIST:NOSERVICE {}", error.describe());
                    self.w.say(&error.describe());
                }
                self.reachable = Some(false);
                return;
            }
        };
        if self.reachable == Some(false) {
            self.w.say("");
        }
        self.reachable = Some(true);
        for info in &mounts {
            self.report(info);
        }
        let selected = self.selected().map(|info| info.name.clone());
        self.w
            .mounts
            .get()
            .refresh_model(mounts.iter().map(drives::row).collect::<Vec<_>>());
        let index = selected
            .and_then(|name| mounts.iter().position(|info| info.name == name))
            .or((!mounts.is_empty()).then_some(0));
        self.w.mounts.get().select(index);
        self.mounts = mounts;
    }

    fn report(&mut self, info: &MountInfo) {
        if self.reported.get(&info.name) == Some(&info.state) {
            return;
        }
        match info.state.as_str() {
            "mounted" => {
                println!("NETDRIVES:MOUNT:PASS name={} path={}", info.name, info.path);
                self.w
                    .say(&format!("{} is mounted at {}.", info.name, info.path));
            }
            "failed" => {
                println!(
                    "NETDRIVES:MOUNT:FAIL name={} reason={}",
                    info.name, info.detail
                );
                self.w
                    .say(&format!("{} failed: {}.", info.name, info.detail));
            }
            _ => {}
        }
        self.reported.insert(info.name.clone(), info.state.clone());
    }

    fn selected(&self) -> Option<&MountInfo> {
        self.w
            .mounts
            .get()
            .selected()
            .and_then(|index| self.mounts.get(index))
    }

    fn mount(&mut self) {
        let request = match drives::check(&self.w.form()) {
            Ok(request) => request,
            Err(why) => {
                println!("NETDRIVES:MOUNT:REFUSED {why}");
                self.w.say(&why);
                return;
            }
        };
        match mounts::mount(&request) {
            Ok(path) => {
                println!(
                    "NETDRIVES:MOUNT:REQUESTED name={} path={path}",
                    request.name
                );
                // The password lives on only in the daemon `mountd` started.
                self.w.password.get().set_text("");
                self.reported.remove(&request.name);
                self.w.say(&format!(
                    "Connecting to {}... The folder appears at {path} once logged in.",
                    request.host
                ));
                self.refresh();
            }
            Err(error) => {
                println!("NETDRIVES:MOUNT:REFUSED {}", error.describe());
                self.w.say(&error.describe());
            }
        }
    }

    fn open(&mut self) {
        let Some(info) = self.selected() else {
            self.w.say("Select a mounted folder first.");
            return;
        };
        if info.state != "mounted" {
            let text = format!(
                "{} is not mounted ({}).",
                info.name,
                drives::state_text(info)
            );
            self.w.say(&text);
            return;
        }
        let path = info.path.clone();
        match mounts::open_in_files(&path) {
            Ok(()) => println!("NETDRIVES:OPEN:PASS path={path}"),
            Err(error) => {
                println!("NETDRIVES:OPEN:FAIL path={path}");
                self.w
                    .say(&format!("Could not open Files: {}", error.describe()));
            }
        }
    }

    fn unmount(&mut self) {
        let Some(name) = self.selected().map(|info| info.name.clone()) else {
            self.w.say("Select a mount first.");
            return;
        };
        match mounts::unmount(&name) {
            Ok(()) => {
                println!("NETDRIVES:UNMOUNT:PASS name={name}");
                self.reported.remove(&name);
                self.w.say(&format!("{name} is unmounted."));
                self.refresh();
            }
            Err(error) => {
                println!("NETDRIVES:UNMOUNT:FAIL name={name}");
                self.w.say(&error.describe());
            }
        }
    }
}

fn main() {
    launch::run("NETDRIVES", TITLE, WINDOW, |ui, backend| {
        backend.on_first_frame(|| println!("NETDRIVES:UP:PASS"));
        let w = Widgets::default();
        ui.root(w.layout())
            .inspect_err(|error| println!("NETDRIVES:BUILD:FAIL:{error}"))?;
        ui.every(REFRESH_MILLIS, Msg::Tick);
        ui.on_close(|| Some(Msg::Close));
        let mut app = Netdrives {
            w,
            mounts: Vec::new(),
            reported: HashMap::new(),
            reachable: None,
            probed: false,
        };
        app.refresh();
        Ok(app)
    })
}
