//! System sleep/wake notifications through IOKit.
//!
//! The CLI does not need root for these notifications.  On `WillSleep` the
//! engine is told to take the host link down; on `HasPoweredOn` it forces a
//! reset/re-initialization of the RNDIS session, which covers USB state that
//! did not survive the sleep.  If the USB handle itself is gone, the normal
//! reconnect supervisor takes over.
//!
//! `IORegisterForSystemPower` delivers callbacks on a dispatch queue.  Only
//! one monitor is expected per process, so the root port is kept in a static
//! atomic for the callback.  The monitor is removed in `Drop`.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const K_IOMESSAGE_CAN_SYSTEM_SLEEP: u32 = 0xE000_0270;
const K_IOMESSAGE_SYSTEM_WILL_SLEEP: u32 = 0xE000_0280;
const K_IOMESSAGE_SYSTEM_HAS_POWERED_ON: u32 = 0xE000_0300;

static SLEEPING: AtomicBool = AtomicBool::new(false);
static WAKE: AtomicBool = AtomicBool::new(false);
static ROOT_PORT: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    Sleep,
    Wake,
}

/// Pure mapping used by the callback and by unit tests.
pub fn event_for(message_type: u32) -> Option<PowerEvent> {
    match message_type {
        K_IOMESSAGE_SYSTEM_WILL_SLEEP => Some(PowerEvent::Sleep),
        K_IOMESSAGE_SYSTEM_HAS_POWERED_ON => Some(PowerEvent::Wake),
        _ => None,
    }
}

type IoObject = u32;
type IoConnect = u32;
type IoReturn = libc::c_int;
type IoNotificationPort = *mut libc::c_void;
type DispatchQueue = *mut libc::c_void;
type IoServiceInterestCallback = extern "C" fn(*mut libc::c_void, IoObject, u32, *mut libc::c_void);

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IORegisterForSystemPower(
        refcon: *mut libc::c_void,
        port: *mut IoNotificationPort,
        callback: IoServiceInterestCallback,
        notifier: *mut IoObject,
    ) -> IoConnect;
    fn IODeregisterForSystemPower(notifier: *mut IoObject) -> IoReturn;
    fn IONotificationPortSetDispatchQueue(port: IoNotificationPort, queue: DispatchQueue);
    fn IONotificationPortDestroy(port: IoNotificationPort);
    fn IOServiceClose(connect: IoConnect) -> IoReturn;
    fn IOAllowPowerChange(connect: IoConnect, notification_id: libc::intptr_t) -> IoReturn;
}

extern "C" {
    fn dispatch_get_global_queue(identifier: libc::c_long, flags: libc::c_ulong) -> DispatchQueue;
}

extern "C" fn power_callback(
    _refcon: *mut libc::c_void,
    _service: IoObject,
    message_type: u32,
    message_argument: *mut libc::c_void,
) {
    if message_type == K_IOMESSAGE_CAN_SYSTEM_SLEEP {
        acknowledge(message_argument);
        return;
    }
    match event_for(message_type) {
        Some(PowerEvent::Sleep) => {
            SLEEPING.store(true, Ordering::SeqCst);
            acknowledge(message_argument);
        }
        Some(PowerEvent::Wake) => {
            SLEEPING.store(false, Ordering::SeqCst);
            WAKE.store(true, Ordering::SeqCst);
        }
        None => {}
    }
}

fn acknowledge(message_argument: *mut libc::c_void) {
    let root_port = ROOT_PORT.load(Ordering::SeqCst);
    if root_port != 0 {
        // The notification identifier is passed through the message argument.
        unsafe {
            IOAllowPowerChange(root_port, message_argument as libc::intptr_t);
        }
    }
}

pub struct PowerMonitor {
    port: IoNotificationPort,
    notifier: IoObject,
    root_port: IoConnect,
}

impl PowerMonitor {
    pub fn start() -> Result<Self, String> {
        let mut port: IoNotificationPort = std::ptr::null_mut();
        let mut notifier: IoObject = 0;
        let root_port = unsafe {
            IORegisterForSystemPower(
                std::ptr::null_mut(),
                &mut port,
                power_callback,
                &mut notifier,
            )
        };
        if root_port == 0 || port.is_null() {
            return Err("IORegisterForSystemPower returned no root port".to_string());
        }
        ROOT_PORT.store(root_port, Ordering::SeqCst);
        SLEEPING.store(false, Ordering::SeqCst);
        WAKE.store(false, Ordering::SeqCst);
        let queue = unsafe { dispatch_get_global_queue(0, 0) };
        if queue.is_null() {
            unsafe {
                IODeregisterForSystemPower(&mut notifier);
                IONotificationPortDestroy(port);
                IOServiceClose(root_port);
            }
            ROOT_PORT.store(0, Ordering::SeqCst);
            return Err("dispatch_get_global_queue returned no queue".to_string());
        }
        unsafe { IONotificationPortSetDispatchQueue(port, queue) };
        Ok(Self {
            port,
            notifier,
            root_port,
        })
    }

    pub fn sleeping_flag(&self) -> &'static AtomicBool {
        &SLEEPING
    }

    pub fn wake_flag(&self) -> &'static AtomicBool {
        &WAKE
    }

    pub fn sleeping(&self) -> bool {
        SLEEPING.load(Ordering::SeqCst)
    }

    pub fn take_wake(&self) -> bool {
        WAKE.swap(false, Ordering::SeqCst)
    }
}

impl Drop for PowerMonitor {
    fn drop(&mut self) {
        ROOT_PORT.store(0, Ordering::SeqCst);
        unsafe {
            IODeregisterForSystemPower(&mut self.notifier);
            IONotificationPortDestroy(self.port);
            IOServiceClose(self.root_port);
        }
        SLEEPING.store(false, Ordering::SeqCst);
        WAKE.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_sleep_and_wake_messages() {
        assert_eq!(
            event_for(K_IOMESSAGE_SYSTEM_WILL_SLEEP),
            Some(PowerEvent::Sleep)
        );
        assert_eq!(
            event_for(K_IOMESSAGE_SYSTEM_HAS_POWERED_ON),
            Some(PowerEvent::Wake)
        );
        assert_eq!(event_for(K_IOMESSAGE_CAN_SYSTEM_SLEEP), None);
        assert_eq!(event_for(0), None);
    }

    #[test]
    fn monitor_registers_and_unregisters() {
        let monitor = PowerMonitor::start().expect("register power monitor");
        assert!(!monitor.sleeping());
        drop(monitor);
    }
}
