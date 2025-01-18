#![allow(dead_code)]

/// FUSE file handle encoding structure
///
/// Encodes device number, drive ID, channel number, and a sequence number into
/// a single u64 that can be passed back and forth with the FUSE kernel module.
///
/// Layout:
/// - Bits 56-63: Device number (8-15)
/// - Bits 48-55: Drive ID (0-1)
/// - Bits 40-47: Channel number (0-15)
/// - Bits 0-39:  Sequence number
#[derive(Debug, Clone, Copy)]
struct FileHandle {
    device_number: u8,
    drive_id: u8,
    channel_number: u8,
    sequence: u64,
}

impl FileHandle {
    fn new(device_number: u8, drive_id: u8, channel_number: u8, sequence: u64) -> Self {
        Self {
            device_number,
            drive_id,
            channel_number,
            sequence,
        }
    }

    fn to_u64(&self) -> u64 {
        (self.device_number as u64) << 56
            | (self.drive_id as u64) << 48
            | (self.channel_number as u64) << 40
            | (self.sequence & 0xFF_FFFF_FFFF) // 40 bits for sequence
    }

    fn from_u64(handle: u64) -> Self {
        Self {
            device_number: ((handle >> 56) & 0xFF) as u8,
            drive_id: ((handle >> 48) & 0xFF) as u8,
            channel_number: ((handle >> 40) & 0xFF) as u8,
            sequence: handle & 0xFF_FFFF_FFFF,
        }
    }
}

