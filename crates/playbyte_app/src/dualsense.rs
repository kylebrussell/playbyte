use crate::input::{Action, UserEvent};
use gilrs::Button;
use hidapi::{HidApi, HidDevice};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use winit::event_loop::EventLoopProxy;

const DUALSENSE_VID: u16 = 0x054c;
const DUALSENSE_PID: u16 = 0x0ce6;
const REPORT_ID_USB: u8 = 0x01;
const TOUCHPOINT0_OFFSET: usize = 33;
const BUTTONS0_OFFSET: usize = 8;
const BUTTONS1_OFFSET: usize = 9;
const BUTTONS2_OFFSET: usize = 10;

const SWIPE_MIN_DISTANCE: i32 = 220;
const SWIPE_MAX_DURATION: Duration = Duration::from_millis(700);
const SWIPE_COOLDOWN: Duration = Duration::from_millis(280);

pub fn spawn_dualsense_listener(
    proxy: EventLoopProxy<UserEvent>,
    buttons_enabled: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut last_scan = Instant::now() - Duration::from_secs(5);
        loop {
            if last_scan.elapsed() < Duration::from_millis(400) {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            last_scan = Instant::now();
            let api = match HidApi::new() {
                Ok(api) => api,
                Err(_) => continue,
            };
            if let Some(device) = open_dualsense(&api) {
                if listen_for_swipes(device, &proxy, &buttons_enabled).is_err() {
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        }
    })
}

fn open_dualsense(api: &HidApi) -> Option<HidDevice> {
    let device = api
        .device_list()
        .find(|d| d.vendor_id() == DUALSENSE_VID && d.product_id() == DUALSENSE_PID)
        .and_then(|d| api.open_path(d.path()).ok());
    if let Some(device) = device.as_ref() {
        let _ = device.set_blocking_mode(false);
    }
    device
}

fn listen_for_swipes(
    device: HidDevice,
    proxy: &EventLoopProxy<UserEvent>,
    buttons_enabled: &Arc<AtomicBool>,
) -> Result<(), hidapi::HidError> {
    let mut detector = SwipeDetector::new();
    let mut last_buttons = DualsenseButtons::default();
    let mut seen_buttons = false;
    let mut buf = [0u8; 64];
    loop {
        let len = device.read_timeout(&mut buf, 50)?;
        if len == 0 {
            continue;
        }
        if buttons_enabled.load(Ordering::Relaxed) {
            if let Some(buttons) = parse_button_state(&buf[..len]) {
                if !seen_buttons {
                    last_buttons = buttons;
                    seen_buttons = true;
                } else {
                    if emit_button_changes(&mut last_buttons, buttons, proxy) {
                        return Ok(());
                    }
                }
            }
        }
        if let Some(sample) = parse_touch_sample(&buf[..len]) {
            if let Some(action) = detector.update(sample) {
                if proxy.send_event(UserEvent::Action(action)).is_err() {
                    return Ok(());
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct TouchSample {
    active: bool,
    x: u16,
}

#[derive(Clone, Copy, Default)]
struct DualsenseButtons {
    dpad_up: bool,
    dpad_down: bool,
    dpad_left: bool,
    dpad_right: bool,
    square: bool,
    cross: bool,
    circle: bool,
    triangle: bool,
    l1: bool,
    r1: bool,
    l2: bool,
    r2: bool,
    share: bool,
    options: bool,
}

fn parse_button_state(report: &[u8]) -> Option<DualsenseButtons> {
    if report.is_empty() || report[0] != REPORT_ID_USB {
        return None;
    }
    if report.len() <= BUTTONS2_OFFSET {
        return None;
    }
    let buttons0 = report[BUTTONS0_OFFSET];
    let buttons1 = report[BUTTONS1_OFFSET];

    let dpad = buttons0 & 0x0f;
    let (dpad_up, dpad_right, dpad_down, dpad_left) = match dpad {
        0 => (true, false, false, false),
        1 => (true, true, false, false),
        2 => (false, true, false, false),
        3 => (false, true, true, false),
        4 => (false, false, true, false),
        5 => (false, false, true, true),
        6 => (false, false, false, true),
        7 => (true, false, false, true),
        _ => (false, false, false, false),
    };

    Some(DualsenseButtons {
        dpad_up,
        dpad_down,
        dpad_left,
        dpad_right,
        square: (buttons0 & 0x10) != 0,
        cross: (buttons0 & 0x20) != 0,
        circle: (buttons0 & 0x40) != 0,
        triangle: (buttons0 & 0x80) != 0,
        l1: (buttons1 & 0x01) != 0,
        r1: (buttons1 & 0x02) != 0,
        l2: (buttons1 & 0x04) != 0,
        r2: (buttons1 & 0x08) != 0,
        share: (buttons1 & 0x10) != 0,
        options: (buttons1 & 0x20) != 0,
    })
}

fn emit_button_changes(
    prev: &mut DualsenseButtons,
    next: DualsenseButtons,
    proxy: &EventLoopProxy<UserEvent>,
) -> bool {
    let changes = [
        (Button::DPadUp, prev.dpad_up, next.dpad_up),
        (Button::DPadDown, prev.dpad_down, next.dpad_down),
        (Button::DPadLeft, prev.dpad_left, next.dpad_left),
        (Button::DPadRight, prev.dpad_right, next.dpad_right),
        (Button::West, prev.square, next.square),
        (Button::South, prev.cross, next.cross),
        (Button::East, prev.circle, next.circle),
        (Button::North, prev.triangle, next.triangle),
        (Button::LeftTrigger, prev.l1, next.l1),
        (Button::RightTrigger, prev.r1, next.r1),
        (Button::LeftTrigger2, prev.l2, next.l2),
        (Button::RightTrigger2, prev.r2, next.r2),
        (Button::Select, prev.share, next.share),
        (Button::Start, prev.options, next.options),
    ];

    for (button, before, after) in changes {
        if before != after {
            if proxy
                .send_event(UserEvent::GamepadButton {
                    button,
                    pressed: after,
                })
                .is_err()
            {
                return true;
            }
        }
    }

    *prev = next;
    false
}

fn parse_touch_sample(report: &[u8]) -> Option<TouchSample> {
    if report.is_empty() || report[0] != REPORT_ID_USB {
        return None;
    }
    if report.len() <= TOUCHPOINT0_OFFSET + 3 {
        return None;
    }
    let contact = report[TOUCHPOINT0_OFFSET];
    let active = (contact & 0x80) == 0;
    let x_lo = report[TOUCHPOINT0_OFFSET + 1];
    let x_hi_y_lo = report[TOUCHPOINT0_OFFSET + 2];
    let y_hi = report[TOUCHPOINT0_OFFSET + 3];

    // Packed format: x_hi is low nibble, y_lo is high nibble.
    let x = (((x_hi_y_lo & 0x0f) as u16) << 8) | x_lo as u16;
    let _y = ((y_hi as u16) << 4) | ((x_hi_y_lo as u16) >> 4);

    Some(TouchSample { active, x })
}

struct SwipeDetector {
    active: bool,
    start_x: u16,
    last_x: u16,
    start_time: Instant,
    last_swipe: Instant,
}

impl SwipeDetector {
    fn new() -> Self {
        Self {
            active: false,
            start_x: 0,
            last_x: 0,
            start_time: Instant::now(),
            last_swipe: Instant::now() - SWIPE_COOLDOWN,
        }
    }

    fn update(&mut self, sample: TouchSample) -> Option<Action> {
        let now = Instant::now();
        if sample.active {
            if !self.active {
                self.active = true;
                self.start_x = sample.x;
                self.last_x = sample.x;
                self.start_time = now;
            } else {
                self.last_x = sample.x;
            }
            return None;
        }

        if self.active {
            self.active = false;
            let dt = now.saturating_duration_since(self.start_time);
            if dt > SWIPE_MAX_DURATION {
                return None;
            }
            if now.saturating_duration_since(self.last_swipe) < SWIPE_COOLDOWN {
                return None;
            }
            let dx = self.last_x as i32 - self.start_x as i32;
            if dx.abs() >= SWIPE_MIN_DISTANCE {
                self.last_swipe = now;
                if dx < 0 {
                    return Some(Action::NextItem);
                } else {
                    return Some(Action::PrevItem);
                }
            }
        }
        None
    }
}
