//! The device layer as the Devices app shows it (issue #481): the inventory
//! with each owner and its rights, the driver class rules the kernel installed
//! at boot, and the refused claims. Read through the device syscall's
//! read-only ops; the row layouts and names are `libs/devinspect`'s, shared
//! with `devctl`.

use devinspect::{Denial, Device, Rule, DENIAL_WORDS, INVENTORY_WORDS, RULE_WORDS};

use crate::sys;

/// One op's rows, the buffer grown until it holds them all.
fn read_all(op: u64, words_per_row: usize) -> Result<(Vec<u64>, usize), i64> {
    let mut capacity = 16;
    loop {
        let mut words = vec![0u64; capacity * words_per_row];
        let code = sys::dev_inspect(op, words.as_mut_ptr() as u64, capacity as u64);
        if code < 0 {
            return Err(-code);
        }
        let total = code as usize;
        if total <= capacity {
            return Ok((words, total));
        }
        capacity = total;
    }
}

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
        let devices = read_all(devinspect::op::INVENTORY, INVENTORY_WORDS).map(|(words, count)| {
            devinspect::rows::<INVENTORY_WORDS>(&words, count)
                .map(|row| Device::from_words(&row))
                .collect()
        });
        let rules = match read_all(devinspect::op::POLICY, RULE_WORDS) {
            Ok((words, count)) => Ok(Some(
                devinspect::rows::<RULE_WORDS>(&words, count)
                    .map(|row| Rule::from_words(&row))
                    .collect(),
            )),
            Err(devinspect::errno::ENOENT) => Ok(None),
            Err(errno) => Err(errno),
        };
        let denials = read_all(devinspect::op::DENIALS, DENIAL_WORDS).map(|(words, count)| {
            devinspect::rows::<DENIAL_WORDS>(&words, count)
                .map(|row| Denial::from_words(&row))
                .collect()
        });
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
