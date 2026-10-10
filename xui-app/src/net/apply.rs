//! What the Network app asks `elevd` for when Apply is pressed.

use super::model::{plan, Form};

/// The arguments of `elevd`'s `net.config` for `form` on `ifname` (card,
/// address, gateway, DNS; an empty address is DHCP), after the same checks as
/// [`super::model::plan`]. `elevd` writes `sys/net/<card>/*` in `plan`'s order, because a
/// session may not write `sys/**` itself.
pub fn elevd_args(ifname: &str, form: &Form) -> Result<[String; 4], String> {
    plan(ifname, form)?;
    let trimmed = |text: &str| text.trim().to_string();
    Ok(if form.manual {
        [
            ifname.to_string(),
            trimmed(&form.address),
            trimmed(&form.gateway),
            trimmed(&form.dns),
        ]
    } else {
        [
            ifname.to_string(),
            String::new(),
            String::new(),
            String::new(),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IF: &str = "eth1";

    fn manual(address: &str, gateway: &str, dns: &str) -> Form {
        Form {
            manual: true,
            address: address.into(),
            gateway: gateway.into(),
            dns: dns.into(),
        }
    }

    #[test]
    fn elevd_gets_the_checked_form() {
        let args = elevd_args(IF, &manual(" 10.0.3.9/24 ", "10.0.3.2", "")).unwrap();
        assert_eq!(args, [IF, "10.0.3.9/24", "10.0.3.2", ""]);
        let dhcp = Form {
            manual: false,
            address: "kept".into(),
            ..Form::default()
        };
        assert_eq!(elevd_args(IF, &dhcp).unwrap(), [IF, "", "", ""]);
        assert!(elevd_args(IF, &manual("nonsense", "", "")).is_err());
    }
}
