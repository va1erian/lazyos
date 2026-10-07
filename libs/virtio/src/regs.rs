//! Register offsets, status bits and feature bits from the virtio 1.x spec
//! (section 4.1.4, "Virtio Over PCI Bus").

/// `device_status` bits.
pub mod status {
    pub const ACKNOWLEDGE: u8 = 1;
    pub const DRIVER: u8 = 2;
    pub const DRIVER_OK: u8 = 4;
    pub const FEATURES_OK: u8 = 8;
    pub const NEEDS_RESET: u8 = 64;
    pub const FAILED: u8 = 128;
}

/// Byte offsets inside the common configuration structure.
pub mod common {
    pub const DEVICE_FEATURE_SELECT: usize = 0x00;
    pub const DEVICE_FEATURE: usize = 0x04;
    pub const DRIVER_FEATURE_SELECT: usize = 0x08;
    pub const DRIVER_FEATURE: usize = 0x0C;
    pub const MSIX_CONFIG: usize = 0x10;
    pub const NUM_QUEUES: usize = 0x12;
    pub const DEVICE_STATUS: usize = 0x14;
    pub const CONFIG_GENERATION: usize = 0x15;
    pub const QUEUE_SELECT: usize = 0x16;
    pub const QUEUE_SIZE: usize = 0x18;
    pub const QUEUE_MSIX_VECTOR: usize = 0x1A;
    pub const QUEUE_ENABLE: usize = 0x1C;
    pub const QUEUE_NOTIFY_OFF: usize = 0x1E;
    pub const QUEUE_DESC: usize = 0x20;
    pub const QUEUE_DRIVER: usize = 0x28;
    pub const QUEUE_DEVICE: usize = 0x30;
    /// Size of the structure the spec defines; a capability must cover it.
    pub const LEN: usize = 0x38;
    /// `msix_vector` value meaning "no vector".
    pub const NO_VECTOR: u16 = 0xFFFF;
}

/// Feature bit 32: the device follows the virtio 1.x (non-legacy) interface.
pub const F_VERSION_1: u64 = 1 << 32;
/// Feature bit 33: the device can use platform (IOMMU) addresses.
pub const F_ACCESS_PLATFORM: u64 = 1 << 33;

/// PCI vendor id of every virtio device.
pub const PCI_VENDOR: u16 = 0x1AF4;
/// Modern virtio device ids are `0x1040 + <virtio device type>`.
pub const PCI_DEVICE_BASE: u16 = 0x1040;

/// The PCI device id of a modern virtio device of `device_type`.
pub const fn pci_device_id(device_type: u16) -> u16 {
    PCI_DEVICE_BASE + device_type
}
