//! Raw touch input for takeover mode: five fingers quit, while Carlito also
//! uses a short single-finger tap to turn answer pages.

use std::io;
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

const EV_SYN: u16 = 0;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const ABS_MT_SLOT: u16 = 47;
const ABS_MT_POSITION_X: u16 = 53;
const ABS_MT_POSITION_Y: u16 = 54;
const ABS_MT_TRACKING_ID: u16 = 57;
const EVIOCGRAB: libc::c_ulong = 0x40044590;
const MAX_SLOTS: usize = 16;

#[repr(C)]
#[derive(Clone, Copy)]
struct InputAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[derive(Default)]
pub struct TouchActions {
    pub quit: bool,
    pub tap: Option<(i32, i32)>,
    pub new_chat: bool,
}

pub struct TouchDevice {
    fd: RawFd,
    slots: [bool; MAX_SLOTS],
    x: [i32; MAX_SLOTS],
    y: [i32; MAX_SLOTS],
    start_x: [i32; MAX_SLOTS],
    start_y: [i32; MAX_SLOTS],
    have_start_x: [bool; MAX_SLOTS],
    have_start_y: [bool; MAX_SLOTS],
    started: [Option<Instant>; MAX_SLOTS],
    cur: usize,
    gesture_max: usize,
    pending_tap: Option<(i32, i32)>,
    pending_new_chat: bool,
    screen_w: i32,
    screen_h: i32,
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
}

impl TouchDevice {
    pub fn open() -> io::Result<Self> {
        Self::open_for_screen(1, 1)
    }

    pub fn open_for_screen(screen_w: usize, screen_h: usize) -> io::Result<Self> {
        for i in 0..8 {
            let name_path = format!("/sys/class/input/event{i}/device/name");
            if let Ok(name) = std::fs::read_to_string(&name_path) {
                let lower = name.to_lowercase();
                if lower.contains("touch") || lower.contains("pt_mt") || lower.contains("mt") {
                    let path = std::ffi::CString::new(format!("/dev/input/event{i}")).unwrap();
                    let fd =
                        unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
                    if fd < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    unsafe { libc::ioctl(fd, EVIOCGRAB, 1i32) };
                    let x_abs = abs_info(fd, ABS_MT_POSITION_X).unwrap_or(InputAbsInfo {
                        value: 0,
                        minimum: 0,
                        maximum: screen_w.saturating_sub(1) as i32,
                        fuzz: 0,
                        flat: 0,
                        resolution: 0,
                    });
                    let y_abs = abs_info(fd, ABS_MT_POSITION_Y).unwrap_or(InputAbsInfo {
                        value: 0,
                        minimum: 0,
                        maximum: screen_h.saturating_sub(1) as i32,
                        fuzz: 0,
                        flat: 0,
                        resolution: 0,
                    });
                    return Ok(Self {
                        fd,
                        slots: [false; MAX_SLOTS],
                        x: [0; MAX_SLOTS],
                        y: [0; MAX_SLOTS],
                        start_x: [0; MAX_SLOTS],
                        start_y: [0; MAX_SLOTS],
                        have_start_x: [false; MAX_SLOTS],
                        have_start_y: [false; MAX_SLOTS],
                        started: [None; MAX_SLOTS],
                        cur: 0,
                        gesture_max: 0,
                        pending_tap: None,
                        pending_new_chat: false,
                        screen_w: screen_w.max(1) as i32,
                        screen_h: screen_h.max(1) as i32,
                        min_x: x_abs.minimum,
                        max_x: x_abs.maximum,
                        min_y: y_abs.minimum,
                        max_y: y_abs.maximum,
                    });
                }
            }
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "no touch device"))
    }

    /// Returns true if a 5-finger touch was seen.
    pub fn drain_check_quit(&mut self) -> bool {
        self.drain().quit
    }

    pub fn drain(&mut self) -> TouchActions {
        let mut actions = TouchActions::default();
        let time_size = std::mem::size_of::<libc::timeval>();
        let event_size = time_size + 8;
        let mut buf = [0u8; 24 * 64];
        loop {
            let n =
                unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            for chunk in buf[..n as usize].chunks_exact(event_size) {
                let etype = u16::from_ne_bytes(chunk[time_size..time_size + 2].try_into().unwrap());
                let code =
                    u16::from_ne_bytes(chunk[time_size + 2..time_size + 4].try_into().unwrap());
                let value =
                    i32::from_ne_bytes(chunk[time_size + 4..time_size + 8].try_into().unwrap());
                if etype == EV_ABS && code == ABS_MT_SLOT {
                    self.cur = (value.max(0) as usize).min(MAX_SLOTS - 1);
                } else if etype == EV_ABS && code == ABS_MT_POSITION_X {
                    self.x[self.cur] = value;
                    if self.slots[self.cur] && !self.have_start_x[self.cur] {
                        self.start_x[self.cur] = value;
                        self.have_start_x[self.cur] = true;
                    }
                } else if etype == EV_ABS && code == ABS_MT_POSITION_Y {
                    self.y[self.cur] = value;
                    if self.slots[self.cur] && !self.have_start_y[self.cur] {
                        self.start_y[self.cur] = value;
                        self.have_start_y[self.cur] = true;
                    }
                } else if etype == EV_ABS && code == ABS_MT_TRACKING_ID {
                    if value != -1 {
                        self.slots[self.cur] = true;
                        self.started[self.cur] = Some(Instant::now());
                        self.have_start_x[self.cur] = false;
                        self.have_start_y[self.cur] = false;
                        let count = self.slots.iter().filter(|&&s| s).count();
                        self.gesture_max = self.gesture_max.max(count);
                        if count >= 5 {
                            actions.quit = true;
                        }
                    } else {
                        let elapsed = self.started[self.cur].take().map(|t| t.elapsed());
                        let start = (
                            map_axis(
                                self.start_x[self.cur],
                                self.min_x,
                                self.max_x,
                                self.screen_w,
                            ),
                            map_axis(
                                self.start_y[self.cur],
                                self.min_y,
                                self.max_y,
                                self.screen_h,
                            ),
                        );
                        let end = (
                            map_axis(self.x[self.cur], self.min_x, self.max_x, self.screen_w),
                            map_axis(self.y[self.cur], self.min_y, self.max_y, self.screen_h),
                        );
                        let moved = if self.have_start_x[self.cur] && self.have_start_y[self.cur] {
                            (end.0 - start.0).abs().max((end.1 - start.1).abs())
                        } else {
                            0
                        };
                        let dx = (end.0 - start.0).abs();
                        let dy = end.1 - start.1;
                        let tap_slop = self.screen_w.min(self.screen_h) / 20;
                        let swipe_thresh = (self.screen_h / 6).max(180);
                        if self.gesture_max == 1 {
                            let short = elapsed.is_some_and(|d| d <= Duration::from_millis(900));
                            if short && moved <= tap_slop {
                                self.pending_tap = Some((self.x[self.cur], self.y[self.cur]));
                            }
                        } else if self.gesture_max == 2 {
                            let swipe_ok = elapsed.is_some_and(|d| {
                                d >= Duration::from_millis(80) && d <= Duration::from_millis(1200)
                            });
                            if swipe_ok && dy > swipe_thresh && dx * 2 < dy {
                                self.pending_new_chat = true;
                            }
                        }
                        self.slots[self.cur] = false;
                    }
                } else if etype == EV_SYN && code == SYN_REPORT {
                    if !self.slots.iter().any(|&active| active) {
                        if self.gesture_max == 1 {
                            if let Some((x, y)) = self.pending_tap.take() {
                                actions.tap = Some((
                                    map_axis(x, self.min_x, self.max_x, self.screen_w),
                                    map_axis(y, self.min_y, self.max_y, self.screen_h),
                                ));
                            }
                        } else if self.gesture_max == 2 && self.pending_new_chat {
                            actions.new_chat = true;
                        }
                        self.pending_tap = None;
                        self.pending_new_chat = false;
                        self.gesture_max = 0;
                    }
                }
            }
        }
        actions
    }
}

fn eviocgabs(axis: u16) -> libc::c_ulong {
    const IOC_READ: libc::c_ulong = 2;
    const IOC_NRBITS: libc::c_ulong = 8;
    const IOC_TYPEBITS: libc::c_ulong = 8;
    const IOC_SIZEBITS: libc::c_ulong = 14;
    const IOC_TYPESHIFT: libc::c_ulong = IOC_NRBITS;
    const IOC_SIZESHIFT: libc::c_ulong = IOC_TYPESHIFT + IOC_TYPEBITS;
    const IOC_DIRSHIFT: libc::c_ulong = IOC_SIZESHIFT + IOC_SIZEBITS;
    (IOC_READ << IOC_DIRSHIFT)
        | ((std::mem::size_of::<InputAbsInfo>() as libc::c_ulong) << IOC_SIZESHIFT)
        | ((b'E' as libc::c_ulong) << IOC_TYPESHIFT)
        | (0x40 + axis) as libc::c_ulong
}

fn abs_info(fd: RawFd, axis: u16) -> io::Result<InputAbsInfo> {
    let mut info: InputAbsInfo = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(fd, eviocgabs(axis), &mut info) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

fn map_axis(raw: i32, min: i32, max: i32, screen: i32) -> i32 {
    let span = (max - min).max(1);
    let pos = (raw - min).clamp(0, span);
    (pos * (screen - 1) / span).clamp(0, screen - 1)
}

impl Drop for TouchDevice {
    fn drop(&mut self) {
        unsafe {
            libc::ioctl(self.fd, EVIOCGRAB, 0i32);
            libc::close(self.fd);
        }
    }
}
