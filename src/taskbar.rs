pub struct Taskbar {
    active: bool,
    enabled: bool,
}

impl Taskbar {
    pub fn new() -> Self {
        Taskbar {
            active: false,
            enabled: true,
        }
    }

    pub fn set_active(&mut self, active: bool) {
        self.active = active;
        self.update();
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.update();
    }

    fn update(&self) {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                use std::ffi::c_void;
                type Hwnd = *mut c_void;
                unsafe extern "system" {
                    fn GetConsoleWindow() -> Hwnd;
                }
                let hwnd = GetConsoleWindow();
                if self.active {
                    // TBPF_NOPROGRESS or TBPF_PAUSED
                } else {
                    // TBPF_ERROR
                }
                let _ = hwnd;
            }
        }
    }
}
