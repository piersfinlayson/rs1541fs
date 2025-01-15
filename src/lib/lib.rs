pub mod cbm;
pub mod cbmtype;
pub mod ipc;
pub mod logging;
pub mod validate;

// Contains OpenCBM ffi wrappers - not be used outside this library
mod opencbm;

pub const MIN_DEVICE_NUM: u8 = 8;
pub const MAX_DEVICE_NUM: u8 = 15;
pub const DEFAULT_DEVICE_NUM: u8 = 8;

pub const XUM1541_VENDOR_ID: &str = "16d0";
pub const XUM1541_PRODUCT_ID: &str = "0504";
