use crate::input::{Action, UserEvent};
use hidapi::{HidApi, HidDevice};
use std::time::{Duration, Instant};
use winit::event_loop::EventLoopProxy;

const DUALSENSE_VID: u16 = 0x054c;
const DUALSENSE_PID: u16 = 0x0ce6;
const REPORT_ID_USB: u8 = 0x01;
const TOUCHPOINT0_OFFSET: usize = 33;

const SWIPE_MIN_DISTANCE: i32 = 220;
const SWIPE_MAX_DURATION: Duration = Duration::from_millis(700);
const SWIPE_COOLDOWN: Duration = Duration::from_millis(280);

pub fn spawn_dualsense_listener(proxy: EventLoopProxy<UserEvent>) -> std::thread::JoinHandle<()> {
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
                if listen_for_swipes(device, &proxy).is_err() {
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
) -> Result<(), hidapi::HidError> {
    let mut detector = SwipeDetector::new();
    let mut buf = [0u8; 64];
    loop {
        let len = device.read_timeout(&mut buf, 50)?;
        if len == 0 {
            continue;
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
