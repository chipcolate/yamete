//! IOKit HID access to the Apple Silicon SPU inertial sensors.
//!
//! The IMU on Apple Silicon laptops is undocumented and unreachable through CoreMotion.
//! It surfaces as a set of `AppleSPUHIDDevice` IOServices on Apple's vendor usage page
//! (`0xFF00`), one per sensor. We want usage 3 (accelerometer) and usage 9 (gyroscope).
//!
//! Contrary to every published project that reads this sensor, **root is not required**.
//! Those projects enforce `geteuid() == 0` themselves; macOS does not. What access *is*
//! gated by is the Input Monitoring TCC grant, which we check explicitly.

use std::ffi::{c_void, CString};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dispatch2::{DispatchQueue, DispatchRetained};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_io_kit::{
    kIOMainPortDefault, IOHIDDevice, IOObjectRelease, IORegistryEntryCreateCFProperty,
    IOServiceMatching, IOIteratorNext, IORegistryEntrySetCFProperty,
};

use crate::report::{self, Sample, REPORT_LEN};
use crate::{Error, SensorKind};

// `IOServiceGetMatchingServices` is not bound by objc2-io-kit 0.3, and `IOHIDCheckAccess`
// / `IOHIDRequestAccess` live in IOHIDLib.h which the crate doesn't generate at all.
extern "C-unwind" {
    fn IOServiceGetMatchingServices(
        main_port: libc::mach_port_t,
        matching: *const CFDictionary,
        existing: *mut libc::mach_port_t,
    ) -> libc::kern_return_t;

    fn IOHIDCheckAccess(request: u32) -> u32;
    fn IOHIDRequestAccess(request: u32) -> bool;
}

const K_IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 0;
const K_IOHID_ACCESS_TYPE_GRANTED: u32 = 0;
const K_IOHID_ACCESS_TYPE_DENIED: u32 = 1;

/// Apple's vendor-defined HID usage page. The IMU is not on the standard sensor page.
const USAGE_PAGE_VENDOR: i32 = 0xFF00;
const USAGE_ACCEL: i32 = 3;
const USAGE_GYRO: i32 = 9;

/// The IOService class exposing the sensors as HID devices.
const CLASS_HID_DEVICE: &str = "AppleSPUHIDDevice";
/// The *driver* class. Power and reporting state live here, not on the HID device.
const CLASS_HID_DRIVER: &str = "AppleSPUHIDDriver";

/// Reporting interval that yields the sensor's native rate.
///
/// The hardware clamps to ~805 Hz, so asking for 1 kHz is how you get the native rate
/// rather than the 8000 µs (125 Hz) the system leaves it at. Larger values trade detection
/// quality for CPU: each report costs a kernel transition, and at 805 Hz across two
/// devices that is the daemon's single largest expense.
pub const NATIVE_REPORT_INTERVAL_US: u32 = 1000;

/// The interval the system leaves the sensors at when nothing has asked for more.
pub const DEFAULT_REPORT_INTERVAL_US: u32 = 8000;

/// Whether the current process may talk to HID devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Granted,
    Denied,
    Unknown,
}

/// Check the Input Monitoring (`kTCCServiceListenEvent`) grant for this process.
///
/// Input Monitoring gates *all* HID access on macOS, not just keyboards, so a denial here
/// will stop us opening the IMU even though it is not an input device in any useful sense.
pub fn check_access() -> Access {
    match unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) } {
        K_IOHID_ACCESS_TYPE_GRANTED => Access::Granted,
        K_IOHID_ACCESS_TYPE_DENIED => Access::Denied,
        _ => Access::Unknown,
    }
}

/// Prompt for Input Monitoring. Returns true if it was granted.
///
/// The prompt is attributed to the binary's signing identity, so re-signing with a
/// different identity resets the grant and this will be asked again.
pub fn request_access() -> bool {
    unsafe { IOHIDRequestAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) }
}

/// RAII wrapper over an `io_object_t`, which is a mach port name rather than a pointer.
struct IoObject(libc::mach_port_t);

impl Drop for IoObject {
    fn drop(&mut self) {
        if self.0 != 0 {
            IOObjectRelease(self.0);
        }
    }
}

/// Iterate the IOService registry for a class name, yielding each matching service.
///
/// `IOServiceGetMatchingServices` consumes a reference on the matching dictionary, hence
/// the `into_raw` — releasing it ourselves as well would be an over-release.
fn matching_services(class_name: &str) -> Result<Vec<IoObject>, Error> {
    let name = CString::new(class_name).expect("class name has no interior nul");
    let matching = unsafe { IOServiceMatching(name.as_ptr()) }
        .ok_or_else(|| Error::Iokit(format!("IOServiceMatching({class_name}) returned null")))?;

    let mut iter: libc::mach_port_t = 0;
    let kr = unsafe {
        IOServiceGetMatchingServices(
            kIOMainPortDefault,
            CFRetained::into_raw(matching).as_ptr().cast_const().cast(),
            &mut iter,
        )
    };
    if kr != 0 {
        return Err(Error::Iokit(format!(
            "IOServiceGetMatchingServices({class_name}) failed: 0x{kr:08x}"
        )));
    }
    let _iter_guard = IoObject(iter);

    let mut out = Vec::new();
    loop {
        let svc = IOIteratorNext(iter);
        if svc == 0 {
            break;
        }
        out.push(IoObject(svc));
    }
    Ok(out)
}

/// Read an integer property off an IORegistry entry.
fn int_property(service: libc::mach_port_t, key: &str) -> Option<i32> {
    let cf_key = CFString::from_str(key);
    let value: CFRetained<CFType> =
        unsafe { IORegistryEntryCreateCFProperty(service, Some(&cf_key), None, 0) }?;
    value.downcast_ref::<CFNumber>()?.as_i32()
}

/// Set an `SInt32` property on an IORegistry entry, ignoring failure.
fn set_int_property(service: libc::mach_port_t, key: &str, value: i32) {
    let cf_key = CFString::from_str(key);
    let cf_val = CFNumber::new_i32(value);
    // Deliberately unchecked: these can fail depending on privilege level and the sensor
    // streams anyway (often because something else already woke the SPU).
    unsafe {
        IORegistryEntrySetCFProperty(service, Some(&cf_key), Some(&cf_val));
    }
}

/// Bring the SPU sensors out of their idle state and raise the report rate.
///
/// This is the single most important non-obvious step in the whole driver. The power and
/// reporting state live on the **`AppleSPUHIDDriver`** service; setting the same keys on
/// the `IOHIDDevice` via `IOHIDDeviceSetProperty` is silently ignored, and the result is
/// that `IOHIDDeviceOpen` succeeds but the input report callback never fires even once.
/// It must also happen *before* the device is opened.
pub fn wake_sensors(report_interval_us: u32) {
    let Ok(drivers) = matching_services(CLASS_HID_DRIVER) else {
        return;
    };
    let interval = report_interval_us.clamp(1000, DEFAULT_REPORT_INTERVAL_US) as i32;
    for driver in &drivers {
        set_int_property(driver.0, "SensorPropertyReportingState", 1);
        set_int_property(driver.0, "SensorPropertyPowerState", 1);
        set_int_property(driver.0, "ReportInterval", interval);
    }
}

/// A sensor found in the IOService registry, before it has been opened.
pub struct FoundDevice {
    pub kind: SensorKind,
    service: IoObject,
}

/// Locate the accelerometer and gyroscope among the SPU HID devices.
///
/// Matching on usage page + usage alone is not sufficient: on a Mac16,5 that returns two
/// devices for the accelerometer usage. The report size disambiguates them.
pub fn find_devices() -> Result<Vec<FoundDevice>, Error> {
    let services = matching_services(CLASS_HID_DEVICE)?;
    if services.is_empty() {
        return Err(Error::NoSensor);
    }

    let mut found = Vec::new();
    for svc in services {
        let page = int_property(svc.0, "PrimaryUsagePage");
        let usage = int_property(svc.0, "PrimaryUsage");
        let size = int_property(svc.0, "MaxInputReportSize");

        if page != Some(USAGE_PAGE_VENDOR) || size != Some(REPORT_LEN as i32) {
            continue;
        }
        let kind = match usage {
            Some(USAGE_ACCEL) => SensorKind::Accel,
            Some(USAGE_GYRO) => SensorKind::Gyro,
            _ => continue,
        };
        if found.iter().any(|d: &FoundDevice| d.kind == kind) {
            continue;
        }
        found.push(FoundDevice { kind, service: svc });
    }

    if found.is_empty() {
        return Err(Error::NoSensor);
    }
    Ok(found)
}

/// Per-device state reachable from the IOKit report callback.
///
/// The callback runs on a serial dispatch queue, so `producer` and `last_seq` are only
/// ever touched from one thread. The counters are atomic because they are read from
/// whichever thread is consuming samples.
struct CallbackContext {
    producer: rtrb::Producer<Sample>,
    last_seq: Option<u16>,
    stats: Arc<Stats>,
}

/// Health counters for one sensor stream.
#[derive(Debug, Default)]
pub struct Stats {
    /// Reports delivered by the kernel and successfully parsed.
    pub received: AtomicU64,
    /// Reports the hardware produced but we never saw, inferred from sequence gaps.
    pub dropped: AtomicU64,
    /// Samples discarded because the consumer wasn't draining the ring fast enough.
    pub overruns: AtomicU64,
    /// Reports whose length wasn't 22 bytes.
    pub malformed: AtomicU64,
}

/// The IOKit input report callback.
///
/// This runs on the sensor's dispatch queue at ~805 Hz, so it does the minimum possible:
/// parse 22 bytes and push into a lock-free ring. All real work happens downstream.
unsafe extern "C-unwind" fn input_report_callback(
    context: *mut c_void,
    _result: objc2_io_kit::IOReturn,
    _sender: *mut c_void,
    _report_type: objc2_io_kit::IOHIDReportType,
    _report_id: u32,
    report: NonNull<u8>,
    report_length: isize,
    timestamp: u64,
) {
    if context.is_null() || report_length <= 0 {
        return;
    }
    let ctx = unsafe { &mut *(context as *mut CallbackContext) };
    let bytes = unsafe { std::slice::from_raw_parts(report.as_ptr(), report_length as usize) };

    let Some(sample) = report::parse(bytes, timestamp) else {
        ctx.stats.malformed.fetch_add(1, Ordering::Relaxed);
        return;
    };

    if let Some(prev) = ctx.last_seq {
        let missed = report::gap(prev, sample.seq);
        if missed > 0 {
            ctx.stats.dropped.fetch_add(u64::from(missed), Ordering::Relaxed);
        }
    }
    ctx.last_seq = Some(sample.seq);
    ctx.stats.received.fetch_add(1, Ordering::Relaxed);

    // Dropping on a full ring is deliberate: a stalled consumer must never apply
    // backpressure to the kernel's callback thread.
    if ctx.producer.push(sample).is_err() {
        ctx.stats.overruns.fetch_add(1, Ordering::Relaxed);
    }
}

/// An open, streaming sensor. Dropping this stops the stream and frees the callback state.
pub struct OpenDevice {
    device: CFRetained<IOHIDDevice>,
    _queue: DispatchRetained<DispatchQueue>,
    stats: Arc<Stats>,
    /// Kept alive for exactly as long as the device is registered — IOKit writes each
    /// incoming report into this buffer before invoking the callback.
    report_buf: Box<[u8; 64]>,
    /// Owned by IOKit for the device's lifetime; reclaimed in `Drop` after cancellation.
    context: *mut CallbackContext,
    cancelled: bool,
}

// The raw pointer is to heap state that only the (serial, device-owned) callback queue
// touches, and it is freed only after the device has been cancelled.
unsafe impl Send for OpenDevice {}

impl OpenDevice {
    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }
}

impl Drop for OpenDevice {
    fn drop(&mut self) {
        if !self.cancelled {
            self.device.cancel();
        }
        unsafe {
            self.device.close(0);
            drop(Box::from_raw(self.context));
        }
        let _ = &self.report_buf;
    }
}

/// Open a discovered sensor and begin streaming into a ring buffer.
///
/// Returns the consumer end of the ring alongside the open device. `capacity` is in
/// samples; at ~805 Hz, 8192 is roughly ten seconds of slack.
pub fn open(found: &FoundDevice, capacity: usize) -> Result<(OpenDevice, rtrb::Consumer<Sample>), Error> {
    let device = IOHIDDevice::new(None, found.service.0)
        .ok_or_else(|| Error::Iokit("IOHIDDeviceCreate returned null".into()))?;

    // kIOHIDOptionsTypeNone. Never seize: that would steal reports from the system's own
    // consumer of this device.
    let kr = device.open(0);
    if kr != 0 {
        return Err(Error::Open {
            kind: found.kind,
            code: kr,
        });
    }

    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    let stats = Arc::new(Stats::default());
    let context = Box::into_raw(Box::new(CallbackContext {
        producer,
        last_seq: None,
        stats: Arc::clone(&stats),
    }));

    let mut report_buf = Box::new([0u8; 64]);
    let buf_ptr = NonNull::new(report_buf.as_mut_ptr()).expect("boxed array is never null");

    let queue = DispatchQueue::new(
        match found.kind {
            SensorKind::Accel => "com.chipcolate.yamete.sensor.accel",
            SensorKind::Gyro => "com.chipcolate.yamete.sensor.gyro",
        },
        None,
    );

    unsafe {
        // The timestamped variant is free and gives us the kernel's mach_absolute_time
        // for each report, which beats stamping it ourselves after queue latency.
        device.register_input_report_with_time_stamp_callback(
            buf_ptr,
            report_buf.len() as isize,
            Some(input_report_callback),
            context.cast(),
        );
        device.set_dispatch_queue(&queue);
    }
    device.activate();

    Ok((
        OpenDevice {
            device,
            _queue: queue,
            stats,
            report_buf,
            context,
            cancelled: false,
        },
        consumer,
    ))
}
