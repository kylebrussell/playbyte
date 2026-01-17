use libloading::Library;
use once_cell::sync::OnceCell;
use std::{
    ffi::{CStr, CString},
    os::raw::{c_char, c_void},
    path::Path,
    ptr,
    sync::{Arc, Mutex},
};
use thiserror::Error;

const RETRO_API_VERSION: u32 = 1;
const RETRO_ENVIRONMENT_GET_CAN_DUPE: u32 = 3;
const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: u32 = 10;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetroPixelFormat {
    Xrgb8888 = 0,
    Rgb565 = 1,
    _0rgb1555 = 2,
}

#[repr(C)]
struct RetroSystemInfo {
    library_name: *const c_char,
    library_version: *const c_char,
    valid_extensions: *const c_char,
    need_fullpath: bool,
    block_extract: bool,
}

#[repr(C)]
struct RetroGameInfo {
    path: *const c_char,
    data: *const c_void,
    size: usize,
    meta: *const c_char,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RetroGameGeometry {
    pub base_width: u32,
    pub base_height: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub aspect_ratio: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RetroSystemTiming {
    pub fps: f64,
    pub sample_rate: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RetroSystemAvInfo {
    geometry: RetroGameGeometry,
    timing: RetroSystemTiming,
}

#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub library_name: String,
    pub library_version: String,
    pub valid_extensions: String,
    pub need_fullpath: bool,
    pub block_extract: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SystemAvInfo {
    pub geometry: RetroGameGeometry,
    pub timing: RetroSystemTiming,
}

#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub pitch: usize,
    pub pixel_format: RetroPixelFormat,
    pub data: Vec<u8>,
}

type RetroEnvironmentFn = unsafe extern "C" fn(cmd: u32, data: *mut c_void) -> bool;
type RetroVideoRefreshFn =
    unsafe extern "C" fn(data: *const c_void, width: u32, height: u32, pitch: usize);
type RetroAudioSampleFn = unsafe extern "C" fn(left: i16, right: i16);
type RetroAudioSampleBatchFn = unsafe extern "C" fn(data: *const i16, frames: usize) -> usize;
type RetroInputPollFn = unsafe extern "C" fn();
type RetroInputStateFn = unsafe extern "C" fn(port: u32, device: u32, index: u32, id: u32) -> i16;

type RetroInitFn = unsafe extern "C" fn();
type RetroDeinitFn = unsafe extern "C" fn();
type RetroApiVersionFn = unsafe extern "C" fn() -> u32;
type RetroGetSystemInfoFn = unsafe extern "C" fn(info: *mut RetroSystemInfo);
type RetroGetSystemAvInfoFn = unsafe extern "C" fn(info: *mut RetroSystemAvInfo);
type RetroSetEnvironmentFn = unsafe extern "C" fn(cb: RetroEnvironmentFn);
type RetroSetVideoRefreshFn = unsafe extern "C" fn(cb: RetroVideoRefreshFn);
type RetroSetAudioSampleFn = unsafe extern "C" fn(cb: RetroAudioSampleFn);
type RetroSetAudioSampleBatchFn = unsafe extern "C" fn(cb: RetroAudioSampleBatchFn);
type RetroSetInputPollFn = unsafe extern "C" fn(cb: RetroInputPollFn);
type RetroSetInputStateFn = unsafe extern "C" fn(cb: RetroInputStateFn);
type RetroLoadGameFn = unsafe extern "C" fn(game: *const RetroGameInfo) -> bool;
type RetroUnloadGameFn = unsafe extern "C" fn();
type RetroRunFn = unsafe extern "C" fn();
type RetroSerializeSizeFn = unsafe extern "C" fn() -> usize;
type RetroSerializeFn = unsafe extern "C" fn(data: *mut c_void, size: usize) -> bool;
type RetroUnserializeFn = unsafe extern "C" fn(data: *const c_void, size: usize) -> bool;

struct Symbols {
    retro_init: RetroInitFn,
    retro_deinit: RetroDeinitFn,
    retro_api_version: RetroApiVersionFn,
    retro_get_system_info: RetroGetSystemInfoFn,
    retro_get_system_av_info: RetroGetSystemAvInfoFn,
    retro_set_environment: RetroSetEnvironmentFn,
    retro_set_video_refresh: RetroSetVideoRefreshFn,
    retro_set_audio_sample: RetroSetAudioSampleFn,
    retro_set_audio_sample_batch: RetroSetAudioSampleBatchFn,
    retro_set_input_poll: RetroSetInputPollFn,
    retro_set_input_state: RetroSetInputStateFn,
    retro_load_game: RetroLoadGameFn,
    retro_unload_game: RetroUnloadGameFn,
    retro_run: RetroRunFn,
    retro_serialize_size: RetroSerializeSizeFn,
    retro_serialize: RetroSerializeFn,
    retro_unserialize: RetroUnserializeFn,
}

impl Symbols {
    unsafe fn load(lib: &Library) -> Result<Self, LibretroError> {
        Ok(Self {
            retro_init: *lib.get(b"retro_init\0")?,
            retro_deinit: *lib.get(b"retro_deinit\0")?,
            retro_api_version: *lib.get(b"retro_api_version\0")?,
            retro_get_system_info: *lib.get(b"retro_get_system_info\0")?,
            retro_get_system_av_info: *lib.get(b"retro_get_system_av_info\0")?,
            retro_set_environment: *lib.get(b"retro_set_environment\0")?,
            retro_set_video_refresh: *lib.get(b"retro_set_video_refresh\0")?,
            retro_set_audio_sample: *lib.get(b"retro_set_audio_sample\0")?,
            retro_set_audio_sample_batch: *lib.get(b"retro_set_audio_sample_batch\0")?,
            retro_set_input_poll: *lib.get(b"retro_set_input_poll\0")?,
            retro_set_input_state: *lib.get(b"retro_set_input_state\0")?,
            retro_load_game: *lib.get(b"retro_load_game\0")?,
            retro_unload_game: *lib.get(b"retro_unload_game\0")?,
            retro_run: *lib.get(b"retro_run\0")?,
            retro_serialize_size: *lib.get(b"retro_serialize_size\0")?,
            retro_serialize: *lib.get(b"retro_serialize\0")?,
            retro_unserialize: *lib.get(b"retro_unserialize\0")?,
        })
    }
}

pub struct Callbacks {
    pub video_refresh: Box<dyn Fn(&[u8], u32, u32, usize, RetroPixelFormat) + Send + Sync>,
    pub audio_sample_batch: Box<dyn Fn(&[i16]) + Send + Sync>,
    pub input_poll: Box<dyn Fn() + Send + Sync>,
    pub input_state: Box<dyn Fn(u32, u32, u32, u32) -> i16 + Send + Sync>,
    pixel_format: Mutex<RetroPixelFormat>,
}

impl Callbacks {
    pub fn new(
        video_refresh: Box<dyn Fn(&[u8], u32, u32, usize, RetroPixelFormat) + Send + Sync>,
        audio_sample_batch: Box<dyn Fn(&[i16]) + Send + Sync>,
        input_poll: Box<dyn Fn() + Send + Sync>,
        input_state: Box<dyn Fn(u32, u32, u32, u32) -> i16 + Send + Sync>,
    ) -> Self {
        Self {
            video_refresh,
            audio_sample_batch,
            input_poll,
            input_state,
            pixel_format: Mutex::new(RetroPixelFormat::Xrgb8888),
        }
    }

    fn set_pixel_format(&self, format: RetroPixelFormat) {
        if let Ok(mut guard) = self.pixel_format.lock() {
            *guard = format;
        }
    }

    fn pixel_format(&self) -> RetroPixelFormat {
        self.pixel_format
            .lock()
            .map(|guard| *guard)
            .unwrap_or(RetroPixelFormat::Xrgb8888)
    }
}

#[derive(Error, Debug)]
pub enum LibretroError {
    #[error("failed to load library: {0}")]
    LoadLibrary(#[from] libloading::Error),
    #[error("libretro API version mismatch: expected {expected}, got {actual}")]
    ApiVersion { expected: u32, actual: u32 },
    #[error("missing libretro symbol: {0}")]
    MissingSymbol(String),
    #[error("libretro core failed to load game")]
    LoadGame,
    #[error("libretro core failed to serialize state")]
    Serialize,
    #[error("libretro core failed to unserialize state")]
    Unserialize,
    #[error("no frame captured during smoke test")]
    NoFrame,
    #[error("invalid utf-8 in core metadata")]
    Utf8(#[from] std::str::Utf8Error),
}

static CALLBACKS: OnceCell<Mutex<Option<Arc<Callbacks>>>> = OnceCell::new();

fn callbacks_cell() -> &'static Mutex<Option<Arc<Callbacks>>> {
    CALLBACKS.get_or_init(|| Mutex::new(None))
}

fn with_callbacks<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&Callbacks) -> R,
{
    let callbacks = callbacks_cell().lock().ok()?.clone();
    callbacks.as_deref().map(f)
}

unsafe extern "C" fn environment_callback(cmd: u32, data: *mut c_void) -> bool {
    match cmd {
        RETRO_ENVIRONMENT_GET_CAN_DUPE => {
            if !data.is_null() {
                *(data as *mut bool) = true;
                return true;
            }
            false
        }
        RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => {
            if data.is_null() {
                return false;
            }
            let format = *(data as *const RetroPixelFormat);
            let supported = matches!(
                format,
                RetroPixelFormat::Xrgb8888 | RetroPixelFormat::Rgb565
            );
            if supported {
                let _ = with_callbacks(|callbacks| callbacks.set_pixel_format(format));
            }
            supported
        }
        _ => false,
    }
}

unsafe extern "C" fn video_refresh_callback(
    data: *const c_void,
    width: u32,
    height: u32,
    pitch: usize,
) {
    if data.is_null() || width == 0 || height == 0 {
        return;
    }
    let byte_len = pitch.saturating_mul(height as usize);
    let slice = std::slice::from_raw_parts(data as *const u8, byte_len);
    let _ = with_callbacks(|callbacks| {
        let format = callbacks.pixel_format();
        (callbacks.video_refresh)(slice, width, height, pitch, format);
    });
}

unsafe extern "C" fn audio_sample_callback(left: i16, right: i16) {
    let samples = [left, right];
    let _ = with_callbacks(|callbacks| {
        (callbacks.audio_sample_batch)(&samples);
    });
}

unsafe extern "C" fn audio_sample_batch_callback(data: *const i16, frames: usize) -> usize {
    if data.is_null() || frames == 0 {
        return 0;
    }
    let slice = std::slice::from_raw_parts(data, frames * 2);
    let _ = with_callbacks(|callbacks| {
        (callbacks.audio_sample_batch)(slice);
    });
    frames
}

unsafe extern "C" fn input_poll_callback() {
    let _ = with_callbacks(|callbacks| {
        (callbacks.input_poll)();
    });
}

unsafe extern "C" fn input_state_callback(port: u32, device: u32, index: u32, id: u32) -> i16 {
    with_callbacks(|callbacks| (callbacks.input_state)(port, device, index, id)).unwrap_or(0)
}

struct LoadedGame {
    _path: CString,
    _data: Vec<u8>,
}

pub struct LibretroCore {
    _lib: Library,
    symbols: Symbols,
    system_info: SystemInfo,
    system_av_info: SystemAvInfo,
    callbacks: Arc<Callbacks>,
    game_loaded: bool,
    loaded_game: Option<LoadedGame>,
}

impl LibretroCore {
    pub fn load(path: impl AsRef<Path>, callbacks: Callbacks) -> Result<Self, LibretroError> {
        let lib = unsafe { Library::new(path.as_ref())? };
        let symbols = unsafe { Symbols::load(&lib)? };

        let api_version = unsafe { (symbols.retro_api_version)() };
        if api_version != RETRO_API_VERSION {
            return Err(LibretroError::ApiVersion {
                expected: RETRO_API_VERSION,
                actual: api_version,
            });
        }

        let callbacks = Arc::new(callbacks);
        if let Ok(mut guard) = callbacks_cell().lock() {
            *guard = Some(callbacks.clone());
        }

        unsafe {
            (symbols.retro_set_environment)(environment_callback);
            (symbols.retro_set_video_refresh)(video_refresh_callback);
            (symbols.retro_set_audio_sample)(audio_sample_callback);
            (symbols.retro_set_audio_sample_batch)(audio_sample_batch_callback);
            (symbols.retro_set_input_poll)(input_poll_callback);
            (symbols.retro_set_input_state)(input_state_callback);
            (symbols.retro_init)();
        }

        let system_info = unsafe { Self::read_system_info(&symbols)? };
        let system_av_info = unsafe { Self::read_system_av_info(&symbols) };

        Ok(Self {
            _lib: lib,
            symbols,
            system_info,
            system_av_info,
            callbacks,
            game_loaded: false,
            loaded_game: None,
        })
    }

    pub fn system_info(&self) -> &SystemInfo {
        &self.system_info
    }

    pub fn system_av_info(&self) -> SystemAvInfo {
        self.system_av_info
    }

    pub fn pixel_format(&self) -> RetroPixelFormat {
        self.callbacks.pixel_format()
    }

    pub fn load_game(&mut self, path: impl AsRef<Path>) -> Result<(), LibretroError> {
        let data = std::fs::read(&path).map_err(|_| LibretroError::LoadGame)?;
        let c_path = CString::new(path.as_ref().to_string_lossy().as_bytes())
            .map_err(|_| LibretroError::LoadGame)?;
        let game = RetroGameInfo {
            path: c_path.as_ptr(),
            data: data.as_ptr() as *const c_void,
            size: data.len(),
            meta: ptr::null(),
        };

        let loaded_game = LoadedGame {
            _path: c_path,
            _data: data,
        };
        let ok = unsafe { (self.symbols.retro_load_game)(&game) };
        if !ok {
            return Err(LibretroError::LoadGame);
        }
        self.game_loaded = true;
        self.loaded_game = Some(loaded_game);
        Ok(())
    }

    pub fn unload_game(&mut self) {
        if self.game_loaded {
            unsafe { (self.symbols.retro_unload_game)() };
            self.game_loaded = false;
            self.loaded_game = None;
        }
    }

    pub fn run_frame(&mut self) {
        unsafe { (self.symbols.retro_run)() };
    }

    pub fn run_frames(&mut self, frames: usize) {
        for _ in 0..frames {
            self.run_frame();
        }
    }

    pub fn serialize_size(&self) -> usize {
        unsafe { (self.symbols.retro_serialize_size)() }
    }

    pub fn serialize(&self) -> Result<Vec<u8>, LibretroError> {
        let size = self.serialize_size();
        let mut buffer = vec![0u8; size];
        let ok =
            unsafe { (self.symbols.retro_serialize)(buffer.as_mut_ptr() as *mut c_void, size) };
        if ok {
            Ok(buffer)
        } else {
            Err(LibretroError::Serialize)
        }
    }

    pub fn unserialize(&self, data: &[u8]) -> Result<(), LibretroError> {
        let ok =
            unsafe { (self.symbols.retro_unserialize)(data.as_ptr() as *const c_void, data.len()) };
        if ok {
            Ok(())
        } else {
            Err(LibretroError::Unserialize)
        }
    }

    unsafe fn read_system_info(symbols: &Symbols) -> Result<SystemInfo, LibretroError> {
        let mut info = RetroSystemInfo {
            library_name: ptr::null(),
            library_version: ptr::null(),
            valid_extensions: ptr::null(),
            need_fullpath: false,
            block_extract: false,
        };
        (symbols.retro_get_system_info)(&mut info);
        let library_name = CStr::from_ptr(info.library_name).to_str()?.to_string();
        let library_version = CStr::from_ptr(info.library_version).to_str()?.to_string();
        let valid_extensions = CStr::from_ptr(info.valid_extensions).to_str()?.to_string();

        Ok(SystemInfo {
            library_name,
            library_version,
            valid_extensions,
            need_fullpath: info.need_fullpath,
            block_extract: info.block_extract,
        })
    }

    unsafe fn read_system_av_info(symbols: &Symbols) -> SystemAvInfo {
        let mut info = RetroSystemAvInfo {
            geometry: RetroGameGeometry {
                base_width: 0,
                base_height: 0,
                max_width: 0,
                max_height: 0,
                aspect_ratio: 0.0,
            },
            timing: RetroSystemTiming {
                fps: 0.0,
                sample_rate: 0.0,
            },
        };
        (symbols.retro_get_system_av_info)(&mut info);
        SystemAvInfo {
            geometry: info.geometry,
            timing: info.timing,
        }
    }
}

impl Drop for LibretroCore {
    fn drop(&mut self) {
        self.unload_game();
        unsafe { (self.symbols.retro_deinit)() };
    }
}

pub fn smoke_test(
    core_path: impl AsRef<Path>,
    rom_path: impl AsRef<Path>,
    frames: usize,
) -> Result<VideoFrame, LibretroError> {
    let last_frame: Arc<Mutex<Option<VideoFrame>>> = Arc::new(Mutex::new(None));
    let capture = last_frame.clone();
    let callbacks = Callbacks::new(
        Box::new(move |data, width, height, pitch, format| {
            let mut guard = capture.lock().expect("frame lock poisoned");
            *guard = Some(VideoFrame {
                width,
                height,
                pitch,
                pixel_format: format,
                data: data.to_vec(),
            });
        }),
        Box::new(|_samples| {}),
        Box::new(|| {}),
        Box::new(|_, _, _, _| 0),
    );

    let mut core = LibretroCore::load(core_path, callbacks)?;
    core.load_game(rom_path)?;
    core.run_frames(frames.max(1));

    let frame = last_frame
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .ok_or(LibretroError::NoFrame)?;
    Ok(frame)
}
