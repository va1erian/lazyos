//! What the Wi-Fi page's buttons do: scan, connect (with the retry for a
//! network `wlanmd` was just told about), disconnect and forget.

use xui_app::net::wifi;
use xui_app::net::wifiview::{self, Plan};
use xui_app::platform::elevd;

use super::wifi_page::{Pending, JOIN_RETRIES};
use super::Network;

impl Network {
    pub(super) fn wifi_scan_now(&mut self) {
        self.wifi.pacer.restart();
        self.wifi_say("Searching for networks...");
        self.wifi_poll();
    }

    /// Connect or Enter on a row: what the network needs decides.
    pub(super) fn wifi_connect(&mut self) {
        let Some(network) = self.selected_network() else {
            self.wifi_say("Select a network first.");
            return;
        };
        match wifiview::plan(&network) {
            Plan::Connect { id } => self.wifi_join_saved(&network.ssid, &id),
            Plan::AddOpen { ssid } => {
                match wifi::add_network(&ssid, wifi::Security::Open, false, None) {
                    Ok(id) => self.wifi_join_saved(&ssid, &id),
                    Err(error) => {
                        self.wifi_say(&format!("Could not save it: {}", error.describe()))
                    }
                }
            }
            Plan::AskPassword { ssid } => self.open_join(wifiview::JoinKind::Known { ssid }),
            Plan::Hidden => self.wifi_say("Use \"Other network...\" to join a hidden network."),
            Plan::Unsupported => {
                self.wifi_say("That network's security mode is not supported (open and WPA2 only).")
            }
        }
    }

    /// Start joining the saved network `id`. `wlanmd` may not have re-read
    /// its profiles when a network was saved a moment ago (`ENOENT`): the
    /// join is then asked again each tick for a while.
    pub(super) fn wifi_join_saved(&mut self, ssid: &str, id: &str) {
        match self.wifi_try_connect(ssid, id) {
            Ok(()) => {}
            Err(error) if error.is_missing() => {
                self.wifi.pending = Some(Pending {
                    id: id.to_string(),
                    ssid: ssid.to_string(),
                    left: JOIN_RETRIES,
                });
                self.wifi_say(&format!(
                    "Saved. Connecting to {ssid} as soon as the Wi-Fi service has it..."
                ));
            }
            Err(error) => self.wifi_connect_failed(ssid, error),
        }
    }

    fn wifi_try_connect(&mut self, ssid: &str, id: &str) -> Result<(), wifi::Error> {
        let Some(ifname) = self.wifi.ifname.clone() else {
            return Ok(());
        };
        wifi::connect(&ifname, id)?;
        println!("NETAPP:WIFI:CONNECT ssid={ssid}");
        self.wifi_say(&format!("Connecting to {ssid}..."));
        self.wifi.pending = None;
        self.wifi.pacer.restart();
        Ok(())
    }

    fn wifi_connect_failed(&mut self, ssid: &str, error: wifi::Error) {
        println!("NETAPP:WIFI:CONNECT:FAIL {} ssid={ssid}", error.describe());
        self.wifi.pending = None;
        self.wifi_say(&format!("Could not connect: {}", error.describe()));
    }

    /// Ask again for a join `wlanmd` did not know yet.
    pub(super) fn wifi_retry_join(&mut self) {
        let Some(mut pending) = self.wifi.pending.take() else {
            return;
        };
        pending.left = pending.left.saturating_sub(1);
        match self.wifi_try_connect(&pending.ssid, &pending.id) {
            Ok(()) => {}
            Err(error) if error.is_missing() && pending.left > 0 => {
                self.wifi.pending = Some(pending)
            }
            Err(error) => self.wifi_connect_failed(&pending.ssid, error),
        }
    }

    pub(super) fn wifi_disconnect(&mut self) {
        let Some(ifname) = self.wifi.ifname.clone() else {
            return;
        };
        match wifi::disconnect(&ifname) {
            Ok(()) => self.wifi_say("Disconnected. It stays off until you connect again."),
            Err(error) => self.wifi_say(&format!("Could not disconnect: {}", error.describe())),
        }
    }

    /// Forget asks first: the saved password goes with it.
    pub(super) fn wifi_forget_ask(&mut self) {
        let Some(network) = self.selected_network().filter(|n| !n.id.is_empty()) else {
            self.wifi_say("Select a saved network to forget.");
            return;
        };
        let dialog = &self.wifi.forget_dialog;
        dialog.set_message(&format!(
            "Forget \"{}\"? Its saved password is deleted. A network kept for all users needs an administrator to approve.",
            network.ssid
        ));
        self.wifi.forget_target = Some((network.id, network.ssid));
        self.wifi.forget_dialog.open();
    }

    pub(super) fn wifi_forget_answer(&mut self, yes: bool) {
        let Some((id, ssid)) = self.wifi.forget_target.take() else {
            return;
        };
        if !yes {
            return;
        }
        let result = match wifi::forget(&id) {
            // A network of all users is the administrator's to remove.
            Err(error) if error.is_denied() => elevd::request("net.wifi.forget", &[id.as_str()])
                .map(drop)
                .map_err(|error| elevd::describe(&error)),
            other => other.map_err(|error| error.describe()),
        };
        match result {
            Ok(()) => {
                println!("NETAPP:WIFI:FORGET ssid={ssid}");
                self.wifi_say(&format!("Forgot {ssid}."));
                self.wifi_poll();
            }
            Err(why) => self.wifi_say(&format!("Could not forget {ssid}: {why}")),
        }
    }
}
