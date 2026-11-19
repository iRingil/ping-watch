//! COM for the Windows services reached through it (Windows Firewall, Task Scheduler).

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// COM initialized for this thread as long as the value lives.
#[derive(Debug)]
pub struct Com {
    /// Whether this value initialized COM, and so uninitializes it.
    initialized: bool,
}

impl Com {
    /// Initializes COM for this thread, unless the thread has it already in another mode (the window's thread):
    /// then that one is used.
    pub fn init() -> Self {
        // SAFETY: plain initialization of COM for the calling thread, paired with `CoUninitialize` on drop.
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self { initialized: result.is_ok() }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: pairs with the successful `CoInitializeEx` of `init` on this thread.
            unsafe { CoUninitialize() };
        }
    }
}
