//! Files the image build places on the boot volume, which code opens or
//! spawns by name. Names are uppercase 8.3 because the kernel's FAT reader has
//! no long-name support.
//!
//! Each program is `<STEM>_ELF`. Target (F3): lowercase binaries in
//! `/system/bin` (and `/system/etc`, `/system/share` for data), looked up by
//! the same constants.

/// The services-profile boot program (`init`). Written by the image build. Target (F3): "/system/bin/init".
pub const SUPER_ELF: &str = "SUPER.ELF";

/// The ABI bench's injected fixture, which owns the boot. Written by the image build. Target (F3): "/system/bin/init".
pub const INIT_ELF: &str = "INIT.ELF";

/// `messengerd`, the Messenger registry daemon. Written by the image build. Target (F3): "/system/bin/messengerd".
pub const MSGRD_ELF: &str = "MSGRD.ELF";

/// `keyd`, the key service. Written by the image build. Target (F3): "/system/bin/keyd".
pub const KEYD_ELF: &str = "KEYD.ELF";

/// `confd`, the configuration registry. Written by the image build. Target (F3): "/system/bin/confd".
pub const CONFD_ELF: &str = "CONFD.ELF";

/// `timed`, the time service. Written by the image build. Target (F3): "/system/bin/timed".
pub const TIMED_ELF: &str = "TIMED.ELF";

/// `inputd`, the input service. Written by the image build. Target (F3): "/system/bin/inputd".
pub const INPUTD_ELF: &str = "INPUTD.ELF";

/// `accountsd`, the accounts service. Written by the image build. Target (F3): "/system/bin/accountsd".
pub const ACCTD_ELF: &str = "ACCTD.ELF";

/// `logind`, the login service. Written by the image build. Target (F3): "/system/bin/logind".
pub const LOGIND_ELF: &str = "LOGIND.ELF";

/// `logd`, the log service. Written by the image build. Target (F3): "/system/bin/logd".
pub const LOGD_ELF: &str = "LOGD.ELF";

/// `healthd`, the health monitor. Written by the image build. Target (F3): "/system/bin/healthd".
pub const HEALTHD_ELF: &str = "HEALTHD.ELF";

/// `clipboardd`, the clipboard service. Written by the image build. Target (F3): "/system/bin/clipboardd".
pub const CLIPD_ELF: &str = "CLIPD.ELF";

/// `mimed`, the MIME database service. Written by the image build. Target (F3): "/system/bin/mimed".
pub const MIMED_ELF: &str = "MIMED.ELF";

/// `pkgd`, the package manager. Written by the image build. Target (F3): "/system/bin/pkgd".
pub const PKGD_ELF: &str = "PKGD.ELF";

/// The crash-test service. Written by the image build. Target (F3): "/system/bin/flaky".
pub const FLAKY_ELF: &str = "FLAKY.ELF";

/// `sndd`, the virtio-sound driver. Written by the image build. Target (F3): "/system/bin/sndd".
pub const SNDD_ELF: &str = "SNDD.ELF";

/// `netdrv`, the NIC driver. Written by the image build. Target (F3): "/system/bin/netdrv".
pub const NETDRV_ELF: &str = "NETDRV.ELF";

/// `netd`, the network stack service. Written by the image build. Target (F3): "/system/bin/netd".
pub const NETD_ELF: &str = "NETD.ELF";

/// `sysmond`, the system monitor service. Written by the image build. Target (F3): "/system/bin/sysmond".
pub const SYSD_ELF: &str = "SYSD.ELF";

/// `xuid`, the display compositor. Written by the image build. Target (F3): "/system/bin/xuid".
pub const XUID_ELF: &str = "XUID.ELF";

/// The `xuid` demo client. Written by the image build. Target (F3): "/system/bin/xdemo".
pub const XDEMO_ELF: &str = "XDEMO.ELF";

/// The single xui app image (`LAZYOS_XUI_APP`). Written by the image build. Target (F3): "/system/bin/xapp".
pub const XAPP_ELF: &str = "XAPP.ELF";

/// The drag and drop demo. Written by the image build. Target (F3): "/system/bin/dragdemo".
pub const DRAGDMO_ELF: &str = "DRAGDMO.ELF";

/// The shell-protocol evidence client. Written by the image build. Target (F3): "/system/bin/shellprobe".
pub const SHELLPRB_ELF: &str = "SHELLPRB.ELF";

/// The hello demo window. Written by the image build. Target (F3): "/system/bin/hello".
pub const HELLO_ELF: &str = "HELLO.ELF";

/// The Editor xui app. Written by the image build. Target (F3): "/system/bin/editor".
pub const XEDITOR_ELF: &str = "XEDITOR.ELF";

/// The Files xui app. Written by the image build. Target (F3): "/system/bin/files".
pub const XFILES_ELF: &str = "XFILES.ELF";

/// The Paint xui app. Written by the image build. Target (F3): "/system/bin/paint".
pub const XPAINT_ELF: &str = "XPAINT.ELF";

/// The Settings xui app. Written by the image build. Target (F3): "/system/bin/settings".
pub const XSETTNG_ELF: &str = "XSETTNG.ELF";

/// The Config xui app. Written by the image build. Target (F3): "/system/bin/confd-editor".
pub const XCONFD_ELF: &str = "XCONFD.ELF";

/// The Terminal xui app. Written by the image build. Target (F3): "/system/bin/terminal".
pub const XTERM_ELF: &str = "XTERM.ELF";

/// The System Monitor xui app. Written by the image build. Target (F3): "/system/bin/sysmon".
pub const XSYSMON_ELF: &str = "XSYSMON.ELF";

/// The Fabric Monitor xui app. Written by the image build. Target (F3): "/system/bin/fabricmon".
pub const XFABMON_ELF: &str = "XFABMON.ELF";

/// The Devices xui app (issue #481). Written by the image build. Target (F3): "/system/bin/devices".
pub const XDEVICES_ELF: &str = "XDEVICES.ELF";

/// The CPU and Memory widget xui app. Written by the image build. Target (F3): "/system/bin/widget".
pub const XWIDGET_ELF: &str = "XWIDGET.ELF";

/// The Counter xui app. Written by the image build. Target (F3): "/system/bin/counter".
pub const XCOUNTR_ELF: &str = "XCOUNTR.ELF";

/// `usbd`, the USB HID driver (`LAZYOS_USB=1` images). Written by the image build. Target (F3): "/system/bin/usbd".
pub const USBD_ELF: &str = "USBD.ELF";

/// The LazyRAD IDE xui app (`LAZYOS_LAZYRAD=1` images). Written by the image build. Target (F3): "/system/bin/lazyrad".
pub const LAZYRAD_ELF: &str = "LAZYRAD.ELF";

/// The Docs xui app. Written by the image build. Target (F3): "/system/bin/docs".
pub const XDOCS_ELF: &str = "XDOCS.ELF";

/// The Package Installer xui app. Written by the image build. Target (F3): "/system/bin/installer".
pub const XINSTALL_ELF: &str = "XINSTALL.ELF";

/// The image viewer (not shipped yet). Written by the image build. Target (F3): "/system/bin/viewer".
pub const VIEW_ELF: &str = "VIEW.ELF";

/// The program runner (not shipped yet). Written by the image build. Target (F3): "/system/bin/runner".
pub const RUNNER_ELF: &str = "RUNNER.ELF";

/// `top`, the text system monitor. Written by the image build. Target (F3): "/system/bin/top".
pub const TOP_ELF: &str = "TOP.ELF";

/// `messengerctl`, the fabric console. Written by the image build. Target (F3): "/system/bin/messengerctl".
pub const MSGCTL_ELF: &str = "MSGCTL.ELF";

/// `confctl`, the configuration command line. Written by the image build. Target (F3): "/system/bin/confctl".
pub const CONFCTL_ELF: &str = "CONFCTL.ELF";

/// The fault-injection probe. Written by the image build. Target (F3): "/system/bin/faultprobe".
pub const FAULTPRB_ELF: &str = "FAULTPRB.ELF";

/// `beep`, the audio client. Written by the image build. Target (F3): "/system/bin/beep".
pub const BEEP_ELF: &str = "BEEP.ELF";

/// `modplay`, the tracker-module player. Written by the image build. Target (F3): "/system/bin/modplay".
pub const MODPLAY_ELF: &str = "MODPLAY.ELF";

/// `pkgctl`, the package manager command line. Written by the image build. Target (F3): "/system/bin/pkgctl".
pub const PKGCTL_ELF: &str = "PKGCTL.ELF";

/// `nicctl`, the NIC control tool. Written by the image build. Target (F3): "/system/bin/nicctl".
pub const NICCTL_ELF: &str = "NICCTL.ELF";

/// `netctl`, the network stack tool. Written by the image build. Target (F3): "/system/bin/netctl".
pub const NETCTL_ELF: &str = "NETCTL.ELF";

/// `devctl`, the device inventory and class-rule viewer. Written by the image build. Target (F3): "/system/bin/devctl".
pub const DEVCTL_ELF: &str = "DEVCTL.ELF";

/// `timectl`, the time service client. Written by the image build. Target (F3): "/system/bin/timectl".
pub const TIMECTL_ELF: &str = "TIMECTL.ELF";

/// `powerctl`, the orderly shutdown/reboot command (docs/shutdown.md): the
/// shell's `shutdown`, `poweroff`, `halt` and `reboot` run it. Written by the
/// image build. Target (F3): "/system/bin/powerctl".
pub const POWERCTL_ELF: &str = "POWERCTL.ELF";

/// `ping`. Written by the image build. Target (F3): "/system/bin/ping".
pub const PING_ELF: &str = "PING.ELF";

/// `nc`. Written by the image build. Target (F3): "/system/bin/nc".
pub const NC_ELF: &str = "NC.ELF";

/// `nslookup`. Written by the image build. Target (F3): "/system/bin/nslookup".
pub const NSLOOKUP_ELF: &str = "NSLOOKUP.ELF";

/// `ftp`, the FTP client. Written by the image build. Target (F3): "/system/bin/ftp".
pub const FTP_ELF: &str = "FTP.ELF";

/// The clipboard copy demo. Written by the image build. Target (F3): "/system/bin/clipcp".
pub const CLIPCP_ELF: &str = "CLIPCP.ELF";

/// The clipboard paste demo. Written by the image build. Target (F3): "/system/bin/clippaste".
pub const CLIPPS_ELF: &str = "CLIPPS.ELF";

/// `rhai`, the scripting command. Written by the image build. Target (F3): "/system/bin/rhai".
pub const RHAI_ELF: &str = "RHAI.ELF";

/// The `lazyrad` player. Written by the image build. Target (F3): "/system/bin/lrplay".
pub const LRPLAY_ELF: &str = "LRPLAY.ELF";

/// BusyBox, the console shell and applets (the Linux ABI sees it as `/busybox`). Written by the image build. Target (F3): "/system/bin/busybox".
pub const BUSYBOX: &str = "BUSYBOX";

/// The account database. Written by the image build. Target (F3): "/system/etc/passwd".
pub const PASSWD: &str = "PASSWD";

/// The MIME type overrides. Written by the image build. Target (F3): "/system/share/mime.types".
pub const MIME_TYP: &str = "MIME.TYP";

/// The list of optional apps the build embedded. Written by the image build. Target (F5): removed in F5.
pub const XAPPS_LST: &str = "XAPPS.LST";

/// The path of the `lazyrad` player at the boot root. Target (F3):
/// `"/system/bin/lrplay"`.
pub const LRPLAY_PATH: &str = "/LRPLAY.ELF";

/// The path the Linux ABI gives BusyBox: the kernel resolves a lowercase root
/// name onto the uppercase file (`process/linux/path.rs`). Target (F3):
/// `"/system/bin/busybox"`.
pub const BUSYBOX_PATH: &str = "/busybox";
