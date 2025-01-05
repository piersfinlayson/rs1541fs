pub mod cbm;
pub mod ipc;
pub mod logging;
pub mod validate;
pub mod cbmtypes;

// Contains ffi wrappers - not be used outside this library
mod opencbm;

pub const MIN_DEVICE_NUM: u8 = 8;
pub const MAX_DEVICE_NUM: u8 = 15;
pub const DEFAULT_DEVICE_NUM: u8 = 8;
