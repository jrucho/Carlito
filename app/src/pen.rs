//! Raw evdev pen input: the full digitizer, bypassing Qt's filtered view.
//! Gives us 0-4096 pressure, tilt, hover, and the eraser tip (BTN_TOOL_RUBBER),
//! at the hardware event rate.
//!
//! The device is grabbed (EVIOCGRAB) while the diary is open so xochitl
//! doesn't also react to the pen; released automatically on close/exit.

use std::io;
use std::os::fd::RawFd;

pub const MAX_PRESSURE: i32 = 4096;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_PRESSURE: u16 = 24;
const BTN_TOOL_PEN: u16 = 320;
const BTN_TOOL_RUBBER: u16 = 321;
const BTN_TOUCH: u16 = 330;

const EVIOCGRAB: libc::c_ulong = 0x40044590;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct InputAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Eraser,
}

#[derive(Debug, Clone, Copy)]
pub struct PenSample {
    /// Screen coordinates.
    pub x: i32,
    pub y: i32,
    /// 0..4096
    pub pressure: i32,
    pub tool: Tool,
    pub touching: bool,
}

pub struct PenDevice {
    fd: RawFd,
    // Accumulated state between SYN_REPORTs.
    raw_x: i32,
    raw_y: i32,
    pressure: i32,
    tool: Tool,
    touching: bool,
    dirty: bool,
    screen_w: i32,
    screen_h: i32,
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
}

impl PenDevice {
    /// Find and grab the marker input device.
    pub fn open(screen_w: usize, screen_h: usize) -> io::Result<Self> {
        let path = find_marker_device()?;
        let cpath = std::ffi::CString::new(path.clone()).unwrap();
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let grab = unsafe { libc::ioctl(fd, EVIOCGRAB, 1i32) };
        if grab != 0 {
            eprintln!(
                "riddle: warning: EVIOCGRAB failed ({}) — xochitl will also see the pen",
                io::Error::last_os_error()
            );
        }
        let x_abs = abs_info(fd, ABS_X).unwrap_or(InputAbsInfo {
            value: 0,
            minimum: 0,
            maximum: screen_w as i32 - 1,
            fuzz: 0,
            flat: 0,
            resolution: 0,
        });
        let y_abs = abs_info(fd, ABS_Y).unwrap_or(InputAbsInfo {
            value: 0,
            minimum: 0,
            maximum: screen_h as i32 - 1,
            fuzz: 0,
            flat: 0,
            resolution: 0,
        });
        eprintln!(
            "riddle: pen device {path} opened (grabbed: {}, x={}..{}, y={}..{})",
            grab == 0,
            x_abs.minimum,
            x_abs.maximum,
            y_abs.minimum,
            y_abs.maximum
        );
        Ok(Self {
            fd,
            raw_x: 0,
            raw_y: 0,
            pressure: 0,
            tool: Tool::Pen,
            touching: false,
            dirty: false,
            screen_w: screen_w as i32,
            screen_h: screen_h as i32,
            min_x: x_abs.minimum,
            max_x: x_abs.maximum,
            min_y: y_abs.minimum,
            max_y: y_abs.maximum,
        })
    }

    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }

    /// Drain all pending events; returns one sample per SYN_REPORT frame
    /// that changed state.
    pub fn drain(&mut self) -> Vec<PenSample> {
        let mut out = Vec::new();
        // `timeval` is 16 bytes on the Move (aarch64) and 8 on rM2 (armv7).
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
                match (etype, code) {
                    (EV_ABS, ABS_X) => {
                        self.raw_x = value;
                        self.dirty = true;
                    }
                    (EV_ABS, ABS_Y) => {
                        self.raw_y = value;
                        self.dirty = true;
                    }
                    (EV_ABS, ABS_PRESSURE) => {
                        self.pressure = value;
                        self.dirty = true;
                    }
                    (EV_KEY, BTN_TOOL_PEN) if value == 1 => {
                        self.tool = Tool::Pen;
                    }
                    (EV_KEY, BTN_TOOL_RUBBER) => {
                        self.tool = if value == 1 { Tool::Eraser } else { Tool::Pen };
                    }
                    (EV_KEY, BTN_TOUCH) => {
                        self.touching = value == 1;
                        self.dirty = true;
                    }
                    (EV_SYN, SYN_REPORT) => {
                        if self.dirty {
                            self.dirty = false;
                            out.push(PenSample {
                                x: map_axis(self.raw_x, self.min_x, self.max_x, self.screen_w),
                                y: map_axis(self.raw_y, self.min_y, self.max_y, self.screen_h),
                                pressure: self.pressure,
                                tool: self.tool,
                                touching: self.touching,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        out
    }
}

fn eviocgabs(axis: u16) -> libc::c_ulong {
    const IOC_READ: libc::c_ulong = 2;
    const IOC_NRBITS: libc::c_ulong = 8;
    const IOC_TYPEBITS: libc::c_ulong = 8;
    const IOC_SIZEBITS: libc::c_ulong = 14;
    const IOC_NRSHIFT: libc::c_ulong = 0;
    const IOC_TYPESHIFT: libc::c_ulong = IOC_NRSHIFT + IOC_NRBITS;
    const IOC_SIZESHIFT: libc::c_ulong = IOC_TYPESHIFT + IOC_TYPEBITS;
    const IOC_DIRSHIFT: libc::c_ulong = IOC_SIZESHIFT + IOC_SIZEBITS;

    (IOC_READ << IOC_DIRSHIFT)
        | ((std::mem::size_of::<InputAbsInfo>() as libc::c_ulong) << IOC_SIZESHIFT)
        | ((b'E' as libc::c_ulong) << IOC_TYPESHIFT)
        | (((0x40 + axis) as libc::c_ulong) << IOC_NRSHIFT)
}

fn abs_info(fd: RawFd, axis: u16) -> io::Result<InputAbsInfo> {
    let mut info = InputAbsInfo {
        value: 0,
        minimum: 0,
        maximum: 0,
        fuzz: 0,
        flat: 0,
        resolution: 0,
    };
    let rc = unsafe { libc::ioctl(fd, eviocgabs(axis), &mut info) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

fn map_axis(raw: i32, min: i32, max: i32, screen: i32) -> i32 {
    let span = (max - min).max(1);
    let pos = (raw - min).clamp(0, span);
    (pos * (screen - 1) / span).clamp(0, screen - 1)
}

impl Drop for PenDevice {
    fn drop(&mut self) {
        unsafe {
            libc::ioctl(self.fd, EVIOCGRAB, 0i32);
            libc::close(self.fd);
        }
    }
}

fn find_marker_device() -> io::Result<String> {
    for i in 0..8 {
        let name_path = format!("/sys/class/input/event{i}/device/name");
        if let Ok(name) = std::fs::read_to_string(&name_path) {
            let lower = name.to_lowercase();
            if lower.contains("marker") || lower.contains("wacom") || lower.contains("digitizer") {
                return Ok(format!("/dev/input/event{i}"));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no marker input device found",
    ))
}
