//! `xui-greeter`: the graphical login screen (issue #623,
//! docs/accounts-plan.md U0).
//!
//! `logind` has `init` start it on a desktop image whenever no desktop
//! session is open: at boot (unless the image logs straight in,
//! `LAZYOS_AUTOLOGIN`) and after every logout. It runs as the `_greeter`
//! system uid with no capability and no session, so it can do exactly one
//! thing of note: ask `logind` to `Login(name, password)`. `logind` accepts
//! that call from this identity alone, checks the password through
//! `accountsd` and `keyd`, opens the session (LazyShell and the session's
//! autostart apps, as the user) and then stops this program. A refused
//! password clears the field; `logind` already made the attempt wait.
//!
//! The window cannot be closed (there would be no way to log in), and it
//! never shows a password: the field is masked and nothing here prints one.
//!
//! Serial evidence: `GREETER:UP:PASS` after the first frame;
//! `GREETER:LOGIN:PASS user=<name> session=<id>` or
//! `GREETER:LOGIN:FAIL user=<name> errno=<e>`.

use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_logind_v1 as wire;
use xui_app::launch;
use xui_app::platform::messenger::Service;
use xui_app::sys::{self, errno};
use xui_core::app::{App, Ui};
use xui_core::prelude::*;

/// The window size when the compositor lays it out (design pixels).
const WINDOW: (i32, i32) = (420, 250);
/// `logind`'s registered name.
const LOGIND: &str = "os.lazy.logind";
/// How long a login may take (PIT ticks, 100 Hz): the Argon2id check, the
/// failed-login delay and starting the desktop shell under emulation.
const LOGIN_TICKS: u64 = 3000;
/// The width of the field captions and of the fields.
const CAPTION_W: i32 = 90;
const FIELD_W: i32 = 240;

#[derive(Clone)]
enum Msg {
    Login,
    /// The close button: a login screen stays.
    Close,
}

#[derive(Default)]
struct Widgets {
    name: Handle<Edit<Msg>>,
    password: Handle<Edit<Msg>>,
    message: Handle<Label<Msg>>,
}

impl Widgets {
    fn layout(&self) -> Layout<Msg> {
        let field = |caption: &str, edit: Build<Edit<Msg>, Msg>| {
            row().gap(8).children((
                label(caption).width(CAPTION_W).align(Align::Center),
                edit.width(FIELD_W),
            ))
        };
        column()
            .padding(Insets::new(Dip(20.0), Dip(16.0), Dip(20.0), Dip(12.0)))
            .gap(10)
            .children((
                label("Welcome to LazyOS. Log in to start your desktop."),
                field("Name", edit().placeholder("user name").bind(&self.name)),
                field(
                    "Password",
                    edit()
                        .password()
                        .placeholder("password")
                        .bind(&self.password),
                ),
                row().gap(8).children((
                    label("").width(CAPTION_W),
                    button("Log in").on_click(Msg::Login).width(100),
                )),
                label("").bind(&self.message).fixed(40),
            ))
    }
}

struct Greeter {
    w: Widgets,
}

impl App for Greeter {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, _ui: &mut Ui<Msg>) {
        match msg {
            Msg::Login => self.login(),
            Msg::Close => self
                .w
                .message
                .get()
                .set_text("Log in to use the desktop, or shut down from the console."),
        }
    }
}

impl Greeter {
    fn login(&mut self) {
        let name = self.w.name.get().text().trim().to_string();
        if name.is_empty() {
            self.w.message.get().set_text("Type your user name.");
            return;
        }
        let secret = self.w.password.get().text();
        self.w.message.get().set_text("Checking...");
        match call_login(&name, &secret) {
            Ok(session) => {
                println!("GREETER:LOGIN:PASS user={name} session={session}");
                self.w.message.get().set_text("Starting your desktop...");
            }
            Err(code) => {
                println!("GREETER:LOGIN:FAIL user={name} errno={}", -code);
                self.w.password.get().set_text("");
                self.w.message.get().set_text(refusal(code));
            }
        }
    }
}

/// `logind.Login`: the new session's id, or the negative errno.
fn call_login(user: &str, secret: &str) -> std::result::Result<u64, i64> {
    let service = Service::try_connect(LOGIND).ok_or(-errno::ENOENT)?;
    let body = wire::encode_login_args(&wire::LoginArgs {
        user: user.to_string(),
        secret: secret.to_string(),
    })
    .map_err(|_| -errno::EINVAL)?;
    let deadline = sys::clock_ticks().saturating_add(LOGIN_TICKS);
    let reply = service.call_until(
        wire::INTERFACE_ID,
        wire::METHOD_LOGIN,
        ERROR_FIELD,
        body,
        deadline,
    )?;
    wire::decode_login_reply(&reply.body)
        .map(|reply| reply.session)
        .map_err(|_| -errno::EINVAL)
}

/// What the user reads for a refused login.
fn refusal(code: i64) -> &'static str {
    match -code {
        errno::EACCES => "Wrong user name or password.",
        errno::EBUSY => "A desktop session is already open.",
        errno::ENOENT => "The login service is not running yet; try again.",
        errno::ETIMEDOUT => "The login service did not answer; try again.",
        _ => "Login failed; see the system log.",
    }
}

fn main() {
    launch::run("GREETER", "Log in", WINDOW, |ui, backend| {
        backend.on_first_frame(|| println!("GREETER:UP:PASS"));
        let w = Widgets::default();
        ui.root(w.layout())
            .inspect_err(|error| println!("GREETER:BUILD:FAIL:{error}"))?;
        ui.on_close(|| Some(Msg::Close));
        Ok(Greeter { w })
    })
}
