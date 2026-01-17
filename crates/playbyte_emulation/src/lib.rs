use playbyte_libretro::{Callbacks, LibretroCore, LibretroError, RetroPixelFormat, VideoFrame};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
};
use thiserror::Error;

pub const RETRO_DEVICE_JOYPAD: u32 = 1;
pub const RETRO_DEVICE_ID_JOYPAD_B: u32 = 0;
pub const RETRO_DEVICE_ID_JOYPAD_Y: u32 = 1;
pub const RETRO_DEVICE_ID_JOYPAD_SELECT: u32 = 2;
pub const RETRO_DEVICE_ID_JOYPAD_START: u32 = 3;
pub const RETRO_DEVICE_ID_JOYPAD_UP: u32 = 4;
pub const RETRO_DEVICE_ID_JOYPAD_DOWN: u32 = 5;
pub const RETRO_DEVICE_ID_JOYPAD_LEFT: u32 = 6;
pub const RETRO_DEVICE_ID_JOYPAD_RIGHT: u32 = 7;
pub const RETRO_DEVICE_ID_JOYPAD_A: u32 = 8;
pub const RETRO_DEVICE_ID_JOYPAD_X: u32 = 9;
pub const RETRO_DEVICE_ID_JOYPAD_L: u32 = 10;
pub const RETRO_DEVICE_ID_JOYPAD_R: u32 = 11;

#[derive(Debug, Default, Clone)]
pub struct JoypadState {
    pub a: bool,
    pub b: bool,
    pub x: bool,
    pub y: bool,
    pub l: bool,
    pub r: bool,
    pub start: bool,
    pub select: bool,
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
}

impl JoypadState {
    pub fn set_button(&mut self, id: u32, pressed: bool) {
        match id {
            RETRO_DEVICE_ID_JOYPAD_A => self.a = pressed,
            RETRO_DEVICE_ID_JOYPAD_B => self.b = pressed,
            RETRO_DEVICE_ID_JOYPAD_X => self.x = pressed,
            RETRO_DEVICE_ID_JOYPAD_Y => self.y = pressed,
            RETRO_DEVICE_ID_JOYPAD_L => self.l = pressed,
            RETRO_DEVICE_ID_JOYPAD_R => self.r = pressed,
            RETRO_DEVICE_ID_JOYPAD_START => self.start = pressed,
            RETRO_DEVICE_ID_JOYPAD_SELECT => self.select = pressed,
            RETRO_DEVICE_ID_JOYPAD_UP => self.up = pressed,
            RETRO_DEVICE_ID_JOYPAD_DOWN => self.down = pressed,
            RETRO_DEVICE_ID_JOYPAD_LEFT => self.left = pressed,
            RETRO_DEVICE_ID_JOYPAD_RIGHT => self.right = pressed,
            _ => {}
        }
    }

    pub fn value_for_id(&self, id: u32) -> i16 {
        let pressed = match id {
            RETRO_DEVICE_ID_JOYPAD_A => self.a,
            RETRO_DEVICE_ID_JOYPAD_B => self.b,
            RETRO_DEVICE_ID_JOYPAD_X => self.x,
            RETRO_DEVICE_ID_JOYPAD_Y => self.y,
            RETRO_DEVICE_ID_JOYPAD_L => self.l,
            RETRO_DEVICE_ID_JOYPAD_R => self.r,
            RETRO_DEVICE_ID_JOYPAD_START => self.start,
            RETRO_DEVICE_ID_JOYPAD_SELECT => self.select,
            RETRO_DEVICE_ID_JOYPAD_UP => self.up,
            RETRO_DEVICE_ID_JOYPAD_DOWN => self.down,
            RETRO_DEVICE_ID_JOYPAD_LEFT => self.left,
            RETRO_DEVICE_ID_JOYPAD_RIGHT => self.right,
            _ => false,
        };
        if pressed {
            1
        } else {
            0
        }
    }
}

#[derive(Debug)]
pub struct AudioRingBuffer {
    inner: Mutex<VecDeque<i16>>,
    capacity: usize,
}

impl AudioRingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    pub fn push_samples(&self, samples: &[i16]) {
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        for &sample in samples {
            if guard.len() == self.capacity {
                guard.pop_front();
            }
            guard.push_back(sample);
        }
    }

    pub fn pop_samples(&self, out: &mut [i16]) {
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        for sample in out.iter_mut() {
            *sample = guard.pop_front().unwrap_or(0);
        }
    }
}

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error(transparent)]
    Libretro(#[from] LibretroError),
}

pub struct EmulatorRuntime {
    core: LibretroCore,
    input_state: Arc<Mutex<JoypadState>>,
    audio: Arc<AudioRingBuffer>,
    latest_frame: Arc<Mutex<Option<VideoFrame>>>,
    fps: f64,
}

impl EmulatorRuntime {
    pub fn new(
        core_path: impl AsRef<Path>,
        rom_path: impl AsRef<Path>,
    ) -> Result<Self, RuntimeError> {
        let latest_frame = Arc::new(Mutex::new(None));
        let latest_frame_cb = Arc::clone(&latest_frame);

        let audio = Arc::new(AudioRingBuffer::new(48_000 * 2));
        let audio_cb = Arc::clone(&audio);

        let input_state = Arc::new(Mutex::new(JoypadState::default()));
        let input_cb = Arc::clone(&input_state);

        let callbacks = Callbacks::new(
            Box::new(move |data, width, height, pitch, format| {
                let mut guard = latest_frame_cb.lock().expect("latest frame lock poisoned");
                *guard = Some(VideoFrame {
                    width,
                    height,
                    pitch,
                    pixel_format: format,
                    data: data.to_vec(),
                });
            }),
            Box::new(move |samples| {
                audio_cb.push_samples(samples);
            }),
            Box::new(|| {}),
            Box::new(move |port, device, _index, id| {
                if port != 0 || device != RETRO_DEVICE_JOYPAD {
                    return 0;
                }
                let guard = input_cb.lock().expect("input lock poisoned");
                guard.value_for_id(id)
            }),
        );

        let mut core = LibretroCore::load(core_path, callbacks)?;
        core.load_game(rom_path)?;
        let av_info = core.system_av_info();
        let fps = if av_info.timing.fps > 0.0 {
            av_info.timing.fps
        } else {
            60.0
        };

        Ok(Self {
            core,
            input_state,
            audio,
            latest_frame,
            fps,
        })
    }

    pub fn fps(&self) -> f64 {
        self.fps
    }

    pub fn system_info(&self) -> &playbyte_libretro::SystemInfo {
        self.core.system_info()
    }

    pub fn pixel_format(&self) -> RetroPixelFormat {
        self.core.pixel_format()
    }

    pub fn run_frame(&mut self) {
        self.core.run_frame();
    }

    pub fn latest_frame(&self) -> Option<VideoFrame> {
        self.latest_frame
            .lock()
            .ok()
            .and_then(|frame| frame.clone())
    }

    pub fn audio_buffer(&self) -> Arc<AudioRingBuffer> {
        Arc::clone(&self.audio)
    }

    pub fn input_state(&self) -> Arc<Mutex<JoypadState>> {
        Arc::clone(&self.input_state)
    }

    pub fn serialize(&self) -> Result<Vec<u8>, RuntimeError> {
        Ok(self.core.serialize()?)
    }

    pub fn unserialize(&self, data: &[u8]) -> Result<(), RuntimeError> {
        Ok(self.core.unserialize(data)?)
    }
}
