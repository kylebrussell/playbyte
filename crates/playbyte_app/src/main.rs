use anyhow::{bail, Context, Result};
use bytemuck::{Pod, Zeroable};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use egui_wgpu::ScreenDescriptor;
use gilrs::{Button, EventType, Gilrs};
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder};
use playbyte_emulation::{
    AudioRingBuffer, EmulatorRuntime, JoypadState, RETRO_DEVICE_ID_JOYPAD_A,
    RETRO_DEVICE_ID_JOYPAD_B, RETRO_DEVICE_ID_JOYPAD_DOWN, RETRO_DEVICE_ID_JOYPAD_L,
    RETRO_DEVICE_ID_JOYPAD_LEFT, RETRO_DEVICE_ID_JOYPAD_R, RETRO_DEVICE_ID_JOYPAD_RIGHT,
    RETRO_DEVICE_ID_JOYPAD_SELECT, RETRO_DEVICE_ID_JOYPAD_START, RETRO_DEVICE_ID_JOYPAD_UP,
};
use playbyte_feed::{LocalByteStore, RomLibrary};
use playbyte_types::{ByteMetadata, System};
use sha1::{Digest, Sha1};
use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use wgpu::util::DeviceExt;
use winit::{
    event::{ElementState, Event, WindowEvent},
    event_loop::EventLoop,
    keyboard::{KeyCode, PhysicalKey},
    window::WindowBuilder,
};

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
}

impl AppConfig {
    fn from_env() -> Self {
        let mut core_path = None;
        let mut rom_path = None;
        let mut data_root = PathBuf::from("./data");
        let mut rom_root = PathBuf::from("./roms");
        let mut cores_root = PathBuf::from("./cores");
        let mut vsync = true;
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
                    data_root = args.next().map(PathBuf::from).unwrap_or(data_root);
                }
                "--roms" => {
                    rom_root = args.next().map(PathBuf::from).unwrap_or(rom_root);
                }
                "--cores" => {
                    cores_root = args.next().map(PathBuf::from).unwrap_or(cores_root);
                }
                "--no-vsync" => {
                    vsync = false;
                }
                _ => {}
            }
        }

        Self {
            core_path,
            rom_path,
            data_root,
            rom_root,
            cores_root,
            vsync,
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
            None
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

struct FeedController {
    store: LocalByteStore,
    roms: RomLibrary,
    core_locator: CoreLocator,
    bytes: Vec<ByteMetadata>,
    current_index: usize,
}

impl FeedController {
    fn load(config: &AppConfig) -> Result<Self> {
        let store = LocalByteStore::new(&config.data_root);
        let bytes = store.load_index()?;

        let mut roms = RomLibrary::new();
        roms.add_root(&config.rom_root);
        let _ = roms.scan()?;

        let core_locator = CoreLocator::new(config.cores_root.clone());

        Ok(Self {
            store,
            roms,
            core_locator,
            bytes,
            current_index: 0,
        })
    }

    fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn current(&self) -> Option<&ByteMetadata> {
        self.bytes.get(self.current_index)
    }

    fn select(&mut self, index: usize) -> Option<&ByteMetadata> {
        if index < self.bytes.len() {
            self.current_index = index;
        }
        self.current()
    }

    fn next(&mut self) -> Option<&ByteMetadata> {
        if self.bytes.is_empty() {
            return None;
        }
        self.current_index = (self.current_index + 1).min(self.bytes.len() - 1);
        self.current()
    }

    fn prev(&mut self) -> Option<&ByteMetadata> {
        if self.bytes.is_empty() {
            return None;
        }
        if self.current_index > 0 {
            self.current_index -= 1;
        }
        self.current()
    }

    fn prefetch_neighbors(&self) {
        if self.bytes.is_empty() {
            return;
        }
        let mut ids = Vec::new();
        if let Some(current) = self.current() {
            ids.push(current.byte_id.clone());
        }
        if self.current_index > 0 {
            ids.push(self.bytes[self.current_index - 1].byte_id.clone());
        }
        if self.current_index + 1 < self.bytes.len() {
            ids.push(self.bytes[self.current_index + 1].byte_id.clone());
        }
        self.store.prefetch(&ids);
    }

    fn build_runtime_for_current(&self) -> Result<RuntimeLoad> {
        let byte = self
            .current()
            .context("no selected Byte available in feed")?;
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

    fn handle_event(&mut self, window: &winit::window::Window, event: &Event<()>) {
        if let Event::WindowEvent { event, .. } = event {
            let _ = self.winit.on_window_event(window, event);
        }
    }
}

struct GuiOutput {
    paint_jobs: Vec<egui::ClippedPrimitive>,
    screen_desc: ScreenDescriptor,
    selected_index: Option<usize>,
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
    gilrs: Option<Gilrs>,
    audio_stream: Option<cpal::Stream>,
    feed: Option<FeedController>,
    feed_error: Option<String>,
    status_message: Option<String>,
    gui: GuiState,
    last_update: Instant,
    accumulator: f64,
    frame_stats: FrameStats,
    data_root: PathBuf,
}

impl State {
    async fn new(window: &winit::window::Window, app_config: AppConfig) -> Result<Self> {
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

        if let (Some(core), Some(rom)) = (app_config.core_path.clone(), app_config.rom_path.clone())
        {
            match EmulatorRuntime::new(core, rom.clone()) {
                Ok(rt) => match build_runtime_meta_from_runtime(&rt, &rom) {
                    Ok(meta) => runtime_load = Some(RuntimeLoad { runtime: rt, meta }),
                    Err(err) => feed_error = Some(format!("Runtime meta error: {err}")),
                },
                Err(err) => feed_error = Some(format!("Runtime error: {err}")),
            }
        } else {
            match FeedController::load(&app_config) {
                Ok(controller) => {
                    if controller.is_empty() {
                        feed_error = Some("No Bytes found in data/bytes".to_string());
                    } else {
                        feed = Some(controller);
                    }
                }
                Err(err) => feed_error = Some(format!("Feed error: {err}")),
            }
        }

        if let Some(controller) = &feed {
            match controller.build_runtime_for_current() {
                Ok(load) => runtime_load = Some(load),
                Err(err) => feed_error = Some(format!("Load Byte failed: {err}")),
            }
        }

        if let Some(controller) = &feed {
            controller.prefetch_neighbors();
        }

        let (runtime, runtime_meta) = match runtime_load {
            Some(load) => (Some(load.runtime), Some(load.meta)),
            None => (None, None),
        };
        let gilrs = Gilrs::new().ok();
        let audio_stream = runtime
            .as_ref()
            .and_then(|rt| build_audio_stream(rt.audio_buffer()).ok());

        let gui = GuiState::new(window, &device, surface_config.format);

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
            gilrs,
            audio_stream,
            feed,
            feed_error,
            status_message: None,
            gui,
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
        if let Some(input) = input_state {
            self.poll_gamepads(input);
        }
        if let Some(runtime) = self.runtime.as_mut() {
            self.accumulator += dt.as_secs_f64();
            let frame_time = 1.0 / runtime.fps();
            while self.accumulator >= frame_time {
                runtime.run_frame();
                self.accumulator -= frame_time;
            }
            if let Some(frame) = runtime.latest_frame() {
                self.update_video_texture(&frame);
            }
        }
    }

    fn poll_gamepads(&mut self, input: Arc<Mutex<JoypadState>>) {
        let Some(gilrs) = self.gilrs.as_mut() else {
            return;
        };
        let mut events = Vec::new();
        while let Some(event) = gilrs.next_event() {
            events.push(event);
        }
        for event in events {
            if let EventType::ButtonPressed(button, _) = event.event {
                if let Some(delta) = feed_nav_button(button) {
                    self.navigate_feed(delta);
                    continue;
                }
            }

            let pressed = matches!(event.event, EventType::ButtonPressed(_, _));
            let released = matches!(event.event, EventType::ButtonReleased(_, _));
            let button = match event.event {
                EventType::ButtonPressed(button, _) => Some(button),
                EventType::ButtonReleased(button, _) => Some(button),
                _ => None,
            };
            if let Some(id) = button.and_then(map_gilrs_button) {
                if let Ok(mut guard) = input.lock() {
                    guard.set_button(id, pressed && !released);
                }
            }
        }
    }

    fn handle_keyboard(&mut self, key: KeyCode, pressed: bool) {
        if let Some(delta) = feed_nav_key(key) {
            if pressed {
                self.navigate_feed(delta);
            }
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

    fn navigate_feed(&mut self, delta: i32) {
        let load_result = {
            let Some(feed) = self.feed.as_mut() else {
                return;
            };

            let has_selection = if delta > 0 {
                feed.next().is_some()
            } else if delta < 0 {
                feed.prev().is_some()
            } else {
                feed.current().is_some()
            };

            if has_selection {
                Some(feed.build_runtime_for_current())
            } else {
                None
            }
        };

        if let Some(result) = load_result {
            match result {
                Ok(load) => {
                    self.apply_runtime_load(load);
                    self.feed_error = None;
                    if let Some(feed) = self.feed.as_ref() {
                        feed.prefetch_neighbors();
                    }
                }
                Err(err) => self.feed_error = Some(format!("Load Byte failed: {err}")),
            }
        }
    }

    fn select_feed_index(&mut self, index: usize) {
        let load_result = {
            let Some(feed) = self.feed.as_mut() else {
                return;
            };
            if feed.select(index).is_some() {
                Some(feed.build_runtime_for_current())
            } else {
                None
            }
        };

        if let Some(result) = load_result {
            match result {
                Ok(load) => {
                    self.apply_runtime_load(load);
                    self.feed_error = None;
                    if let Some(feed) = self.feed.as_ref() {
                        feed.prefetch_neighbors();
                    }
                }
                Err(err) => self.feed_error = Some(format!("Load Byte failed: {err}")),
            }
        }
    }

    fn apply_runtime_load(&mut self, load: RuntimeLoad) {
        self.audio_stream = build_audio_stream(load.runtime.audio_buffer()).ok();
        self.runtime_meta = Some(load.meta);
        self.runtime = Some(load.runtime);
        self.accumulator = 0.0;
        self.status_message = None;
    }

    fn create_byte(&mut self) {
        let runtime = match self.runtime.as_ref() {
            Some(runtime) => runtime,
            None => {
                self.feed_error = Some("No active runtime to capture".to_string());
                return;
            }
        };
        let meta = match self.runtime_meta.as_ref() {
            Some(meta) => meta.clone(),
            None => {
                self.feed_error = Some("Missing runtime metadata".to_string());
                return;
            }
        };
        let frame = match runtime.latest_frame() {
            Some(frame) => frame,
            None => {
                self.feed_error = Some("No frame available for thumbnail".to_string());
                return;
            }
        };
        let state = match runtime.serialize() {
            Ok(state) => state,
            Err(err) => {
                self.feed_error = Some(format!("Serialize failed: {err}"));
                return;
            }
        };
        let thumbnail = match encode_thumbnail(&frame) {
            Ok(png) => png,
            Err(err) => {
                self.feed_error = Some(format!("Thumbnail failed: {err}"));
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
            return;
        }

        if let Some(feed) = self.feed.as_mut() {
            feed.bytes.push(metadata.clone());
            feed.current_index = feed.bytes.len().saturating_sub(1);
        }

        self.status_message = Some(format!("Saved Byte {}", metadata.byte_id));
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

        let mut selected_index = None;
        let mut create_byte = false;

        egui::TopBottomPanel::top("top_bar").show(&self.gui.ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Playbyte");
                if self.runtime.is_some() {
                    if ui.button("Bookmark Byte").clicked() {
                        create_byte = true;
                    }
                }
                if let Some(fps) = self.frame_stats.avg_fps() {
                    ui.label(format!("{fps:.0} fps"));
                }
                if let Some(runtime) = &self.runtime {
                    ui.label(format!("emu {:.1} Hz", runtime.fps()));
                }
                if let Some(feed) = &self.feed {
                    ui.label(format!("{} bytes", feed.bytes.len()));
                }
            });
        });

        egui::CentralPanel::default().show(&self.gui.ctx, |ui| {
            if let Some(feed) = &self.feed {
                for (idx, byte) in feed.bytes.iter().enumerate() {
                    let title = if byte.title.is_empty() {
                        &byte.byte_id
                    } else {
                        &byte.title
                    };
                    if ui
                        .selectable_label(idx == feed.current_index, title)
                        .clicked()
                    {
                        selected_index = Some(idx);
                    }
                }
            } else {
                ui.label("No feed loaded. Pass --core and --rom or add Bytes in ./data/bytes.");
            }

            if let Some(error) = &self.feed_error {
                ui.add_space(12.0);
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            if let Some(status) = &self.status_message {
                ui.add_space(6.0);
                ui.colored_label(egui::Color32::LIGHT_GREEN, status);
            }
        });

        if create_byte {
            self.create_byte();
        }

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
            selected_index,
        }
    }

    fn render(&mut self, window: &winit::window::Window) -> Result<(), wgpu::SurfaceError> {
        let gui_output = self.prepare_gui(window);
        if let Some(index) = gui_output.selected_index {
            self.select_feed_index(index);
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

fn feed_nav_button(button: Button) -> Option<i32> {
    match button {
        Button::LeftTrigger2 => Some(-1),
        Button::RightTrigger2 => Some(1),
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

fn feed_nav_key(key: KeyCode) -> Option<i32> {
    match key {
        KeyCode::PageUp => Some(-1),
        KeyCode::PageDown => Some(1),
        _ => None,
    }
}

fn convert_frame_to_rgba(frame: &playbyte_libretro::VideoFrame) -> Vec<u8> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let mut out = vec![0u8; width * height * 4];

    match frame.pixel_format {
        playbyte_libretro::RetroPixelFormat::Xrgb8888 => {
            for y in 0..height {
                let row = &frame.data[y * frame.pitch..y * frame.pitch + width * 4];
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
                let row = &frame.data[y * frame.pitch..y * frame.pitch + width * 2];
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
                let row = &frame.data[y * frame.pitch..y * frame.pitch + width * 2];
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
        Some(ext) if ext == "sfc" || ext == "smc" => System::Snes,
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
    let event_loop = EventLoop::new()?;
    let window = WindowBuilder::new()
        .with_title("Playbyte")
        .build(&event_loop)?;
    let mut state = pollster::block_on(State::new(&window, app_config))?;

    event_loop.run(move |event, elwt| {
        state.gui.handle_event(&window, &event);
        match event {
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
