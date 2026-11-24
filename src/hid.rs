//! The USB share button (the puck sold with KShare receivers, USB id 022b:3001).
//!
//! Its exact reports are undocumented: any report with a non-zero byte counts as a press,
//! an all-zero report as the release. Access needs the udev rule shipped in the packages.

use crate::net::{Event, Net};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const HID_ID: &str = "HID_ID=0003:0000022B:00003001";

/// Watches for the button on a background thread for the life of the app.
pub fn watch(net: Arc<Net>) {
    std::thread::spawn(move || {
        loop {
            if let Some(dev) = find() {
                if let Ok(mut f) = std::fs::File::open(&dev) {
                    net.send(Event::UsbButton { connected: true });
                    let mut buf = [0u8; 64];
                    let mut last = Instant::now() - Duration::from_secs(1);
                    while let Ok(n) = f.read(&mut buf) {
                        if n > 0 && buf[..n].iter().any(|&b| b != 0) && last.elapsed() > Duration::from_millis(500) {
                            last = Instant::now();
                            net.send(Event::UsbPressed);
                        }
                    }
                    net.send(Event::UsbButton { connected: false });
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

fn find() -> Option<PathBuf> {
    std::fs::read_dir("/sys/class/hidraw").ok()?.flatten().find_map(|e| {
        let uevent = std::fs::read_to_string(e.path().join("device/uevent")).ok()?;
        uevent
            .lines()
            .any(|l| l.eq_ignore_ascii_case(HID_ID))
            .then(|| PathBuf::from("/dev").join(e.file_name()))
    })
}
