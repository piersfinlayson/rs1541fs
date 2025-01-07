use rs1541fs::cbm::Cbm;

use crate::mount::Mountpoint;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

// parking_lot is used to simplify handling - there is no need to handle
// failing to get the lock
use parking_lot::{Mutex, RwLock};

/// Main 1541fsd data structure, storing:
/// * libopencbm handle
/// * mountpoints
///
/// An instance of this struct must be wrapped in Arc and Mutex in order to
/// safely pass between threads.
///
/// Example creation:
///   let cbm = Cbm::new()?; // Simplified - see main.rs
///   let daemon = Arc::new(Mutex::new(Daemon(cbm)));
#[derive(Debug)]
pub struct Daemon {
    pub cbm: Arc<Mutex<Cbm>>,
    pub mountpoints: Arc<RwLock<HashMap<PathBuf, Mountpoint>>>,
}

impl Daemon {
    pub fn new(cbm: Cbm) -> Result<Self, String> {
        Ok(Self {
            cbm: Arc::new(Mutex::new(cbm)),
            mountpoints: Arc::new(RwLock::new(HashMap::new())),
        })
    }
}
