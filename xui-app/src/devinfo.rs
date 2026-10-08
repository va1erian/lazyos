//! The device layer as the Devices app shows it (issue #481): the inventory
//! with each owner and its rights, the driver class rules the kernel installed
//! at boot, and the refused claims. Read through the device syscall's
//! read-only ops; the row layouts and names are `libs/devinspect`'s, shared
//! with `devctl`.

use devinspect::{Denial, Device, Rule};

/// What one refresh read. Each part fails on its own: a user without
/// `CAP_AUDIT_READ` still sees the devices and the rules.
pub struct DevView {
    pub devices: Result<Vec<Device>, i64>,
    /// `Ok(None)`: the kernel has not installed a class policy.
    pub rules: Result<Option<Vec<Rule>>, i64>,
    pub denials: Result<Vec<Denial>, i64>,
}

impl DevView {
    pub fn read() -> DevView {
        // The view reports positive errnos.
        let devices = lazyos_sys::dev::inventory().map_err(|code| -code);
        let rules = lazyos_sys::dev::policy().map_err(|code| -code);
        let denials = lazyos_sys::dev::denials().map_err(|code| -code);
        DevView {
            devices,
            rules,
            denials,
        }
    }
}

/// One driver's grants, the rules grouped by `(uid, class)` in load order:
/// `_net  net  claim map dma`.
pub struct Grant {
    pub uid: u32,
    pub class_id: u64,
    pub methods: Vec<&'static str>,
    pub allow: bool,
}

/// Group `rules` for display, keeping the first-match order.
pub fn grants(rules: &[Rule]) -> Vec<Grant> {
    let mut out: Vec<Grant> = Vec::new();
    for rule in rules {
        let method = devinspect::method_name(rule.method);
        match out.iter_mut().find(|grant| {
            grant.uid == rule.actor
                && grant.class_id == rule.interface_id
                && grant.allow == rule.allow
        }) {
            Some(grant) => grant.methods.push(method),
            None => out.push(Grant {
                uid: rule.actor,
                class_id: rule.interface_id,
                methods: vec![method],
                allow: rule.allow,
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use devinspect::{fnv1a32, fnv1a64};

    fn rule(actor: u32, class: &str, method: &str) -> Rule {
        Rule {
            actor,
            interface_id: fnv1a64(class),
            method: fnv1a32(method),
            allow: true,
        }
    }

    #[test]
    fn grants_group_methods_per_driver_and_class() {
        let rules = [
            rule(902, "os.kernel.dev.net", "claim"),
            rule(902, "os.kernel.dev.net", "map"),
            rule(904, "os.kernel.dev.usb", "claim"),
            rule(902, "os.kernel.dev.net", "dma"),
        ];
        let grants = grants(&rules);
        assert_eq!(grants.len(), 2);
        assert_eq!(grants[0].methods, ["claim", "map", "dma"]);
        assert_eq!(devinspect::class_name(grants[1].class_id), "usb");
    }
}
