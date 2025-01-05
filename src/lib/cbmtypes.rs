use serde::{Deserialize, Serialize};

pub struct CbmDeviceInfo {
    pub device_type: CbmDeviceType,
    pub description: String,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(into = "i32", from = "i32")]

pub enum CbmDeviceType {
    Unknown = -1,
    Cbm1541 = 0,
    Cbm1570 = 1,
    Cbm1571 = 2,
    Cbm1581 = 3,
    Cbm2040 = 4,
    Cbm2031 = 5,
    Cbm3040 = 6,
    Cbm4040 = 7,
    Cbm4031 = 8,
    Cbm8050 = 9,
    Cbm8250 = 10,
    Sfd1001 = 11,
    FdX000 = 12,
}

#[derive(Debug)]
pub struct CbmDeviceTypeError(String);

impl std::fmt::Display for CbmDeviceTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid device type value: {}", self.0)
    }
}

impl std::error::Error for CbmDeviceTypeError {}

impl From<i32> for CbmDeviceType {
    fn from(value: i32) -> Self {
        match value {
            -1 => Self::Unknown,
            0 => Self::Cbm1541,
            1 => Self::Cbm1570,
            2 => Self::Cbm1571,
            3 => Self::Cbm1581,
            4 => Self::Cbm2040,
            5 => Self::Cbm2031,
            6 => Self::Cbm3040,
            7 => Self::Cbm4040,
            8 => Self::Cbm4031,
            9 => Self::Cbm8050,
            10 => Self::Cbm8250,
            11 => Self::Sfd1001,
            12 => Self::FdX000,
            _ => Self::Unknown,
        }
    }
}

impl From<CbmDeviceType> for i32 {
    fn from(value: CbmDeviceType) -> Self {
        value as i32
    }
}

impl CbmDeviceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "Unknown Device",
            Self::Cbm1541 => "1541",
            Self::Cbm1570 => "1570",
            Self::Cbm1571 => "1571",
            Self::Cbm1581 => "1581",
            Self::Cbm2040 => "2040",
            Self::Cbm2031 => "2031",
            Self::Cbm3040 => "3040",
            Self::Cbm4040 => "4040",
            Self::Cbm4031 => "4031",
            Self::Cbm8050 => "8050",
            Self::Cbm8250 => "8250",
            Self::Sfd1001 => "SFD-1001",
            Self::FdX000 => "FDX000",
        }
    }
}

