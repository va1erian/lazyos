//! The FNV-1a hashes the kernel keys its policy by.
//!
//! An interface id is the 64-bit hash of the `.vN` interface name; a method id
//! (and, for a name or topic segment, the "method" a policy rule keys on) is the
//! 32-bit hash masked to 31 bits (`tools/midlc`). The tests pin both against the
//! generated interface ids, so a drift between this file and `midlc` fails here
//! rather than silently granting the wrong thing.

/// FNV-1a 64 of `text`: an interface id.
pub const fn fnv1a64(text: &str) -> u64 {
    let bytes = text.as_bytes();
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    let mut index = 0;
    while index < bytes.len() {
        hash = (hash ^ bytes[index] as u64).wrapping_mul(0x0000_0100_0000_01B3);
        index += 1;
    }
    hash
}

/// FNV-1a 32 of `text`, masked to 31 bits: a method id, or the method a rule
/// uses for one name or topic segment.
pub const fn fnv1a32(text: &str) -> u32 {
    let bytes = text.as_bytes();
    let mut hash = 0x811C_9DC5u32;
    let mut index = 0;
    while index < bytes.len() {
        hash = (hash ^ bytes[index] as u32).wrapping_mul(0x0100_0193);
        index += 1;
    }
    hash & 0x7FFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;
    use messenger_generated::{
        os_lazy_messenger_names_resolve_v1 as names, os_lazy_messenger_policy_v1 as policy,
        os_lazy_messenger_topics_publish_v1 as publish,
        os_lazy_messenger_topics_subscribe_v1 as subscribe, os_lazy_messenger_topics_v1 as topics,
        os_lazy_pkgd_v1 as pkgd,
    };

    #[test]
    fn interface_ids_match_the_generated_stubs() {
        assert_eq!(fnv1a64("os.lazy.pkgd.v1"), pkgd::INTERFACE_ID);
        assert_eq!(fnv1a64("os.lazy.messenger.policy.v1"), policy::INTERFACE_ID);
        assert_eq!(
            fnv1a64("os.lazy.messenger.names.resolve.v1"),
            names::INTERFACE_ID
        );
        assert_eq!(
            fnv1a64("os.lazy.messenger.topics.publish.v1"),
            publish::INTERFACE_ID
        );
        assert_eq!(
            fnv1a64("os.lazy.messenger.topics.subscribe.v1"),
            subscribe::INTERFACE_ID
        );
        assert_eq!(fnv1a64("os.lazy.messenger.topics.v1"), topics::INTERFACE_ID);
    }

    #[test]
    fn method_ids_match_the_generated_stubs() {
        assert_eq!(fnv1a32("Publish"), topics::METHOD_PUBLISH);
        assert_eq!(fnv1a32("Subscribe"), topics::METHOD_SUBSCRIBE);
        assert_eq!(fnv1a32("NextEvent"), topics::METHOD_NEXTEVENT);
        assert_eq!(fnv1a32("Install"), pkgd::METHOD_INSTALL);
    }
}
