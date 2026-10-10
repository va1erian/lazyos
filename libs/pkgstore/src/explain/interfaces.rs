//! The interface rows of the explanation table, one per interface in
//! `idl/` (a test in [`super`] holds it to `idl/manifest.json`).

use super::{HIGH, LOW, MEDIUM};

/// `(interface name, risk, sentence)` for every interface in `idl/`.
pub const INTERFACES: &[(&str, &str, &str)] = &[
    (
        "os.lazy.accounts.v1",
        HIGH,
        "See the user accounts and change your own password",
    ),
    (
        "os.lazy.audio.v1",
        MEDIUM,
        "Play sound through the speakers",
    ),
    (
        "os.lazy.audio.mixer.v1",
        MEDIUM,
        "Change the volume of every app's sound and the master volume",
    ),
    (
        "os.lazy.clipboard.v1",
        MEDIUM,
        "Read and change what you copy and paste",
    ),
    ("os.lazy.confd.v1", HIGH, "Read and change system settings"),
    (
        "os.lazy.display.v1",
        LOW,
        "Show its own windows on the desktop",
    ),
    (
        "os.lazy.display.prompt.v1",
        HIGH,
        "Show the administrator prompt, which only the elevation service may do",
    ),
    (
        "os.lazy.echo.v1",
        LOW,
        "Use the echo test service, which only repeats what it is sent",
    ),
    (
        "os.lazy.elevd.v1",
        MEDIUM,
        "Ask an administrator to approve a system change, such as adding a user or changing a system setting",
    ),
    (
        "os.lazy.files.v1",
        LOW,
        "See which files you have selected in Files",
    ),
    (
        "os.lazy.healthd.v1",
        LOW,
        "See whether the system's services are healthy",
    ),
    (
        "os.lazy.init.v1",
        HIGH,
        "Start programs and see which services are running",
    ),
    (
        "os.lazy.input.v1",
        LOW,
        "Receive keyboard input while its window is focused",
    ),
    (
        "os.lazy.input.shell.v1",
        HIGH,
        "Control how keyboard input is routed for the whole desktop",
    ),
    (
        "os.lazy.keyd.v1",
        HIGH,
        "Use the stored secrets and encryption keys",
    ),
    (
        "os.lazy.lifecycle.v1",
        HIGH,
        "Ask system services to save their state and stop, which they only obey from the system supervisor",
    ),
    (
        "os.lazy.logd.v1",
        MEDIUM,
        "Read the system event log, which records what other apps did",
    ),
    (
        "os.lazy.logind.v1",
        MEDIUM,
        "See who is logged in and which sessions exist",
    ),
    (
        "os.lazy.mimed.v1",
        MEDIUM,
        "Look up which app opens a file type and open files with other apps",
    ),
    (
        "os.lazy.mount.v1",
        HIGH,
        "Connect to file servers on the network and mount them as folders",
    ),
    (
        "os.lazy.net.nic.v1",
        HIGH,
        "Control the network card directly",
    ),
    (
        "os.lazy.net.stack.v1",
        HIGH,
        "Read and change the network configuration",
    ),
    (
        "os.lazy.net.socket.v1",
        HIGH,
        "Open network connections to other computers",
    ),
    (
        "os.lazy.net.wifi.hw.v1",
        HIGH,
        "Control the Wi-Fi radio directly",
    ),
    (
        "os.lazy.net.wifi.v1",
        HIGH,
        "Scan for Wi-Fi networks and join or leave them",
    ),
    ("os.lazy.pkgd.v1", HIGH, "Install and remove applications"),
    (
        "os.lazy.print.v1",
        MEDIUM,
        "Print documents on printers on your network",
    ),
    (
        "os.lazy.messenger.policy.v1",
        HIGH,
        "Change what other apps are allowed to do",
    ),
    (
        "os.lazy.messenger.names.resolve.v1",
        HIGH,
        "Look up any system service by name",
    ),
    (
        "os.lazy.messenger.registry.v1",
        HIGH,
        "Publish system services and list the ones that exist",
    ),
    (
        "os.lazy.shell.v1",
        MEDIUM,
        "See your open windows, switch between them, open the start menu, launch apps and refresh the menu and desktop icons",
    ),
    ("os.lazy.sysmond.v1", LOW, "Read CPU and memory statistics"),
    ("os.lazy.timed.v1", LOW, "Read the time and the time zone"),
    (
        "os.lazy.messenger.topics.v1",
        MEDIUM,
        "Send and receive messages on topics",
    ),
    (
        "os.lazy.messenger.topics.bell.v1",
        LOW,
        "Be told when messages arrive on topics it listens to",
    ),
    (
        "os.lazy.messenger.topics.publish.v1",
        HIGH,
        "Pass the system's internal check for sending messages on any topic",
    ),
    (
        "os.lazy.messenger.topics.subscribe.v1",
        HIGH,
        "Pass the system's internal check for listening on any topic",
    ),
    (
        "os.lazy.devd.v1",
        LOW,
        "See which hardware devices the system has found",
    ),
    ("os.lazy.process.label.spawn.v1", HIGH, DEVELOP),
    (
        "os.lazy.shell.tray.v1",
        LOW,
        "Show an icon in the taskbar, with a tooltip and a menu",
    ),
    (
        "os.lazy.shell.tray.events.v1",
        LOW,
        "Be told when you click its taskbar icon or pick from its menu",
    ),
    (
        "os.lazy.init.app.v1",
        LOW,
        "Be told when you open it again or ask it to quit",
    ),
    (
        "os.lazy.init.app.events.v1",
        LOW,
        "Receive the requests to open again or to quit",
    ),
];

/// What `develop = true` lets an app do (issue #529).
pub const DEVELOP: &str = "Run apps you are developing, with permissions you approve";
