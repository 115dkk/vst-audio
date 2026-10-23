#![deny(unsafe_code)]
//! Test-only application windows. Native resources remain on the creating thread.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{Parent, ResourceManager};
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
#[cfg(windows)]
pub use windows::Parent;
#[cfg(windows)]
#[allow(unsafe_code)]
pub mod desktop;

#[cfg(any(windows, target_os = "linux"))]
impl Parent {
    /// Injects a physical-input-path left button press in parent client coordinates.
    pub fn press(&self, x: i32, y: i32) {
        self.pointer(x, y, Some(true));
    }
    pub fn release(&self, x: i32, y: i32) {
        self.pointer(x, y, Some(false));
    }
    pub fn click(&self, x: i32, y: i32) {
        self.press(x, y);
        self.release(x, y);
    }
    pub fn drag(&self, from: (i32, i32), to: (i32, i32), steps: u32) {
        assert!(steps > 0);
        self.press(from.0, from.1);
        for step in 1..=steps {
            let coordinate = |a: i32, b: i32| {
                (i64::from(a) + i64::from(b - a) * i64::from(step) / i64::from(steps)) as i32
            };
            self.pointer(coordinate(from.0, to.0), coordinate(from.1, to.1), None);
        }
        self.release(to.0, to.1);
    }
    pub fn settle(&self) {
        let end = std::time::Instant::now() + std::time::Duration::from_millis(40);
        while std::time::Instant::now() < end {
            self.pump();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}
