//! `--timing`: measure and log times since process creation (not since `main`).

use std::io::Write;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

pub struct Timing {
    pub enabled: bool,
    /// Exit after the first frame with content.
    pub bench: bool,
    created: u64,
    log: Vec<String>,
}

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

impl Timing {
    pub fn new(enabled: bool, bench: bool) -> Timing {
        let mut created = 0;
        if enabled {
            let (mut c, mut e, mut k, mut u) = Default::default();
            unsafe {
                if GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u).is_ok() {
                    created = ft(c);
                }
            }
        }
        Timing { enabled, bench, created, log: Vec::new() }
    }

    pub fn since_start_ms(&self) -> f64 {
        let now = ft(unsafe { GetSystemTimePreciseAsFileTime() });
        now.saturating_sub(self.created) as f64 / 10_000.0
    }

    pub fn mark(&mut self, what: &str) {
        if self.enabled {
            let line = format!("[{:8.2} ms] {what}", self.since_start_ms());
            eprintln!("{line}");
            self.log.push(line);
        }
    }

    pub fn note(&mut self, what: String) {
        if self.enabled {
            eprintln!("{what}");
            self.log.push(what);
        }
    }

    pub fn flush(&mut self) {
        if !self.enabled || self.log.is_empty() {
            return;
        }
        if let Some(dir) = crate::config::dir() {
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("timing.log"))
            {
                let _ = writeln!(f, "--- run ---");
                for l in self.log.drain(..) {
                    let _ = writeln!(f, "{l}");
                }
            }
        }
    }
}
