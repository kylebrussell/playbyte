mod dualsense;
mod input;
mod ui;

use anyhow::{bail, Context, Result};
use bytemuck::{Pod, Zeroable};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use egui_wgpu::ScreenDescriptor;
use gilrs::{Axis, Button, EventType, Gilrs};
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder};
use playbyte_emulation::{
    AudioRingBuffer, EmulatorRuntime, JoypadState, RETRO_DEVICE_ID_JOYPAD_A,
    RETRO_DEVICE_ID_JOYPAD_B, RETRO_DEVICE_ID_JOYPAD_DOWN, RETRO_DEVICE_ID_JOYPAD_L,
    RETRO_DEVICE_ID_JOYPAD_LEFT, RETRO_DEVICE_ID_JOYPAD_R, RETRO_DEVICE_ID_JOYPAD_RIGHT,
    RETRO_DEVICE_ID_JOYPAD_SELECT, RETRO_DEVICE_ID_JOYPAD_START, RETRO_DEVICE_ID_JOYPAD_UP,
    RETRO_DEVICE_ID_JOYPAD_X, RETRO_DEVICE_ID_JOYPAD_Y,
};
use playbyte_feed::{LocalByteStore, RomLibrary};
use playbyte_types::{ByteMetadata, System};
use sha1::{Digest, Sha1};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use wgpu::util::DeviceExt;
use winit::{
    event::{ElementState, Event, WindowEvent},
    event_loop::EventLoopBuilder,
    keyboard::{KeyCode, PhysicalKey},
    window::WindowBuilder,
};

use crate::input::{Action, UserEvent};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    uv: [f32; 2],
}

impl Vertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

const VERTICES: &[Vertex] = &[
    Vertex {
        position: [-1.0, -1.0, 0.0],
        uv: [0.0, 1.0],
    },
    Vertex {
        position: [1.0, -1.0, 0.0],
        uv: [1.0, 1.0],
    },
    Vertex {
        position: [1.0, 1.0, 0.0],
        uv: [1.0, 0.0],
    },
    Vertex {
        position: [-1.0, 1.0, 0.0],
        uv: [0.0, 0.0],
    },
];

const INDICES: &[u16] = &[0, 1, 2, 0, 2, 3];

const SDL_GAMECONTROLLERCONFIG: &str = "SDL_GAMECONTROLLERCONFIG";
const DUALSENSE_USB_MAPPING: &str = "050000004c050000e60c000000010000,PS5 Controller,a:b1,b:b2,back:b8,dpdown:h0.4,dpleft:h0.8,dpright:h0.2,dpup:h0.1,guide:b12,leftshoulder:b4,leftstick:b10,lefttrigger:a3,leftx:a0,lefty:a1,misc1:b14,rightshoulder:b5,rightstick:b11,righttrigger:a4,rightx:a2,righty:a5,start:b9,touchpad:b13,x:b0,y:b3,platform:Mac OS X,";
const DUALSENSE_BLUETOOTH_MAPPING: &str = "050000004c050000f20d000000010000,PS5 Controller,a:b1,b:b2,back:b8,dpdown:h0.4,dpleft:h0.8,dpright:h0.2,dpup:h0.1,guide:b12,leftshoulder:b4,leftstick:b10,lefttrigger:a3,leftx:a0,lefty:a1,rightshoulder:b5,rightstick:b11,righttrigger:a4,rightx:a2,righty:a5,start:b9,touchpad:b13,x:b0,y:b3,platform:Mac OS X,";
const GAMEPAD_AXIS_THRESHOLD: f32 = 0.5;

fn configure_dualsense_mappings() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let existing = std::env::var(SDL_GAMECONTROLLERCONFIG).unwrap_or_default();
    let mut entries = Vec::new();
    if !existing.contains("050000004c050000e60c000000010000") {
        entries.push(DUALSENSE_USB_MAPPING);
    }
    if !existing.contains("050000004c050000f20d000000010000") {
        entries.push(DUALSENSE_BLUETOOTH_MAPPING);
    }
    if entries.is_empty() {
        return;
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&entries.join("\n"));
    std::env::set_var(SDL_GAMECONTROLLERCONFIG, updated);
}

fn axis_to_dpad_buttons(axis: Axis, value: f32) -> Option<[(Button, bool); 2]> {
    let direction = if value <= -GAMEPAD_AXIS_THRESHOLD {
        -1
    } else if value >= GAMEPAD_AXIS_THRESHOLD {
        1
    } else {
        0
    };
    match axis {
        Axis::DPadX => Some(match direction {
            -1 => [(Button::DPadLeft, true), (Button::DPadRight, false)],
            1 => [(Button::DPadLeft, false), (Button::DPadRight, true)],
            _ => [(Button::DPadLeft, false), (Button::DPadRight, false)],
        }),
        Axis::DPadY => Some(match direction {
            -1 => [(Button::DPadUp, true), (Button::DPadDown, false)],
            1 => [(Button::DPadUp, false), (Button::DPadDown, true)],
            _ => [(Button::DPadUp, false), (Button::DPadDown, false)],
        }),
        _ => None,
    }
}

struct FrameStats {
    samples: VecDeque<f64>,
    max_samples: usize,
}

impl FrameStats {
    fn new(max_samples: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(max_samples),
            max_samples,
        }
    }

    fn record(&mut self, dt: Duration) {
        let secs = dt.as_secs_f64();
        if secs <= 0.0 {
            return;
        }
        if self.samples.len() == self.max_samples {
            self.samples.pop_front();
        }
        self.samples.push_back(secs);
    }

    fn avg_fps(&self) -> Option<f64> {
        if self.samples.is_empty() {
            return None;
        }
        let sum: f64 = self.samples.iter().sum();
        let avg = sum / self.samples.len() as f64;
        if avg > 0.0 {
            Some(1.0 / avg)
        } else {
            None
        }
    }
}

struct AppConfig {
    core_path: Option<PathBuf>,
    rom_path: Option<PathBuf>,
    data_root: PathBuf,
    rom_root: PathBuf,
    cores_root: PathBuf,
    vsync: bool,
    dualsense_swipes: bool,
}

impl AppConfig {
    fn from_env() -> Self {
        let mut core_path = None;
        let mut rom_path = None;
        let mut data_root = PathBuf::from("./data");
        let mut rom_root = PathBuf::from("./roms");
        let mut cores_root = PathBuf::from("./cores");
        let mut data_root_overridden = false;
        let mut rom_root_overridden = false;
        let mut cores_root_overridden = false;
        let mut vsync = true;
        let mut dualsense_swipes = true;
        let mut args = std::env::args().skip(1);

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--core" => {
                    core_path = args.next().map(PathBuf::from);
                }
                "--rom" => {
                    rom_path = args.next().map(PathBuf::from);
                }
                "--data" => {
                    if let Some(path) = args.next() {
                        data_root = PathBuf::from(path);
                        data_root_overridden = true;
                    }
                }
                "--roms" => {
                    if let Some(path) = args.next() {
                        rom_root = PathBuf::from(path);
                        rom_root_overridden = true;
                    }
                }
                "--cores" => {
                    if let Some(path) = args.next() {
                        cores_root = PathBuf::from(path);
                        cores_root_overridden = true;
                    }
                }
                "--no-vsync" => {
                    vsync = false;
                }
                "--no-dualsense-swipes" => {
                    dualsense_swipes = false;
                }
                _ => {}
            }
        }

        if !data_root_overridden || !rom_root_overridden || !cores_root_overridden {
            if let Some(asset_root) = resolve_assets_root() {
                if !data_root_overridden {
                    data_root = asset_root.join("data");
                }
                if !rom_root_overridden {
                    rom_root = asset_root.join("roms");
                }
                if !cores_root_overridden {
                    cores_root = asset_root.join("cores");
                }
            } else if !data_root_overridden {
                if let Some(default_root) = default_data_root() {
                    data_root = default_root;
                }
            }
        }

        Self {
            core_path,
            rom_path,
            data_root,
            rom_root,
            cores_root,
            vsync,
            dualsense_swipes,
        }
    }
}

fn resolve_assets_root() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.to_path_buf());
        }
    }

    let mut expanded = candidates.clone();
    for candidate in &candidates {
        if let Some(root) = find_repo_root(candidate) {
            if !expanded.iter().any(|existing| existing == &root) {
                expanded.push(root);
            }
        }
    }

    for candidate in expanded {
        if has_asset_dirs(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn find_repo_root(start: &PathBuf) -> Option<PathBuf> {
    let mut current = start.clone();
    loop {
        if current.join(".git").exists() || current.join("Cargo.toml").exists() {
            return Some(current);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

fn has_asset_dirs(root: &PathBuf) -> bool {
    root.join("data").exists() || root.join("roms").exists() || root.join("cores").exists()
}

fn default_data_root() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME")?;
        let mut root = PathBuf::from(home);
        root.push("Library");
        root.push("Application Support");
        root.push("Playbyte");
        Some(root)
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(|appdata| PathBuf::from(appdata).join("Playbyte"))
    } else {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            Some(PathBuf::from(xdg).join("playbyte"))
        } else if let Some(home) = std::env::var_os("HOME") {
            Some(PathBuf::from(home).join(".local").join("share").join("playbyte"))
        } else {
            None
        }
    }
}

struct CoreLocator {
    root: PathBuf,
}

impl CoreLocator {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn resolve(&self, core_id: &str) -> Option<PathBuf> {
        let direct = self.root.join(core_id);
        if direct.exists() {
            return Some(direct);
        }
        let ext = if cfg!(target_os = "windows") {
            "dll"
        } else if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        };
        let filename = format!("{core_id}_libretro.{ext}");
        let candidate = self.root.join(filename);
        if candidate.exists() {
            Some(candidate)
        } else {
            let by_ext = self.root.join(format!("{core_id}.{ext}"));
            if by_ext.exists() {
                Some(by_ext)
            } else {
                None
            }
        }
    }
}

#[derive(Clone)]
struct RuntimeMetadata {
    core_id: String,
    core_version: String,
    rom_sha1: String,
    _rom_path: PathBuf,
    system: System,
}

struct RuntimeLoad {
    runtime: EmulatorRuntime,
    meta: RuntimeMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SessionAutosaveKey {
    Byte(String),
    Rom(String),
}

#[derive(Clone)]
enum FeedItem {
    Byte(ByteMetadata),
    RomFallback(RomFallback),
}

#[derive(Clone)]
struct RomFallback {
    rom_sha1: String,
    rom_path: PathBuf,
    system: System,
    title: String,
    official_title: Option<String>,
    core_id: String,
    core_path: Option<PathBuf>,
}

impl FeedItem {
    fn system(&self) -> System {
        match self {
            FeedItem::Byte(byte) => byte.system.clone(),
            FeedItem::RomFallback(fallback) => fallback.system.clone(),
        }
    }

    fn title(&self) -> &str {
        match self {
            FeedItem::Byte(byte) => {
                if byte.title.is_empty() {
                    &byte.byte_id
                } else {
                    &byte.title
                }
            }
            FeedItem::RomFallback(fallback) => &fallback.title,
        }
    }

    fn session_autosave_key(&self) -> SessionAutosaveKey {
        match self {
            FeedItem::Byte(byte) => SessionAutosaveKey::Byte(byte.byte_id.clone()),
            FeedItem::RomFallback(fallback) => SessionAutosaveKey::Rom(fallback.rom_sha1.clone()),
        }
    }

}

struct FeedController {
    store: LocalByteStore,
    roms: RomLibrary,
    core_locator: CoreLocator,
    items: Vec<FeedItem>,
    current_index: usize,
}

impl FeedController {
    fn load(config: &AppConfig) -> Result<Self> {
        let store = LocalByteStore::new(&config.data_root);
        let bytes = store.load_index()?;
        let rom_titles = store.load_rom_titles()?;
        let rom_overrides = store.load_rom_official_overrides()?;

        let mut roms = RomLibrary::new();
        roms.add_root(&config.rom_root);
        let _ = roms.scan()?;

        let core_locator = CoreLocator::new(config.cores_root.clone());
        let items =
            build_feed_items(&store, &core_locator, &roms, &bytes, &rom_titles, &rom_overrides)?;

        Ok(Self {
            store,
            roms,
            core_locator,
            items,
            current_index: 0,
        })
    }

    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn current(&self) -> Option<&FeedItem> {
        self.items.get(self.current_index)
    }

    fn select(&mut self, index: usize) -> Option<&FeedItem> {
        if index < self.items.len() {
            self.current_index = index;
        }
        self.current()
    }

    fn next(&mut self) -> Option<&FeedItem> {
        if self.items.is_empty() {
            return None;
        }
        self.current_index = (self.current_index + 1).min(self.items.len() - 1);
        self.current()
    }

    fn prev(&mut self) -> Option<&FeedItem> {
        if self.items.is_empty() {
            return None;
        }
        if self.current_index > 0 {
            self.current_index -= 1;
        }
        self.current()
    }

    fn prefetch_neighbors(&self) {
        if self.items.is_empty() {
            return;
        }
        let mut ids = Vec::new();
        if let Some(FeedItem::Byte(current)) = self.current() {
            ids.push(current.byte_id.clone());
        }
        if self.current_index > 0 {
            if let FeedItem::Byte(byte) = &self.items[self.current_index - 1] {
                ids.push(byte.byte_id.clone());
            }
        }
        if self.current_index + 1 < self.items.len() {
            if let FeedItem::Byte(byte) = &self.items[self.current_index + 1] {
                ids.push(byte.byte_id.clone());
            }
        }
        self.store.prefetch(&ids);
    }

    fn build_runtime_for_current(&self) -> Result<RuntimeLoad> {
        let item = self.current().context("no selected item available in feed")?;
        match item {
            FeedItem::Byte(byte) => {
                let core_path = self
                    .core_locator
                    .resolve(&byte.core_id)
                    .with_context(|| format!("missing core for {}", byte.core_id))?;
                let rom_path = self
                    .roms
                    .find_by_hash(&byte.rom_sha1)
                    .with_context(|| format!("missing ROM for hash {}", byte.rom_sha1))?;

                let runtime = EmulatorRuntime::new(core_path, rom_path.clone())?;
                let info = runtime.system_info();
                if info.library_name != byte.core_id {
                    bail!(
                        "core mismatch: expected {}, found {}",
                        byte.core_id,
                        info.library_name
                    );
                }
                if info.library_version != byte.core_semver {
                    bail!(
                        "core version mismatch: expected {}, found {}",
                        byte.core_semver,
                        info.library_version
                    );
                }
                let state = self.store.load_state(&byte.byte_id)?;
                runtime.unserialize(&state)?;
                let meta = RuntimeMetadata {
                    core_id: byte.core_id.clone(),
                    core_version: byte.core_semver.clone(),
                    rom_sha1: byte.rom_sha1.clone(),
                    _rom_path: rom_path,
                    system: byte.system.clone(),
                };
                Ok(RuntimeLoad { runtime, meta })
            }
            FeedItem::RomFallback(fallback) => {
                let core_path = if let Some(path) = &fallback.core_path {
                    path.clone()
                } else {
                    self.core_locator
                        .resolve(&fallback.core_id)
                        .with_context(|| format!("missing core for {}", fallback.core_id))?
                };
                let runtime = EmulatorRuntime::new(core_path, fallback.rom_path.clone())?;
                let meta = build_runtime_meta_from_runtime(&runtime, &fallback.rom_path)?;
                Ok(RuntimeLoad { runtime, meta })
            }
        }
    }

    fn add_byte(&mut self, metadata: ByteMetadata) {
        let rom_sha1 = metadata.rom_sha1.clone();
        self.items.retain(|item| {
            !matches!(item, FeedItem::RomFallback(fallback) if fallback.rom_sha1 == rom_sha1)
        });
        self.items.push(FeedItem::Byte(metadata));
        self.current_index = self.items.len().saturating_sub(1);
    }

    fn add_fallback_rom(
        &mut self,
        rom_path: PathBuf,
        core_path: Option<PathBuf>,
    ) -> Result<()> {
        let rom_sha1 = hash_rom(&rom_path)?;
        if self.items.iter().any(|item| match item {
            FeedItem::Byte(byte) => byte.rom_sha1 == rom_sha1,
            FeedItem::RomFallback(fallback) => fallback.rom_sha1 == rom_sha1,
        }) {
            return Ok(());
        }
        let system = system_from_rom_path(&rom_path);
        let core_id = if let Some(path) = &core_path {
            core_id_from_path(path)
                .ok_or_else(|| anyhow::anyhow!("unable to infer core id from path"))?
        } else {
            let available_cores = list_core_ids(&self.core_locator.root);
            let bytes: Vec<ByteMetadata> = self
                .items
                .iter()
                .filter_map(|item| match item {
                    FeedItem::Byte(byte) => Some(byte.clone()),
                    FeedItem::RomFallback(_) => None,
                })
                .collect();
            select_default_core(system.clone(), &available_cores, &bytes, &self.core_locator)
                .ok_or_else(|| anyhow::anyhow!("no core available for fallback ROM"))?
        };
        let rom_titles = self.store.load_rom_titles()?;
        let rom_overrides = self.store.load_rom_official_overrides()?;
        let title = rom_titles
            .get(&rom_sha1)
            .cloned()
            .unwrap_or_else(|| title_from_rom_path(&rom_path));
        let official_title = resolve_official_title(
            &self.store,
            &rom_sha1,
            &rom_path,
            system.clone(),
            &title,
            &rom_overrides,
        );
        self.items.push(FeedItem::RomFallback(RomFallback {
            rom_sha1,
            rom_path,
            system,
            title,
            official_title,
            core_id,
            core_path,
        }));
        if self.items.len() == 1 {
            self.current_index = 0;
        }
        Ok(())
    }
}

fn build_feed_items(
    store: &LocalByteStore,
    core_locator: &CoreLocator,
    roms: &RomLibrary,
    bytes: &[ByteMetadata],
    rom_titles: &HashMap<String, String>,
    rom_overrides: &HashMap<String, String>,
) -> Result<Vec<FeedItem>> {
    let mut items: Vec<FeedItem> = bytes.iter().cloned().map(FeedItem::Byte).collect();
    let mut covered_roms: HashSet<String> = bytes.iter().map(|byte| byte.rom_sha1.clone()).collect();

    let available_cores = list_core_ids(&core_locator.root);
    let nes_core = select_default_core(System::Nes, &available_cores, bytes, core_locator);
    let snes_core = select_default_core(System::Snes, &available_cores, bytes, core_locator);
    let gbc_core = select_default_core(System::Gbc, &available_cores, bytes, core_locator);
    let gba_core = select_default_core(System::Gba, &available_cores, bytes, core_locator);

    let mut rom_entries = roms.entries();
    rom_entries.sort_by(|a, b| a.1.to_string_lossy().cmp(&b.1.to_string_lossy()));
    for (rom_sha1, rom_path) in rom_entries {
        if covered_roms.contains(&rom_sha1) {
            continue;
        }
        let system = system_from_rom_path(&rom_path);
        let core_id = match system {
            System::Nes => nes_core.clone(),
            System::Snes => snes_core.clone(),
            System::Gbc => gbc_core.clone(),
            System::Gba => gba_core.clone(),
        };
        let Some(core_id) = core_id else {
            continue;
        };
        let title = rom_titles
            .get(&rom_sha1)
            .cloned()
            .unwrap_or_else(|| title_from_rom_path(&rom_path));
        let official_title = resolve_official_title(
            store,
            &rom_sha1,
            &rom_path,
            system.clone(),
            &title,
            rom_overrides,
        );
        items.push(FeedItem::RomFallback(RomFallback {
            rom_sha1: rom_sha1.clone(),
            rom_path,
            system,
            title,
            official_title,
            core_id,
            core_path: None,
        }));
        covered_roms.insert(rom_sha1);
    }

    Ok(items)
}

fn title_from_rom_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.replace('_', " "))
        .unwrap_or_else(|| "Unknown ROM".to_string())
}

fn resolve_official_title(
    store: &LocalByteStore,
    rom_sha1: &str,
    rom_path: &PathBuf,
    system: System,
    display_title: &str,
    overrides: &HashMap<String, String>,
) -> Option<String> {
    if let Some(override_title) = overrides.get(rom_sha1) {
        return Some(override_title.clone());
    }

    let db = store.load_romdb(system).ok()?;
    if let Some(title) = db.title_for_sha1(rom_sha1) {
        return Some(title.to_string());
    }

    if system == System::Snes {
        if let Ok(Some(alt_sha1)) = hash_rom_without_snes_header(rom_path) {
            if let Some(title) = db.title_for_sha1(&alt_sha1) {
                return Some(title.to_string());
            }
        }
    }

    if let Some(title) = db.best_match(display_title) {
        return Some(title);
    }

    let fallback = title_from_rom_path(rom_path);
    if fallback != display_title {
        return db.best_match(&fallback);
    }

    None
}

fn core_id_from_path(path: &PathBuf) -> Option<String> {
    let filename = path.file_name()?.to_str()?;
    if let Some((core_id, _)) = filename.split_once("_libretro.") {
        if !core_id.is_empty() {
            return Some(core_id.to_string());
        }
    }
    path.file_stem().and_then(|stem| stem.to_str()).map(|stem| stem.to_string())
}

fn list_core_ids(root: &PathBuf) -> Vec<String> {
    let mut cores = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return cores;
    };
    let ext = if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    for entry in entries.flatten() {
        if let Ok(file_type) = entry.file_type() {
            if !(file_type.is_file() || file_type.is_symlink()) {
                continue;
            }
        }
        let path = entry.path();
        let matches_ext = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.eq_ignore_ascii_case(ext))
            .unwrap_or(false);
        if !matches_ext {
            continue;
        }
        if let Some(core_id) = core_id_from_path(&path) {
            cores.push(core_id);
        }
    }
    cores.sort();
    cores.dedup();
    cores
}

fn core_id_matches_preference(core_id: &str, needle: &str) -> bool {
    let core = core_id.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    if core == needle {
        return true;
    }
    // Avoid overly-broad matches like "nes" matching "bsnes".
    if needle.len() < 4 {
        return false;
    }
    core.contains(&needle)
}

fn select_default_core(
    system: System,
    available_cores: &[String],
    bytes: &[ByteMetadata],
    core_locator: &CoreLocator,
) -> Option<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for byte in bytes {
        if byte.system != system {
            continue;
        }
        if core_locator.resolve(&byte.core_id).is_none() {
            continue;
        }
        *counts.entry(byte.core_id.clone()).or_insert(0) += 1;
    }
    if let Some((core_id, _)) = counts.into_iter().max_by_key(|(_, count)| *count) {
        return Some(core_id);
    }

    let preferred = match system {
        System::Nes => &["mesen", "nestopia", "fceux", "nes"][..],
        System::Snes => &["bsnes", "snes9x", "snes"][..],
        System::Gbc => &["gambatte", "sameboy", "gearboy", "gb"][..],
        System::Gba => &["mgba", "gpsp", "vba", "gba"][..],
    };
    for needle in preferred {
        if let Some(core_id) = available_cores
            .iter()
            .find(|core| core_id_matches_preference(core, needle))
        {
            return Some(core_id.clone());
        }
    }

    None
}

struct VideoTexture {
    texture: wgpu::Texture,
    _view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

struct GuiState {
    ctx: egui::Context,
    winit: egui_winit::State,
    renderer: egui_wgpu::Renderer,
}

impl GuiState {
    fn new(
        window: &winit::window::Window,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> Self {
        let ctx = egui::Context::default();
        let winit = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        let renderer = egui_wgpu::Renderer::new(device, format, None, 1);
        Self {
            ctx,
            winit,
            renderer,
        }
    }

    fn handle_event(&mut self, window: &winit::window::Window, event: &Event<UserEvent>) {
        if let Event::WindowEvent { event, .. } = event {
            let _ = self.winit.on_window_event(window, event);
        }
    }
}

struct GuiOutput {
    paint_jobs: Vec<egui::ClippedPrimitive>,
    screen_desc: ScreenDescriptor,
    actions: Vec<Action>,
}

struct State {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: winit::dpi::PhysicalSize<u32>,
    render_pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    num_indices: u32,
    texture_layout: wgpu::BindGroupLayout,
    video_texture: VideoTexture,
    runtime: Option<EmulatorRuntime>,
    runtime_meta: Option<RuntimeMetadata>,
    session_autosaves: HashMap<SessionAutosaveKey, Vec<u8>>,
    gilrs: Option<Gilrs>,
    audio_stream: Option<cpal::Stream>,
    feed: Option<FeedController>,
    feed_error: Option<String>,
    gui: GuiState,
    ui: ui::UiState,
    dualsense_buttons_enabled: Arc<AtomicBool>,
    l2_held: bool,
    r2_held: bool,
    overlay_toggle_armed: bool,
    last_update: Instant,
    accumulator: f64,
    frame_stats: FrameStats,
    data_root: PathBuf,
}

impl State {
    async fn new(
        window: &winit::window::Window,
        app_config: AppConfig,
        dualsense_buttons_enabled: Arc<AtomicBool>,
    ) -> Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        let surface_target = unsafe { wgpu::SurfaceTargetUnsafe::from_window(window)? };
        let surface = unsafe { instance.create_surface_unsafe(surface_target)? };
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| anyhow::anyhow!("No compatible GPU adapters found"))?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("playbyte-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await?;

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
            .unwrap_or(surface_caps.formats[0]);
        let present_mode = if app_config.vsync {
            wgpu::PresentMode::Fifo
        } else {
            wgpu::PresentMode::Immediate
        };
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            desired_maximum_frame_latency: 2,
            present_mode,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &surface_config);

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video-texture-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let video_texture = Self::create_placeholder_texture(&device, &queue, &texture_layout);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("video-shader"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
) -> VertexOut {
    var out: VertexOut;
    out.position = vec4<f32>(position, 1.0);
    out.uv = uv;
    return out;
}

@group(0) @binding(0) var video_tex: texture_2d<f32>;
@group(0) @binding(1) var video_sampler: sampler;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(video_tex, video_sampler, in.uv);
}
"#
                .into(),
            ),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video-pipeline-layout"),
            bind_group_layouts: &[&texture_layout],
            push_constant_ranges: &[],
        });
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[Vertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("video-vertex-buffer"),
            contents: bytemuck::cast_slice(VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("video-index-buffer"),
            contents: bytemuck::cast_slice(INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });

    let mut feed = None;
    let mut feed_error = None;
    let mut runtime_load: Option<RuntimeLoad> = None;

    match FeedController::load(&app_config) {
        Ok(controller) => {
            if controller.is_empty() {
                feed_error = Some("No Bytes or ROMs found to build the feed.".to_string());
            }
            feed = Some(controller);
        }
        Err(err) => feed_error = Some(format!("Feed error: {err}")),
    }

    if let (Some(core), Some(rom)) = (app_config.core_path.clone(), app_config.rom_path.clone())
    {
        if let Some(controller) = feed.as_mut() {
            if let Err(err) = controller.add_fallback_rom(rom, Some(core)) {
                feed_error = Some(format!("Feed ROM error: {err}"));
            }
        } else {
            match EmulatorRuntime::new(core, rom.clone()) {
                Ok(rt) => match build_runtime_meta_from_runtime(&rt, &rom) {
                    Ok(meta) => runtime_load = Some(RuntimeLoad { runtime: rt, meta }),
                    Err(err) => feed_error = Some(format!("Runtime meta error: {err}")),
                },
                Err(err) => feed_error = Some(format!("Runtime error: {err}")),
            }
        }
    }

        if let Some(controller) = &feed {
            if !controller.is_empty() {
                match controller.build_runtime_for_current() {
                    Ok(load) => runtime_load = Some(load),
                    Err(err) => feed_error = Some(format!("Load feed item failed: {err}")),
                }
            }
        }

        if let Some(controller) = &feed {
            controller.prefetch_neighbors();
        }

        let (runtime, runtime_meta) = match runtime_load {
            Some(load) => (Some(load.runtime), Some(load.meta)),
            None => (None, None),
        };
        configure_dualsense_mappings();
        let (gilrs, gamepad_error) = match Gilrs::new() {
            Ok(gilrs) => (Some(gilrs), None),
            Err(err) => (None, Some(format!("Gamepad init failed: {err}"))),
        };
        let audio_stream = runtime
            .as_ref()
            .and_then(|rt| build_audio_stream(rt.audio_buffer()).ok());

        let gui = GuiState::new(window, &device, surface_config.format);
        let mut ui = ui::UiState::new(&gui.ctx);
        if let Some(message) = gamepad_error {
            ui.push_toast(ui::ToastKind::Error, message);
        }
        let detected_gamepads = gilrs
            .as_ref()
            .map(|gilrs| {
                gilrs
                    .gamepads()
                    .map(|(_, gamepad)| {
                        format!(
                            "Gamepad detected: {} ({:?})",
                            gamepad.name(),
                            gamepad.mapping_source()
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !detected_gamepads.is_empty() {
            dualsense_buttons_enabled.store(false, Ordering::Relaxed);
        }
        for message in detected_gamepads {
            ui.push_toast(ui::ToastKind::Success, message);
        }

        Ok(Self {
            surface,
            device,
            queue,
            config: surface_config,
            size,
            render_pipeline,
            vertex_buffer,
            index_buffer,
            num_indices: INDICES.len() as u32,
            texture_layout,
            video_texture,
            runtime,
            runtime_meta,
            session_autosaves: HashMap::new(),
            gilrs,
            audio_stream,
            feed,
            feed_error,
            gui,
            ui,
            dualsense_buttons_enabled,
            l2_held: false,
            r2_held: false,
            overlay_toggle_armed: true,
            last_update: Instant::now(),
            accumulator: 0.0,
            frame_stats: FrameStats::new(120),
            data_root: app_config.data_root,
        })
    }

    fn create_placeholder_texture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
    ) -> VideoTexture {
        let width = 2;
        let height = 2;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("placeholder-texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let pixels: [u8; 16] = [
            0x25, 0x25, 0x2d, 0xff, // dark gray
            0x3d, 0x7a, 0xf7, 0xff, // blue
            0x3d, 0x7a, 0xf7, 0xff, // blue
            0x25, 0x25, 0x2d, 0xff, // dark gray
        ];

        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        VideoTexture {
            texture,
            _view: view,
            bind_group,
            width,
            height,
        }
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.size = new_size;
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    fn update(&mut self, dt: Duration) {
        self.frame_stats.record(dt);
        let input_state = self.runtime.as_ref().map(|runtime| runtime.input_state());
        self.poll_gamepads(input_state);
        if let Some(runtime) = self.runtime.as_mut() {
            self.accumulator += dt.as_secs_f64();
            let frame_time = 1.0 / runtime.fps();
            let frame_time = frame_time.max(1.0 / 1000.0);
            self.accumulator = self.accumulator.min(frame_time * 5.0);
            while self.accumulator >= frame_time {
                runtime.run_frame();
                self.accumulator -= frame_time;
            }
            if let Some(frame) = runtime.latest_frame() {
                self.update_video_texture(&frame);
            }
        }
    }

    fn handle_gamepad_button(
        &mut self,
        button: Button,
        pressed: bool,
        input: Option<Arc<Mutex<JoypadState>>>,
    ) {
        if button == Button::LeftTrigger2 {
            self.l2_held = pressed;
        } else if button == Button::RightTrigger2 {
            self.r2_held = pressed;
        }

        if self.l2_held && self.r2_held {
            if self.overlay_toggle_armed {
                self.overlay_toggle_armed = false;
                self.apply_action(Action::ToggleOverlay);
            }
            return;
        }

        if !self.l2_held && !self.r2_held {
            self.overlay_toggle_armed = true;
        }

        let context = input::ButtonContext {
            overlay_visible: self.ui.is_overlay_visible(),
            official_picker_open: self.ui.is_official_picker_open(),
            is_editing_text: self.ui.is_editing_text(),
        };

        if pressed {
            if let Some(action) = input::action_from_button(button, true, context) {
                self.apply_action(action);
                return;
            }
        }

        if context.capture_gameplay() {
            return;
        }

        if let Some(id) = map_gilrs_button(button) {
            if let Some(input) = input {
                if let Ok(mut guard) = input.lock() {
                    guard.set_button(id, pressed);
                }
            }
        }
    }

    fn poll_gamepads(&mut self, input: Option<Arc<Mutex<JoypadState>>>) {
        let events = {
            let Some(gilrs) = self.gilrs.as_mut() else {
                return;
            };
            let mut events = Vec::new();
            while let Some(event) = gilrs.next_event() {
                let name = match event.event {
                    EventType::Connected | EventType::Disconnected => {
                        Some(gilrs.gamepad(event.id).name().to_string())
                    }
                    _ => None,
                };
                events.push((event.event, name));
            }
            events
        };
        let mut saw_disconnect = false;
        for (event, name) in events {
            match event {
                EventType::Connected => {
                    let name = name.as_deref().unwrap_or("Unknown");
                    self.ui.push_toast(
                        ui::ToastKind::Success,
                        format!("Gamepad connected: {name}"),
                    );
                    self.dualsense_buttons_enabled
                        .store(false, Ordering::Relaxed);
                    continue;
                }
                EventType::Disconnected => {
                    let name = name.as_deref().unwrap_or("Unknown");
                    self.ui.push_toast(
                        ui::ToastKind::Error,
                        format!("Gamepad disconnected: {name}"),
                    );
                    saw_disconnect = true;
                    continue;
                }
                _ => {}
            }

            if let EventType::ButtonChanged(button, value, _) = event {
                let pressed = value >= GAMEPAD_AXIS_THRESHOLD;
                self.handle_gamepad_button(button, pressed, input.clone());
                continue;
            }

            if let EventType::AxisChanged(axis, value, _) = event {
                if let Some(buttons) = axis_to_dpad_buttons(axis, value) {
                    for (button, pressed) in buttons {
                        self.handle_gamepad_button(button, pressed, input.clone());
                    }
                }
                continue;
            }

            if let EventType::ButtonPressed(button, _) = event {
                self.handle_gamepad_button(button, true, input.clone());
                continue;
            }
            if let EventType::ButtonReleased(button, _) = event {
                self.handle_gamepad_button(button, false, input.clone());
                continue;
            }
        }
        if saw_disconnect {
            let has_gamepad = self
                .gilrs
                .as_ref()
                .map(|gilrs| gilrs.gamepads().next().is_some())
                .unwrap_or(false);
            self.dualsense_buttons_enabled
                .store(!has_gamepad, Ordering::Relaxed);
        }
    }

    fn handle_keyboard(&mut self, key: KeyCode, pressed: bool) {
        if self.ui.is_editing_text() {
            return;
        }
        if let Some(action) = input::action_from_key(key, pressed) {
            self.apply_action(action);
            return;
        }
        if self.gui.ctx.wants_keyboard_input() {
            return;
        }

        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let Some(id) = map_keycode(key) else {
            return;
        };
        if let Ok(mut guard) = runtime.input_state().lock() {
            guard.set_button(id, pressed);
        }
    }

    fn apply_action(&mut self, action: Action) {
        self.ui.record_interaction();
        match action {
            Action::NextItem => self.navigate_feed(1),
            Action::PrevItem => self.navigate_feed(-1),
            Action::SelectIndex(index) => self.select_feed_index(index),
            Action::CreateByte => self.create_byte(),
            Action::OpenOfficialPickerCurrent => {
                let Some(feed) = self.feed.as_ref() else {
                    return;
                };
                let Some(item) = feed.current() else {
                    return;
                };
                if let FeedItem::RomFallback(fallback) = item {
                    self.ui
                        .open_official_picker(feed.current_index, fallback, &feed.store);
                } else {
                    self.ui.push_toast(
                        ui::ToastKind::Error,
                        "Official picker is only available for ROMs.".to_string(),
                    );
                }
            }
            Action::CancelUi => {
                self.ui.cancel_active_ui();
            }
            Action::OfficialPickerMove(delta) => {
                self.ui.move_official_picker_selection(delta);
            }
            Action::OfficialPickerConfirm => {
                if let Some((index, title)) = self.ui.confirm_official_picker_selection() {
                    self.set_official_title(index, title);
                }
            }
            Action::RenameTitle { index, title } => self.rename_feed_title(index, title),
            Action::SetOfficialTitle { index, title } => self.set_official_title(index, title),
            Action::ClearOfficialTitle { index } => self.clear_official_title(index),
            Action::ToggleOverlay => self.ui.toggle_overlay(),
        }
    }

    fn rename_feed_title(&mut self, index: usize, title: String) {
        let Some(feed) = self.feed.as_mut() else {
            return;
        };
        let Some(item) = feed.items.get(index).cloned() else {
            return;
        };
        let store = feed.store.clone();
        let trimmed = title.trim();
        match item {
            FeedItem::Byte(mut byte) => {
                byte.title = trimmed.to_string();
                if let Err(err) = store.update_metadata(&byte) {
                    let message = format!("Rename failed: {err}");
                    self.feed_error = Some(message.clone());
                    self.ui.push_toast(ui::ToastKind::Error, message);
                    return;
                }
                feed.items[index] = FeedItem::Byte(byte);
            }
            FeedItem::RomFallback(mut fallback) => {
                if let Err(err) = store.set_rom_title(&fallback.rom_sha1, trimmed) {
                    let message = format!("Rename failed: {err}");
                    self.feed_error = Some(message.clone());
                    self.ui.push_toast(ui::ToastKind::Error, message);
                    return;
                }
                fallback.title = if trimmed.is_empty() {
                    title_from_rom_path(&fallback.rom_path)
                } else {
                    trimmed.to_string()
                };
                feed.items[index] = FeedItem::RomFallback(fallback);
            }
        }
        self.feed_error = None;
        self.ui
            .push_toast(ui::ToastKind::Success, "Title updated".to_string());
    }

    fn set_official_title(&mut self, index: usize, title: String) {
        let Some(feed) = self.feed.as_mut() else {
            return;
        };
        let Some(item) = feed.items.get(index).cloned() else {
            return;
        };
        let store = feed.store.clone();
        let trimmed = title.trim();
        match item {
            FeedItem::RomFallback(mut fallback) => {
                if let Err(err) = store.set_rom_official_override(&fallback.rom_sha1, Some(trimmed)) {
                    let message = format!("Official game update failed: {err}");
                    self.feed_error = Some(message.clone());
                    self.ui.push_toast(ui::ToastKind::Error, message);
                    return;
                }
                fallback.official_title = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                };
                let rom_sha1 = fallback.rom_sha1.clone();
                feed.items[index] = FeedItem::RomFallback(fallback);
                self.ui.invalidate_cover_art(&rom_sha1);
                self.feed_error = None;
                self.ui.push_toast(
                    ui::ToastKind::Success,
                    "Official game updated".to_string(),
                );
            }
            _ => {}
        }
    }

    fn clear_official_title(&mut self, index: usize) {
        let Some(feed) = self.feed.as_mut() else {
            return;
        };
        let Some(item) = feed.items.get(index).cloned() else {
            return;
        };
        let store = feed.store.clone();
        match item {
            FeedItem::RomFallback(mut fallback) => {
                if let Err(err) = store.set_rom_official_override(&fallback.rom_sha1, None) {
                    let message = format!("Official game update failed: {err}");
                    self.feed_error = Some(message.clone());
                    self.ui.push_toast(ui::ToastKind::Error, message);
                    return;
                }
                fallback.official_title = None;
                let rom_sha1 = fallback.rom_sha1.clone();
                feed.items[index] = FeedItem::RomFallback(fallback);
                self.ui.invalidate_cover_art(&rom_sha1);
                self.feed_error = None;
                self.ui.push_toast(
                    ui::ToastKind::Success,
                    "Official game cleared".to_string(),
                );
            }
            _ => {}
        }
    }

    fn store_session_autosave(&mut self, key: Option<SessionAutosaveKey>) {
        let Some(key) = key else {
            return;
        };
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        match runtime.serialize() {
            Ok(state) => {
                self.session_autosaves.insert(key, state);
            }
            Err(err) => {
                self.ui
                    .push_toast(ui::ToastKind::Error, format!("Autosave failed: {err}"));
            }
        }
    }

    fn restore_session_autosave(&mut self, key: Option<SessionAutosaveKey>, load: &mut RuntimeLoad) {
        let Some(key) = key else {
            return;
        };
        let Some(state) = self.session_autosaves.get(&key) else {
            return;
        };
        if let Err(err) = load.runtime.unserialize(state) {
            self.ui.push_toast(
                ui::ToastKind::Error,
                format!("Autosave restore failed: {err}"),
            );
        }
    }

    fn navigate_feed(&mut self, delta: i32) {
        let (leaving_key, changed) = {
            let Some(feed) = self.feed.as_mut() else {
                return;
            };
            let leaving_key = feed.current().map(FeedItem::session_autosave_key);
            let previous_index = feed.current_index;
            if delta > 0 {
                feed.next();
            } else if delta < 0 {
                feed.prev();
            } else {
                feed.current();
            }
            let changed = feed.current_index != previous_index;
            (leaving_key, changed)
        };

        if !changed {
            return;
        }

        self.store_session_autosave(leaving_key);

        // Libretro cores (and our callback wiring) are effectively single-instance.
        // Drop the current runtime BEFORE constructing the next one to avoid
        // shared-global-state cores (e.g. bsnes) corrupting each other during swaps.
        self.audio_stream = None;
        self.runtime = None;
        self.runtime_meta = None;

        let result = {
            let Some(feed) = self.feed.as_ref() else {
                return;
            };
            feed.build_runtime_for_current()
        };

        match result {
            Ok(mut load) => {
                let entering_key = self
                    .feed
                    .as_ref()
                    .and_then(|feed| feed.current().map(FeedItem::session_autosave_key));
                self.restore_session_autosave(entering_key, &mut load);
                self.apply_runtime_load(load);
                self.feed_error = None;
                if let Some(feed) = self.feed.as_ref() {
                    feed.prefetch_neighbors();
                }
            }
            Err(err) => self.feed_error = Some(format!("Load feed item failed: {err}")),
        }
    }

    fn select_feed_index(&mut self, index: usize) {
        let (leaving_key, changed) = {
            let Some(feed) = self.feed.as_mut() else {
                return;
            };
            let leaving_key = feed.current().map(FeedItem::session_autosave_key);
            let previous_index = feed.current_index;
            feed.select(index);
            let changed = feed.current_index != previous_index;
            (leaving_key, changed)
        };

        if !changed {
            return;
        }

        self.store_session_autosave(leaving_key);

        // See note in `navigate_feed`.
        self.audio_stream = None;
        self.runtime = None;
        self.runtime_meta = None;

        let result = {
            let Some(feed) = self.feed.as_ref() else {
                return;
            };
            feed.build_runtime_for_current()
        };

        match result {
            Ok(mut load) => {
                let entering_key = self
                    .feed
                    .as_ref()
                    .and_then(|feed| feed.current().map(FeedItem::session_autosave_key));
                self.restore_session_autosave(entering_key, &mut load);
                self.apply_runtime_load(load);
                self.feed_error = None;
                if let Some(feed) = self.feed.as_ref() {
                    feed.prefetch_neighbors();
                }
            }
            Err(err) => self.feed_error = Some(format!("Load feed item failed: {err}")),
        }
    }

    fn apply_runtime_load(&mut self, load: RuntimeLoad) {
        self.audio_stream = build_audio_stream(load.runtime.audio_buffer()).ok();
        self.runtime_meta = Some(load.meta);
        self.runtime = Some(load.runtime);
        self.accumulator = 0.0;
        self.ui.trigger_transition();
    }

    fn create_byte(&mut self) {
        let runtime = match self.runtime.as_ref() {
            Some(runtime) => runtime,
            None => {
                self.feed_error = Some("No active runtime to capture".to_string());
                self.ui.push_toast(
                    ui::ToastKind::Error,
                    "No active runtime to capture".to_string(),
                );
                return;
            }
        };
        let meta = match self.runtime_meta.as_ref() {
            Some(meta) => meta.clone(),
            None => {
                self.feed_error = Some("Missing runtime metadata".to_string());
                self.ui.push_toast(
                    ui::ToastKind::Error,
                    "Missing runtime metadata".to_string(),
                );
                return;
            }
        };
        let frame = match runtime.latest_frame() {
            Some(frame) => frame,
            None => {
                self.feed_error = Some("No frame available for thumbnail".to_string());
                self.ui.push_toast(
                    ui::ToastKind::Error,
                    "No frame available for thumbnail".to_string(),
                );
                return;
            }
        };
        let state = match runtime.serialize() {
            Ok(state) => state,
            Err(err) => {
                self.feed_error = Some(format!("Serialize failed: {err}"));
                self.ui.push_toast(
                    ui::ToastKind::Error,
                    format!("Serialize failed: {err}"),
                );
                return;
            }
        };
        let thumbnail = match encode_thumbnail(&frame) {
            Ok(png) => png,
            Err(err) => {
                self.feed_error = Some(format!("Thumbnail failed: {err}"));
                self.ui.push_toast(
                    ui::ToastKind::Error,
                    format!("Thumbnail failed: {err}"),
                );
                return;
            }
        };

        let byte_id = Uuid::new_v4().to_string();
        let created_at = time::OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "unknown".to_string());
        let metadata = ByteMetadata {
            byte_id: byte_id.clone(),
            system: meta.system.clone(),
            core_id: meta.core_id.clone(),
            core_semver: meta.core_version.clone(),
            rom_sha1: meta.rom_sha1.clone(),
            region: None,
            title: format!("Byte {}", &byte_id[..8]),
            description: String::new(),
            tags: Vec::new(),
            author: "local".to_string(),
            created_at,
            thumbnail_path: "thumbnail.png".to_string(),
            state_path: "state.zst".to_string(),
        };

        let store = if let Some(feed) = self.feed.as_ref() {
            feed.store.clone()
        } else {
            LocalByteStore::new(&self.data_root)
        };

        if let Err(err) = store.save_byte(&metadata, &state, &thumbnail) {
            self.feed_error = Some(format!("Save Byte failed: {err}"));
            self.ui
                .push_toast(ui::ToastKind::Error, format!("Save Byte failed: {err}"));
            return;
        }

        if let Some(feed) = self.feed.as_mut() {
            feed.add_byte(metadata.clone());
        }

        self.feed_error = None;
        self.ui.push_toast(
            ui::ToastKind::Success,
            format!("Saved Byte {}", metadata.byte_id),
        );
    }

    fn update_video_texture(&mut self, frame: &playbyte_libretro::VideoFrame) {
        if frame.width == 0 || frame.height == 0 {
            return;
        }
        let rgba = convert_frame_to_rgba(frame);
        if frame.width != self.video_texture.width || frame.height != self.video_texture.height {
            self.video_texture = Self::create_video_texture(
                &self.device,
                &self.queue,
                &self.texture_layout,
                frame.width,
                frame.height,
                &rgba,
            );
        } else {
            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.video_texture.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * frame.width),
                    rows_per_image: Some(frame.height),
                },
                wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    fn create_video_texture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> VideoTexture {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("video-texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        VideoTexture {
            texture,
            _view: view,
            bind_group,
            width,
            height,
        }
    }

    fn prepare_gui(&mut self, window: &winit::window::Window) -> GuiOutput {
        let raw_input = self.gui.winit.take_egui_input(window);
        self.gui.ctx.begin_frame(raw_input);
        let ui_output = self.ui.render(
            &self.gui.ctx,
            ui::UiContext {
                feed: self.feed.as_ref(),
                runtime: self.runtime.as_ref(),
                frame_stats: &self.frame_stats,
                feed_error: self.feed_error.as_deref(),
            },
        );

        let output = self.gui.ctx.end_frame();
        self.gui
            .winit
            .handle_platform_output(window, output.platform_output);
        let paint_jobs = self
            .gui
            .ctx
            .tessellate(output.shapes, output.pixels_per_point);

        let pixels_per_point = egui_winit::pixels_per_point(&self.gui.ctx, window);
        let screen_desc = ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point,
        };

        for (id, delta) in &output.textures_delta.set {
            self.gui
                .renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        for id in &output.textures_delta.free {
            self.gui.renderer.free_texture(id);
        }

        GuiOutput {
            paint_jobs,
            screen_desc,
            actions: ui_output.actions,
        }
    }

    fn render(&mut self, window: &winit::window::Window) -> Result<(), wgpu::SurfaceError> {
        let gui_output = self.prepare_gui(window);
        if !gui_output.actions.is_empty() {
            for action in gui_output.actions {
                self.apply_action(action);
            }
        }

        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("video-encoder"),
            });
        let mut egui_cmds = self.gui.renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &gui_output.paint_jobs,
            &gui_output.screen_desc,
        );

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("video-render-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_bind_group(0, &self.video_texture.bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            render_pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..self.num_indices, 0, 0..1);

            self.gui.renderer.render(
                &mut render_pass,
                &gui_output.paint_jobs,
                &gui_output.screen_desc,
            );
        }

        egui_cmds.push(encoder.finish());
        self.queue.submit(egui_cmds);
        frame.present();
        Ok(())
    }
}

fn map_gilrs_button(button: Button) -> Option<u32> {
    match button {
        Button::South => Some(RETRO_DEVICE_ID_JOYPAD_B),
        Button::East => Some(RETRO_DEVICE_ID_JOYPAD_A),
        Button::West => Some(RETRO_DEVICE_ID_JOYPAD_Y),
        Button::North => Some(RETRO_DEVICE_ID_JOYPAD_X),
        Button::Select => Some(RETRO_DEVICE_ID_JOYPAD_SELECT),
        Button::Start => Some(RETRO_DEVICE_ID_JOYPAD_START),
        Button::DPadUp => Some(RETRO_DEVICE_ID_JOYPAD_UP),
        Button::DPadDown => Some(RETRO_DEVICE_ID_JOYPAD_DOWN),
        Button::DPadLeft => Some(RETRO_DEVICE_ID_JOYPAD_LEFT),
        Button::DPadRight => Some(RETRO_DEVICE_ID_JOYPAD_RIGHT),
        Button::LeftTrigger => Some(RETRO_DEVICE_ID_JOYPAD_L),
        Button::RightTrigger => Some(RETRO_DEVICE_ID_JOYPAD_R),
        _ => None,
    }
}

fn map_keycode(key: KeyCode) -> Option<u32> {
    match key {
        KeyCode::KeyZ => Some(RETRO_DEVICE_ID_JOYPAD_B),
        KeyCode::KeyX => Some(RETRO_DEVICE_ID_JOYPAD_A),
        KeyCode::ShiftLeft | KeyCode::ShiftRight => Some(RETRO_DEVICE_ID_JOYPAD_SELECT),
        KeyCode::Enter => Some(RETRO_DEVICE_ID_JOYPAD_START),
        KeyCode::ArrowUp => Some(RETRO_DEVICE_ID_JOYPAD_UP),
        KeyCode::ArrowDown => Some(RETRO_DEVICE_ID_JOYPAD_DOWN),
        KeyCode::ArrowLeft => Some(RETRO_DEVICE_ID_JOYPAD_LEFT),
        KeyCode::ArrowRight => Some(RETRO_DEVICE_ID_JOYPAD_RIGHT),
        _ => None,
    }
}

fn convert_frame_to_rgba(frame: &playbyte_libretro::VideoFrame) -> Vec<u8> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut out = vec![0u8; width * height * 4];

    let bytes_per_pixel = match frame.pixel_format {
        playbyte_libretro::RetroPixelFormat::Xrgb8888 => 4,
        playbyte_libretro::RetroPixelFormat::Rgb565 | playbyte_libretro::RetroPixelFormat::_0rgb1555 => 2,
    };
    let min_pitch = width.saturating_mul(bytes_per_pixel);
    if frame.pitch < min_pitch {
        return out;
    }
    if frame.data.len() < frame.pitch.saturating_mul(height) {
        return out;
    }

    match frame.pixel_format {
        playbyte_libretro::RetroPixelFormat::Xrgb8888 => {
            for y in 0..height {
                let row_start = y * frame.pitch;
                let row = &frame.data[row_start..row_start + min_pitch];
                for x in 0..width {
                    let src = x * 4;
                    let dst = (y * width + x) * 4;
                    let b = row[src];
                    let g = row[src + 1];
                    let r = row[src + 2];
                    out[dst] = r;
                    out[dst + 1] = g;
                    out[dst + 2] = b;
                    out[dst + 3] = 0xff;
                }
            }
        }
        playbyte_libretro::RetroPixelFormat::Rgb565 => {
            for y in 0..height {
                let row_start = y * frame.pitch;
                let row = &frame.data[row_start..row_start + min_pitch];
                for x in 0..width {
                    let src = x * 2;
                    let value = u16::from_le_bytes([row[src], row[src + 1]]);
                    let r = ((value >> 11) & 0x1f) as u8;
                    let g = ((value >> 5) & 0x3f) as u8;
                    let b = (value & 0x1f) as u8;
                    let dst = (y * width + x) * 4;
                    out[dst] = (r << 3) | (r >> 2);
                    out[dst + 1] = (g << 2) | (g >> 4);
                    out[dst + 2] = (b << 3) | (b >> 2);
                    out[dst + 3] = 0xff;
                }
            }
        }
        playbyte_libretro::RetroPixelFormat::_0rgb1555 => {
            for y in 0..height {
                let row_start = y * frame.pitch;
                let row = &frame.data[row_start..row_start + min_pitch];
                for x in 0..width {
                    let src = x * 2;
                    let value = u16::from_le_bytes([row[src], row[src + 1]]);
                    let r = ((value >> 10) & 0x1f) as u8;
                    let g = ((value >> 5) & 0x1f) as u8;
                    let b = (value & 0x1f) as u8;
                    let dst = (y * width + x) * 4;
                    out[dst] = (r << 3) | (r >> 2);
                    out[dst + 1] = (g << 3) | (g >> 2);
                    out[dst + 2] = (b << 3) | (b >> 2);
                    out[dst + 3] = 0xff;
                }
            }
        }
    }

    out
}

fn encode_thumbnail(frame: &playbyte_libretro::VideoFrame) -> Result<Vec<u8>> {
    let rgba = convert_frame_to_rgba(frame);
    let mut png = Vec::new();
    let encoder = PngEncoder::new(&mut png);
    encoder.write_image(&rgba, frame.width, frame.height, ColorType::Rgba8.into())?;
    Ok(png)
}

fn build_runtime_meta_from_runtime(
    runtime: &EmulatorRuntime,
    rom_path: &PathBuf,
) -> Result<RuntimeMetadata> {
    let info = runtime.system_info();
    let rom_sha1 = hash_rom(rom_path)?;
    Ok(RuntimeMetadata {
        core_id: info.library_name.clone(),
        core_version: info.library_version.clone(),
        rom_sha1,
        _rom_path: rom_path.clone(),
        system: system_from_rom_path(rom_path),
    })
}

fn hash_rom_without_snes_header(path: &PathBuf) -> Result<Option<String>> {
    let data = fs::read(path)?;
    if data.len() <= 512 || data.len() % 1024 != 512 {
        return Ok(None);
    }
    let mut hasher = Sha1::new();
    hasher.update(&data[512..]);
    Ok(Some(format!("{:x}", hasher.finalize())))
}

fn hash_rom(path: &PathBuf) -> Result<String> {
    let data = fs::read(path)?;
    let mut hasher = Sha1::new();
    hasher.update(data);
    Ok(format!("{:x}", hasher.finalize()))
}

fn system_from_rom_path(path: &PathBuf) -> System {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_lowercase())
    {
        Some(ext) if ext == "gba" => System::Gba,
        Some(ext) if ext == "gb" || ext == "gbc" => System::Gbc,
        Some(ext) if ext == "sfc" || ext == "smc" => System::Snes,
        Some(ext) if ext == "nes" => System::Nes,
        _ => System::Nes,
    }
}

fn build_audio_stream(audio: Arc<AudioRingBuffer>) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("No output audio device available"))?;
    let config = device.default_output_config()?;
    let sample_format = config.sample_format();
    let stream_config = config.into();

    let err_fn = |err| eprintln!("audio stream error: {err}");

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            &stream_config,
            move |data: &mut [f32], _| write_audio(data, &audio),
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_output_stream(
            &stream_config,
            move |data: &mut [i16], _| write_audio(data, &audio),
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_output_stream(
            &stream_config,
            move |data: &mut [u16], _| write_audio(data, &audio),
            err_fn,
            None,
        )?,
        _ => return Err(anyhow::anyhow!("Unsupported audio sample format")),
    };

    stream.play()?;
    Ok(stream)
}

fn write_audio<T>(output: &mut [T], audio: &AudioRingBuffer)
where
    T: cpal::Sample + cpal::FromSample<f32>,
{
    let mut temp = vec![0i16; output.len()];
    audio.pop_samples(&mut temp);
    for (dst, sample) in output.iter_mut().zip(temp.into_iter()) {
        let sample_f32 = sample as f32 / i16::MAX as f32;
        *dst = T::from_sample(sample_f32);
    }
}

fn main() -> Result<()> {
    let app_config = AppConfig::from_env();
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let dualsense_buttons_enabled = Arc::new(AtomicBool::new(true));
    let _dualsense_thread = if app_config.dualsense_swipes {
        Some(dualsense::spawn_dualsense_listener(
            proxy,
            Arc::clone(&dualsense_buttons_enabled),
        ))
    } else {
        None
    };
    let window = WindowBuilder::new()
        .with_title("Playbyte")
        .build(&event_loop)?;
    let mut state = pollster::block_on(State::new(
        &window,
        app_config,
        Arc::clone(&dualsense_buttons_enabled),
    ))?;

    event_loop.run(move |event, elwt| {
        state.gui.handle_event(&window, &event);
        match event {
            Event::UserEvent(UserEvent::Action(action)) => {
                state.apply_action(action);
                window.request_redraw();
            }
            Event::UserEvent(UserEvent::GamepadButton { button, pressed }) => {
                let input = state.runtime.as_ref().map(|runtime| runtime.input_state());
                state.handle_gamepad_button(button, pressed, input);
                window.request_redraw();
            }
            Event::WindowEvent { event, window_id } if window_id == window.id() => match event {
                WindowEvent::CloseRequested => elwt.exit(),
                WindowEvent::Resized(size) => state.resize(size),
                WindowEvent::RedrawRequested => match state.render(&window) {
                    Ok(()) => {}
                    Err(wgpu::SurfaceError::Lost) => state.resize(state.size),
                    Err(wgpu::SurfaceError::OutOfMemory) => elwt.exit(),
                    Err(wgpu::SurfaceError::Outdated) | Err(wgpu::SurfaceError::Timeout) => {}
                },
                WindowEvent::KeyboardInput { event, .. } => {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        let pressed = event.state == ElementState::Pressed;
                        state.handle_keyboard(code, pressed);
                    }
                }
                _ => {}
            },
            Event::AboutToWait => {
                let now = Instant::now();
                let dt = now.saturating_duration_since(state.last_update);
                state.last_update = now;
                state.update(dt);
                window.request_redraw();
            }
            _ => {}
        }
    })?;

    Ok(())
}
