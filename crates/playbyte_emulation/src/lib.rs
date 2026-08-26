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

    /// Pop one interleaved stereo frame (left, right). If an odd sample is
    /// stranded at the end, it is duplicated so channel alignment is preserved.
    /// Returns `None` when the buffer is empty.
    pub fn pop_frame(&self) -> Option<[i16; 2]> {
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let left = guard.pop_front()?;
        let right = if guard.is_empty() {
            left
        } else {
            guard.pop_front()?
        };
        Some([left, right])
    }
}

/// Fallback core output rate used when a libretro core reports something
/// implausible via `retro_get_system_av_info`.
pub const DEFAULT_CORE_SAMPLE_RATE: f64 = 32_040.0;

/// Linear-interpolating resampler that converts an interleaved stereo source
/// (e.g. a libretro core's native output rate) to a device output rate. The
/// fractional read position is carried across calls, so no pitch drift
/// accumulates between audio callbacks.
pub struct LinearResampler {
    ratio: f64,
    pos: f64,
    prev: [f32; 2],
    cur: [f32; 2],
    cur_index: i64,
}

impl LinearResampler {
    pub fn new(src_rate: f64, dst_rate: f64) -> Self {
        let ratio =
            if src_rate.is_finite() && dst_rate.is_finite() && src_rate > 0.0 && dst_rate > 0.0 {
                src_rate / dst_rate
            } else {
                1.0
            };
        Self {
            ratio,
            pos: 0.0,
            prev: [0.0; 2],
            cur: [0.0; 2],
            cur_index: -1,
        }
    }

    /// Fill `out` (interleaved i16 samples, `out_channels` per frame; values
    /// other than 1 are treated as stereo) with resampled audio pulled from
    /// `next_frame`. On source underrun, stops writing (leaving zeros), keeps
    /// its position, and resumes seamlessly once frames are available again.
    /// Returns the number of complete output frames written.
    pub fn process(
        &mut self,
        out: &mut [i16],
        out_channels: usize,
        mut next_frame: impl FnMut() -> Option<[i16; 2]>,
    ) -> usize {
        let channels = if out_channels == 1 { 1 } else { 2 };
        out.fill(0);
        let out_frames = out.len() / channels;
        let mut written = 0usize;

        for frame in 0..out_frames {
            let n = self.pos.floor();
            if !self.ensure_pair((n + 1.0) as i64, &mut next_frame) {
                // Underrun: leave remaining slots zeroed and retry this
                // position on the next callback instead of advancing time.
                break;
            }
            let frac = (self.pos - n) as f32;
            let left = self.prev[0] + (self.cur[0] - self.prev[0]) * frac;
            let right = self.prev[1] + (self.cur[1] - self.prev[1]) * frac;

            let base = frame * channels;
            if channels == 1 {
                out[base] = to_i16((left + right) * 0.5);
            } else {
                out[base] = to_i16(left);
                out[base + 1] = to_i16(right);
            }
            written += 1;
            self.pos += self.ratio;
        }
        written
    }

    /// Ensure `cur` holds source frame `up_to` (and `prev` frame `up_to - 1`).
    fn ensure_pair(&mut self, up_to: i64, next: &mut impl FnMut() -> Option<[i16; 2]>) -> bool {
        while self.cur_index < up_to {
            match next() {
                Some([left, right]) => {
                    self.prev = self.cur;
                    self.cur = [left as f32 / 32_768.0, right as f32 / 32_768.0];
                    self.cur_index += 1;
                }
                None => return false,
            }
        }
        true
    }
}

fn to_i16(value: f32) -> i16 {
    (value * 32_767.0).round().clamp(-32_768.0, 32_767.0) as i16
}

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error(transparent)]
    Libretro(#[from] LibretroError),
    #[error("ROM extension '.{rom_ext}' not supported by core '{core}' (valid extensions: {valid_extensions})")]
    IncompatibleRom {
        core: String,
        rom_ext: String,
        valid_extensions: String,
    },
}

pub struct EmulatorRuntime {
    core: LibretroCore,
    input_state: Arc<Mutex<JoypadState>>,
    audio: Arc<AudioRingBuffer>,
    latest_frame: Arc<Mutex<Option<VideoFrame>>>,
    fps: f64,
    aspect_ratio: f32,
    sample_rate: f64,
}

impl EmulatorRuntime {
    pub fn new(
        core_path: impl AsRef<Path>,
        rom_path: impl AsRef<Path>,
    ) -> Result<Self, RuntimeError> {
        let rom_extension = rom_path
            .as_ref()
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase());

        let latest_frame = Arc::new(Mutex::new(None));
        let latest_frame_cb = Arc::clone(&latest_frame);

        let audio = Arc::new(AudioRingBuffer::new(48_000 * 2));
        let audio_cb = Arc::clone(&audio);

        let input_state = Arc::new(Mutex::new(JoypadState::default()));
        let input_cb = Arc::clone(&input_state);

        let callbacks = Callbacks::new(
            Box::new(move |data, width, height, pitch, format| {
                let mut guard = latest_frame_cb
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
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
                let guard = input_cb
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                guard.value_for_id(id)
            }),
        );

        // Load the library exactly once. Extension compatibility is checked
        // against system_info() after retro_init (which LibretroCore::load runs),
        // and before load_game so incompatible ROMs fail fast without full game
        // initialization.
        let mut core = LibretroCore::load(core_path, callbacks)?;
        if let Some(ext) = rom_extension.as_deref() {
            let info = core.system_info();
            if !core_supports_extension(&info.valid_extensions, ext) {
                return Err(RuntimeError::IncompatibleRom {
                    core: info.library_name.clone(),
                    rom_ext: ext.to_string(),
                    valid_extensions: info.valid_extensions.clone(),
                });
            }
        }
        core.load_game(rom_path)?;
        let av_info = core.system_av_info();
        let fps = if av_info.timing.fps.is_finite()
            && av_info.timing.fps >= 1.0
            && av_info.timing.fps <= 240.0
        {
            av_info.timing.fps
        } else {
            60.0
        };
        let aspect_ratio =
            if av_info.geometry.aspect_ratio.is_finite() && av_info.geometry.aspect_ratio > 0.0 {
                av_info.geometry.aspect_ratio
            } else if av_info.geometry.base_width > 0 && av_info.geometry.base_height > 0 {
                av_info.geometry.base_width as f32 / av_info.geometry.base_height as f32
            } else {
                4.0 / 3.0
            };
        let sample_rate = if av_info.timing.sample_rate.is_finite()
            && (8_000.0..=192_000.0).contains(&av_info.timing.sample_rate)
        {
            av_info.timing.sample_rate
        } else {
            DEFAULT_CORE_SAMPLE_RATE
        };

        Ok(Self {
            core,
            input_state,
            audio,
            latest_frame,
            fps,
            aspect_ratio,
            sample_rate,
        })
    }

    pub fn fps(&self) -> f64 {
        self.fps
    }

    /// Core output sample rate in Hz (from libretro timing, with a sane
    /// fallback when the core reports something unusable).
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Content aspect ratio reported by the core (geometry ratio, width/height
    /// fallback, or 4:3).
    pub fn aspect_ratio(&self) -> f32 {
        self.aspect_ratio
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

fn core_supports_extension(valid_extensions: &str, rom_ext: &str) -> bool {
    let rom_ext = rom_ext.trim().trim_start_matches('.').to_ascii_lowercase();
    if rom_ext.is_empty() {
        return true;
    }

    let valid = valid_extensions.trim();
    if valid.is_empty() || valid == "*" {
        return true;
    }

    valid
        .split(|ch: char| ch == '|' || ch == ',' || ch == ';' || ch.is_whitespace())
        .filter(|part| !part.is_empty())
        .any(|part| part.eq_ignore_ascii_case(&rom_ext))
}

#[cfg(test)]
mod resampler_tests {
    use super::*;

    fn frame_source(frames: &[[i16; 2]]) -> impl FnMut() -> Option<[i16; 2]> + '_ {
        let mut index = 0usize;
        move || {
            let frame = frames.get(index).copied();
            index += 1;
            frame
        }
    }

    #[test]
    fn passthrough_when_rates_match() {
        let frames: Vec<[i16; 2]> = (0..100)
            .map(|i| [(i * 100) as i16, -(i * 100) as i16])
            .collect();
        let mut resampler = LinearResampler::new(48_000.0, 48_000.0);
        let mut out = vec![0i16; 200];
        let written = resampler.process(&mut out, 2, frame_source(&frames));

        // A linear interpolator always consumes the "next" source frame, so
        // N input frames yield N-1 outputs at a 1:1 rate.
        assert_eq!(written, 99);
        assert_eq!(&out[..6], &[0, 0, 100, -100, 200, -200]);
    }

    #[test]
    fn upsampling_produces_proportional_length_and_preserves_ramp() {
        // A monotonic ramp at ~32 kHz upsampled to 48 kHz.
        let frames: Vec<[i16; 2]> = (0..1000).map(|i| [(i * 30) as i16, 0]).collect();
        let mut resampler = LinearResampler::new(32_040.0, 48_000.0);
        let expected_max = ((1000f64) * 48_000.0 / 32_040.0) as usize;
        let mut out = vec![0i16; expected_max * 2];
        let written = resampler.process(&mut out, 2, frame_source(&frames));

        // Output length should be close to the ideal ratio (~1497 frames).
        assert!(
            (1490..=1505).contains(&written),
            "unexpected output length {written}"
        );
        let left: Vec<i16> = out.chunks(2).take(written).map(|c| c[0]).collect();
        assert!(
            left.windows(2).all(|w| w[0] <= w[1]),
            "ramp must stay monotonic"
        );
        let last = left[written - 1];
        assert!(
            (last as i32 - 999 * 30).abs() <= 60,
            "endpoint drifted: {last}"
        );
    }

    #[test]
    fn downsampling_shrinks_length() {
        let frames: Vec<[i16; 2]> = (0..1000).map(|i| [i as i16, 0]).collect();
        let mut resampler = LinearResampler::new(48_000.0, 24_000.0);
        let mut out = vec![0i16; 4000];
        let written = resampler.process(&mut out, 2, frame_source(&frames));
        assert!(
            (490..=510).contains(&written),
            "unexpected output length {written}"
        );
    }

    #[test]
    fn mono_output_downmixes_to_average() {
        let frames: Vec<[i16; 2]> = vec![[100, 200]; 20];
        let mut resampler = LinearResampler::new(48_000.0, 48_000.0);
        let mut out = vec![0i16; 10];
        let written = resampler.process(&mut out, 1, frame_source(&frames));
        assert_eq!(written, 10);
        assert!(out.iter().all(|&s| s == 150), "got {out:?}");
    }

    #[test]
    fn underrun_writes_silence_and_resumes_at_same_position() {
        let first: Vec<[i16; 2]> = (0..10).map(|i| [i as i16, 0]).collect();
        let mut resampler = LinearResampler::new(48_000.0, 48_000.0);

        let mut out = vec![0i16; 80];
        // Positions 0..=8 are producible (each needs the following frame).
        let written = resampler.process(&mut out, 2, frame_source(&first));
        assert_eq!(written, 9);
        assert!(
            out[18..].iter().all(|&s| s == 0),
            "underrun region must be silent"
        );

        // Continue with a source whose frame 0 is the original frame 10: no
        // time should have been skipped during the underrun.
        let continuation: Vec<[i16; 2]> = (10..60).map(|i| [i as i16, 0]).collect();
        let mut resumed = vec![0i16; 10];
        let resumed_written = resampler.process(&mut resumed, 2, frame_source(&continuation));
        assert_eq!(resumed_written, 5);
        // Position 9.0 maps exactly onto source sample 9: playback resumes
        // precisely where it stopped instead of skipping time.
        assert_eq!(resumed[0], 9);
    }
}
