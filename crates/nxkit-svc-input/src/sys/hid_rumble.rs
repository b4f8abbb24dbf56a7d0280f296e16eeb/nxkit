//! HD Rumble on a Switch Pro Controller through IOKit: the rumble reports of `hd` go to the controller as HID output
//! reports, written by one thread per controller at the pace of `hd::pacer`.
//!
//! GameController keeps the controller for its input. The device is opened here without seizing it
//! (`kIOHIDOptionsTypeNone`), so GameController still reads it and this module only writes. When the open or a write
//! fails, the controller goes back to Core Haptics. Whether GameController's own driver lets another client write
//! output reports has not been tried on a controller (decisions R74).
//!
//! The writer is a thread because `IOHIDDeviceSetReport` blocks until the report is out, over Bluetooth for some
//! milliseconds, and the reports need their own pace. The game's call only stores the newest values. The thread sleeps
//! while the controller is at rest.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFDictionary, CFNumber, CFString, CFType};

use crate::hd::encoding::{NEUTRAL, enable_report, rumble_report};
use crate::hd::pacer::{Link, Pacer, Poll};
use crate::rumble::VibrationValue;

/// Nintendo.
const VENDOR: i32 = 0x057e;
/// The Switch Pro Controller.
const PRO_CONTROLLER: i32 = 0x2009;
const IO_RETURN_SUCCESS: i32 = 0;
const OPTIONS_NONE: u32 = 0;
const REPORT_TYPE_OUTPUT: u32 = 1;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> *mut c_void;
    fn IOHIDManagerSetDeviceMatching(manager: *mut c_void, matching: *const c_void);
    fn IOHIDManagerCopyDevices(manager: *mut c_void) -> *const c_void;
    fn IOHIDDeviceGetProperty(device: *mut c_void, key: *const c_void) -> *const c_void;
    fn IOHIDDeviceOpen(device: *mut c_void, options: u32) -> i32;
    fn IOHIDDeviceClose(device: *mut c_void, options: u32) -> i32;
    fn IOHIDDeviceSetReport(
        device: *mut c_void,
        report_type: u32,
        report_id: isize,
        report: *const u8,
        length: isize,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    fn CFSetGetCount(set: *const c_void) -> isize;
    fn CFSetGetValues(set: *const c_void, values: *mut *const c_void);
}

/// An open Pro Controller and the manager that found it.
struct Device {
    manager: *mut c_void,
    device: *mut c_void,
    /// The size of an output report, which every write is padded to.
    report_len: usize,
}

// SAFETY: the device is used by one thread at a time: the thread that opens it, then the writer it is moved to.
// IOKit's HID device functions may be called from any thread.
unsafe impl Send for Device {}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the device was opened by `find` and both objects were retained there.
        unsafe {
            IOHIDDeviceClose(self.device, OPTIONS_NONE);
            CFRelease(self.device);
            CFRelease(self.manager);
        }
    }
}

/// A property of a device. The object belongs to the device.
fn property(device: *mut c_void, key: &str) -> Option<&CFType> {
    let key = CFString::from_str(key);
    // SAFETY: the device is alive and the key a string. The result is a borrowed CF object or null.
    let p = unsafe { IOHIDDeviceGetProperty(device, (&*key as *const CFString).cast()) };
    // SAFETY: a non-null result is a CF object that lives while the device does.
    (!p.is_null()).then(|| unsafe { &*(p as *const CFType) })
}

impl Device {
    /// The one Switch Pro Controller of the Mac, opened without seizing it.
    fn find() -> Result<Device, String> {
        let keys = [
            CFString::from_str("VendorID"),
            CFString::from_str("ProductID"),
        ];
        let values = [CFNumber::new_i32(VENDOR), CFNumber::new_i32(PRO_CONTROLLER)];
        let matching = CFDictionary::<CFString, CFNumber>::from_slices(
            &[&*keys[0], &*keys[1]],
            &[&*values[0], &*values[1]],
        );
        // SAFETY: a new manager with the default allocator, matching on a dictionary of CF strings and numbers. The
        // set of devices is released after its devices are retained.
        unsafe {
            let manager = IOHIDManagerCreate(std::ptr::null(), OPTIONS_NONE);
            if manager.is_null() {
                return Err("no HID manager".to_owned());
            }
            IOHIDManagerSetDeviceMatching(
                manager,
                (&*matching as *const CFDictionary<_, _>).cast(),
            );
            let set = IOHIDManagerCopyDevices(manager);
            let count = if set.is_null() { 0 } else { CFSetGetCount(set) };
            if count != 1 {
                if !set.is_null() {
                    CFRelease(set);
                }
                CFRelease(manager);
                return Err(format!("{count} Pro Controllers on IOKit, one is needed"));
            }
            let mut device: *const c_void = std::ptr::null();
            CFSetGetValues(set, &mut device);
            CFRetain(device);
            CFRelease(set);
            let device = device as *mut c_void;
            let opened = IOHIDDeviceOpen(device, OPTIONS_NONE);
            if opened != IO_RETURN_SUCCESS {
                CFRelease(device);
                CFRelease(manager);
                return Err(format!("IOHIDDeviceOpen returned {opened:#x}"));
            }
            let report_len = property(device, "MaxOutputReportSize")
                .and_then(|p| p.downcast_ref::<CFNumber>())
                .and_then(|n| n.as_i32())
                .map_or(0, |n| n.max(0) as usize);
            Ok(Device {
                manager,
                device,
                report_len,
            })
        }
    }

    /// How the controller is connected.
    fn link(&self) -> Link {
        let transport = property(self.device, "Transport")
            .and_then(|p| p.downcast_ref::<CFString>())
            .map(|s| s.to_string())
            .unwrap_or_default();
        if transport.contains("Bluetooth") {
            Link::Bluetooth
        } else {
            Link::Usb
        }
    }

    /// Writes an output report. The first byte is the report id.
    fn write(&self, report: &[u8]) -> bool {
        let mut buf = report.to_vec();
        buf.resize(report.len().max(self.report_len), 0);
        // SAFETY: the device is open and the buffer lives for the call.
        let r = unsafe {
            IOHIDDeviceSetReport(
                self.device,
                REPORT_TYPE_OUTPUT,
                isize::from(report[0]),
                buf.as_ptr(),
                buf.len() as isize,
            )
        };
        r == IO_RETURN_SUCCESS
    }
}

#[derive(Default)]
struct State {
    sides: [VibrationValue; 2],
    fresh: bool,
    stop: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    failed: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The HD Rumble of one Pro Controller.
pub(crate) struct HidRumble {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    link: Link,
}

impl HidRumble {
    /// Opens the Pro Controller, switches its actuators on and starts the writer. An error names the reason.
    pub(crate) fn open() -> Result<HidRumble, String> {
        let device = Device::find()?;
        let link = device.link();
        let mut pacer = Pacer::new(link);
        if !device.write(&enable_report(pacer.next_counter())) {
            return Err(
                "the controller refused the report that switches on its actuators".to_owned(),
            );
        }
        let shared = Arc::new(Shared::default());
        let writer = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("nxkit-hd-rumble".to_owned())
            .spawn(move || run(&device, &writer, pacer))
            .map_err(|e| format!("no writer thread: {e}"))?;
        Ok(HidRumble {
            shared,
            thread: Some(thread),
            link,
        })
    }

    /// How the controller is connected.
    pub(crate) fn link(&self) -> Link {
        self.link
    }

    /// True when a write failed. The writer has stopped then.
    pub(crate) fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Relaxed)
    }

    /// Takes the values of the left and the right side. An unchanged value does nothing.
    pub(crate) fn send(&self, sides: [VibrationValue; 2]) {
        let mut s = self.shared.lock();
        if s.sides != sides {
            s.sides = sides;
            s.fresh = true;
            self.shared.wake.notify_one();
        }
    }
}

impl Drop for HidRumble {
    fn drop(&mut self) {
        self.shared.lock().stop = true;
        self.shared.wake.notify_one();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The writer: waits for a new value or the time of the next report, and writes what the pacer decides. At the stop
/// the controller gets a report at rest.
fn run(device: &Device, shared: &Shared, mut pacer: Pacer) {
    let epoch = Instant::now();
    let now = || epoch.elapsed().as_micros() as u64;
    let mut due: Option<u64> = None;
    loop {
        let (sides, stop) = {
            let mut s = shared.lock();
            while !s.stop && !s.fresh {
                let wait = match due {
                    None => None,
                    Some(d) => match d.checked_sub(now()) {
                        Some(left) if left > 0 => Some(Duration::from_micros(left)),
                        _ => break,
                    },
                };
                s = match wait {
                    None => shared.wake.wait(s).unwrap_or_else(PoisonError::into_inner),
                    Some(w) => {
                        let (s, _) = shared
                            .wake
                            .wait_timeout(s, w)
                            .unwrap_or_else(PoisonError::into_inner);
                        s
                    }
                };
            }
            let fresh = std::mem::take(&mut s.fresh);
            (fresh.then_some(s.sides), s.stop)
        };
        if stop {
            let _ = device.write(&rumble_report(pacer.next_counter(), &[NEUTRAL; 2]));
            return;
        }
        if let Some(sides) = sides {
            pacer.push(sides);
        }
        due = match pacer.poll(now()) {
            Poll::Send(report) => {
                if !device.write(&report) {
                    shared.failed.store(true, Ordering::Relaxed);
                    return;
                }
                Some(0)
            }
            Poll::Wait(next) => next,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays a short pattern on a Switch Pro Controller through IOKit alone, without GameController. Needs one
    /// connected Pro Controller: `cargo test -p nxkit-svc-input --lib pro_controller -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a connected Switch Pro Controller"]
    fn pro_controller_plays_a_pattern() {
        let hd = match HidRumble::open() {
            Ok(hd) => hd,
            Err(e) => panic!("no HD Rumble: {e}"),
        };
        println!("HD Rumble over {:?}", hd.link());
        let band = |lo: f32, fl: f32, hi: f32, fh: f32| VibrationValue {
            amp_low: lo,
            freq_low: fl,
            amp_high: hi,
            freq_high: fh,
        };
        let idle = VibrationValue::IDLE;
        let steps: [(&str, [VibrationValue; 2], u64); 5] = [
            ("low 80 Hz left", [band(0.8, 80.0, 0.0, 320.0), idle], 400),
            (
                "high 640 Hz right",
                [idle, band(0.0, 160.0, 0.6, 640.0)],
                400,
            ),
            (
                "both bands both sides",
                [band(0.4, 160.0, 0.4, 320.0); 2],
                400,
            ),
            ("rest", [idle; 2], 300),
            ("high 1250 Hz both", [band(0.0, 160.0, 0.5, 1250.0); 2], 400),
        ];
        for (name, sides, ms) in steps {
            println!("{name}");
            hd.send(sides);
            std::thread::sleep(Duration::from_millis(ms));
        }
        hd.send([idle; 2]);
        std::thread::sleep(Duration::from_millis(100));
        assert!(!hd.failed(), "a write failed");
    }
}
