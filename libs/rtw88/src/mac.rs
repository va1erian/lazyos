//! The MAC (upstream `mac.c`): the power sequence engine
//! (`rtw_pwr_seq_parser`), `rtw_mac_power_on`/`off`, `rtw_mac_init` (TRX
//! configuration, queue pages, DMA mapping), `rtw_mac_postinit`, and the
//! firmware download primitives `mac.c` holds (`download_firmware_*`,
//! `rtw_ddma_*`).
//!
//! Owner: the core/MAC work package.
