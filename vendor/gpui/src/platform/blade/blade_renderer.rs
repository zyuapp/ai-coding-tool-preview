// Doing `if let` gives you nice scoping with passes/encoders
#![allow(irrefutable_let_patterns)]

use super::{BladeAtlas, BladeContext};
use crate::{
    BackdropBlur, Background, Bounds, DevicePixels, FadeMask, GpuSpecs, MonochromeSprite, Path, Point, PolychromeSprite,
    PrimitiveBatch, Quad, ScaledPixels, Scene, Shadow, Size, Underline,
};
use blade_graphics as gpu;
use blade_util::{BufferBelt, BufferBeltDescriptor};
use bytemuck::{Pod, Zeroable};
#[cfg(target_os = "macos")]
use media::core_video::CVMetalTextureCache;
use std::sync::Arc;

const MAX_FRAME_TIME_MS: u32 = 10000;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlobalParams {
    viewport_size: [f32; 2],
    premultiplied_alpha: u32,
    pad: u32,
}

//Note: we can't use `Bounds` directly here because
// it doesn't implement Pod + Zeroable
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PodBounds {
    origin: [f32; 2],
    size: [f32; 2],
}

impl From<Bounds<ScaledPixels>> for PodBounds {
    fn from(bounds: Bounds<ScaledPixels>) -> Self {
        Self {
            origin: [bounds.origin.x.0, bounds.origin.y.0],
            size: [bounds.size.width.0, bounds.size.height.0],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SurfaceParams {
    bounds: PodBounds,
    content_mask: PodBounds,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BackdropPassParams {
    source_origin: [f32; 2],
    source_size: [f32; 2],
    direction: [f32; 2],
    scale: f32,
    sigma: f32,
    frame_origin: [f32; 2],
    pad: [f32; 2],
    hidden: [f32; 4],
    hidden_color: [f32; 4],
    holes: [[f32; 4]; BACKDROP_HOLES],
}

/// How many parts of the hidden region a backdrop filter still sees.
const BACKDROP_HOLES: usize = 4;

/// What a backdrop filter reads differently from what was drawn: a region read as a color, the
/// parts of it read as drawn, and a region the filtered copy replaces.
#[derive(Clone, Copy, Default)]
struct BackdropScene {
    hidden: [f32; 4],
    hidden_color: [f32; 4],
    holes: [[f32; 4]; BACKDROP_HOLES],
    replaced: Option<[f32; 4]>,
}

fn rect(bounds: Bounds<ScaledPixels>) -> [f32; 4] {
    [
        bounds.origin.x.0,
        bounds.origin.y.0,
        bounds.origin.x.0 + bounds.size.width.0,
        bounds.origin.y.0 + bounds.size.height.0,
    ]
}

impl BackdropScene {
    fn new(scene: &Scene, blur: &BackdropBlur) -> Self {
        let covers = |outer: &Bounds<ScaledPixels>, inner: &Bounds<ScaledPixels>| {
            outer.origin.x <= inner.origin.x
                && outer.origin.y <= inner.origin.y
                && outer.bottom_right().x >= inner.bottom_right().x
                && outer.bottom_right().y >= inner.bottom_right().y
        };
        let mut this = Self::default();
        // A veil over the whole region, such as a floating sidebar's, still frosts it.
        if let Some((hidden, color)) = scene.backdrop_hidden.filter(|(hidden, _)| !covers(&blur.bounds, hidden)) {
            this.hidden = rect(hidden);
            this.hidden_color = [color.r * color.a, color.g * color.a, color.b * color.a, color.a];
            if let Some(tint) = scene.hidden_tint_for(blur.order, &blur.bounds) {
                let under = this.hidden_color;
                this.hidden_color = [
                    tint.r * tint.a + under[0] * (1. - tint.a),
                    tint.g * tint.a + under[1] * (1. - tint.a),
                    tint.b * tint.a + under[2] * (1. - tint.a),
                    tint.a + under[3] * (1. - tint.a),
                ];
            }
        }
        // A float shown to backdrops does not see through itself: its own frost still reads the
        // hidden region as hidden.
        let holes = scene.backdrop_holes.iter().filter(|hole| !covers(hole, &blur.bounds));
        for (slot, hole) in this.holes.iter_mut().zip(holes) {
            *slot = rect(*hole);
        }
        this.replaced = scene.replaced_under(&blur.bounds).map(rect);
        this
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BackdropEraseParams {
    rect: [f32; 4],
}

#[derive(blade_macros::ShaderData)]
struct ShaderBackdropEraseData {
    globals: GlobalParams,
    backdrop_blur: BackdropBlurParams,
    backdrop_erase: BackdropEraseParams,
}

fn fade_array(fade: &FadeMask) -> [f32; 8] {
    [fade.y_clear, fade.y_solid, fade.y_end_solid, fade.y_end_clear, fade.x_clear, fade.x_solid, fade.x_end_solid, fade.x_end_clear]
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BackdropBlurParams {
    bounds: PodBounds,
    content_mask: PodBounds,
    corner_radii: [f32; 4],
    saturation: f32,
    brightness: f32,
    opacity: f32,
    pad: f32,
    fade: [f32; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BackdropCompositeParams {
    origin: [f32; 2],
    scale: f32,
    pad: f32,
    blurred_size: [f32; 2],
    pad2: [f32; 2],
}

#[derive(blade_macros::ShaderData)]
struct ShaderBackdropPassData {
    backdrop_pass: BackdropPassParams,
    t_backdrop: gpu::TextureView,
    s_backdrop: gpu::Sampler,
}

#[derive(blade_macros::ShaderData)]
struct ShaderBackdropCompositeData {
    globals: GlobalParams,
    backdrop_blur: BackdropBlurParams,
    backdrop_composite: BackdropCompositeParams,
    t_backdrop: gpu::TextureView,
    s_backdrop: gpu::Sampler,
}

#[derive(blade_macros::ShaderData)]
struct ShaderQuadsData {
    globals: GlobalParams,
    b_quads: gpu::BufferPiece,
}

#[derive(blade_macros::ShaderData)]
struct ShaderShadowsData {
    globals: GlobalParams,
    b_shadows: gpu::BufferPiece,
}

#[derive(blade_macros::ShaderData)]
struct ShaderPathRasterizationData {
    globals: GlobalParams,
    b_path_vertices: gpu::BufferPiece,
}

#[derive(blade_macros::ShaderData)]
struct ShaderPathsData {
    globals: GlobalParams,
    t_sprite: gpu::TextureView,
    s_sprite: gpu::Sampler,
    b_path_sprites: gpu::BufferPiece,
}

#[derive(blade_macros::ShaderData)]
struct ShaderUnderlinesData {
    globals: GlobalParams,
    b_underlines: gpu::BufferPiece,
}

#[derive(blade_macros::ShaderData)]
struct ShaderMonoSpritesData {
    globals: GlobalParams,
    gamma_ratios: [f32; 4],
    grayscale_enhanced_contrast: f32,
    t_sprite: gpu::TextureView,
    s_sprite: gpu::Sampler,
    b_mono_sprites: gpu::BufferPiece,
}

#[derive(blade_macros::ShaderData)]
struct ShaderPolySpritesData {
    globals: GlobalParams,
    t_sprite: gpu::TextureView,
    s_sprite: gpu::Sampler,
    b_poly_sprites: gpu::BufferPiece,
}

#[derive(blade_macros::ShaderData)]
struct ShaderSurfacesData {
    globals: GlobalParams,
    surface_locals: SurfaceParams,
    t_y: gpu::TextureView,
    t_cb_cr: gpu::TextureView,
    s_surface: gpu::Sampler,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(C)]
struct PathSprite {
    bounds: Bounds<ScaledPixels>,
}

#[derive(Clone, Debug)]
#[repr(C)]
struct PathRasterizationVertex {
    xy_position: Point<ScaledPixels>,
    st_position: Point<f32>,
    color: Background,
    bounds: Bounds<ScaledPixels>,
    fade: FadeMask,
}

struct BladePipelines {
    quads: gpu::RenderPipeline,
    shadows: gpu::RenderPipeline,
    path_rasterization: gpu::RenderPipeline,
    paths: gpu::RenderPipeline,
    underlines: gpu::RenderPipeline,
    mono_sprites: gpu::RenderPipeline,
    poly_sprites: gpu::RenderPipeline,
    surfaces: gpu::RenderPipeline,
    backdrop_downsample: gpu::RenderPipeline,
    backdrop_blur: gpu::RenderPipeline,
    backdrop_composite: gpu::RenderPipeline,
    backdrop_erase: gpu::RenderPipeline,
}

impl BladePipelines {
    fn new(gpu: &gpu::Context, surface_info: gpu::SurfaceInfo, path_sample_count: u32) -> Self {
        use gpu::ShaderData as _;

        log::info!(
            "Initializing Blade pipelines for surface {:?}",
            surface_info
        );
        let shader = gpu.create_shader(gpu::ShaderDesc {
            source: include_str!("shaders.wgsl"),
        });
        shader.check_struct_size::<GlobalParams>();
        shader.check_struct_size::<SurfaceParams>();
        shader.check_struct_size::<Quad>();
        shader.check_struct_size::<Shadow>();
        shader.check_struct_size::<PathRasterizationVertex>();
        shader.check_struct_size::<PathSprite>();
        shader.check_struct_size::<Underline>();
        shader.check_struct_size::<MonochromeSprite>();
        shader.check_struct_size::<PolychromeSprite>();
        shader.check_struct_size::<BackdropPassParams>();
        shader.check_struct_size::<BackdropBlurParams>();
        shader.check_struct_size::<BackdropCompositeParams>();
        shader.check_struct_size::<BackdropEraseParams>();

        // See https://apoorvaj.io/alpha-compositing-opengl-blending-and-premultiplied-alpha/
        let blend_mode = match surface_info.alpha {
            gpu::AlphaMode::Ignored => gpu::BlendState::ALPHA_BLENDING,
            gpu::AlphaMode::PreMultiplied => gpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
            gpu::AlphaMode::PostMultiplied => gpu::BlendState::ALPHA_BLENDING,
        };
        let color_targets = &[gpu::ColorTargetState {
            format: surface_info.format,
            blend: Some(blend_mode),
            write_mask: gpu::ColorWrites::default(),
        }];

        Self {
            quads: gpu.create_render_pipeline(gpu::RenderPipelineDesc {
                name: "quads",
                data_layouts: &[&ShaderQuadsData::layout()],
                vertex: shader.at("vs_quad"),
                vertex_fetches: &[],
                primitive: gpu::PrimitiveState {
                    topology: gpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                fragment: Some(shader.at("fs_quad")),
                color_targets,
                multisample_state: gpu::MultisampleState::default(),
            }),
            shadows: gpu.create_render_pipeline(gpu::RenderPipelineDesc {
                name: "shadows",
                data_layouts: &[&ShaderShadowsData::layout()],
                vertex: shader.at("vs_shadow"),
                vertex_fetches: &[],
                primitive: gpu::PrimitiveState {
                    topology: gpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                fragment: Some(shader.at("fs_shadow")),
                color_targets,
                multisample_state: gpu::MultisampleState::default(),
            }),
            path_rasterization: gpu.create_render_pipeline(gpu::RenderPipelineDesc {
                name: "path_rasterization",
                data_layouts: &[&ShaderPathRasterizationData::layout()],
                vertex: shader.at("vs_path_rasterization"),
                vertex_fetches: &[],
                primitive: gpu::PrimitiveState {
                    topology: gpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                fragment: Some(shader.at("fs_path_rasterization")),
                // The original implementation was using ADDITIVE blende mode,
                // I don't know why
                // color_targets: &[gpu::ColorTargetState {
                //     format: PATH_TEXTURE_FORMAT,
                //     blend: Some(gpu::BlendState::ADDITIVE),
                //     write_mask: gpu::ColorWrites::default(),
                // }],
                color_targets: &[gpu::ColorTargetState {
                    format: surface_info.format,
                    blend: Some(gpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: gpu::ColorWrites::default(),
                }],
                multisample_state: gpu::MultisampleState {
                    sample_count: path_sample_count,
                    ..Default::default()
                },
            }),
            paths: gpu.create_render_pipeline(gpu::RenderPipelineDesc {
                name: "paths",
                data_layouts: &[&ShaderPathsData::layout()],
                vertex: shader.at("vs_path"),
                vertex_fetches: &[],
                primitive: gpu::PrimitiveState {
                    topology: gpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                fragment: Some(shader.at("fs_path")),
                color_targets: &[gpu::ColorTargetState {
                    format: surface_info.format,
                    blend: Some(gpu::BlendState {
                        color: gpu::BlendComponent::OVER,
                        alpha: gpu::BlendComponent::ADDITIVE,
                    }),
                    write_mask: gpu::ColorWrites::default(),
                }],
                multisample_state: gpu::MultisampleState::default(),
            }),
            underlines: gpu.create_render_pipeline(gpu::RenderPipelineDesc {
                name: "underlines",
                data_layouts: &[&ShaderUnderlinesData::layout()],
                vertex: shader.at("vs_underline"),
                vertex_fetches: &[],
                primitive: gpu::PrimitiveState {
                    topology: gpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                fragment: Some(shader.at("fs_underline")),
                color_targets,
                multisample_state: gpu::MultisampleState::default(),
            }),
            mono_sprites: gpu.create_render_pipeline(gpu::RenderPipelineDesc {
                name: "mono-sprites",
                data_layouts: &[&ShaderMonoSpritesData::layout()],
                vertex: shader.at("vs_mono_sprite"),
                vertex_fetches: &[],
                primitive: gpu::PrimitiveState {
                    topology: gpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                fragment: Some(shader.at("fs_mono_sprite")),
                color_targets,
                multisample_state: gpu::MultisampleState::default(),
            }),
            poly_sprites: gpu.create_render_pipeline(gpu::RenderPipelineDesc {
                name: "poly-sprites",
                data_layouts: &[&ShaderPolySpritesData::layout()],
                vertex: shader.at("vs_poly_sprite"),
                vertex_fetches: &[],
                primitive: gpu::PrimitiveState {
                    topology: gpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                fragment: Some(shader.at("fs_poly_sprite")),
                color_targets,
                multisample_state: gpu::MultisampleState::default(),
            }),
            surfaces: gpu.create_render_pipeline(gpu::RenderPipelineDesc {
                name: "surfaces",
                data_layouts: &[&ShaderSurfacesData::layout()],
                vertex: shader.at("vs_surface"),
                vertex_fetches: &[],
                primitive: gpu::PrimitiveState {
                    topology: gpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                fragment: Some(shader.at("fs_surface")),
                color_targets,
                multisample_state: gpu::MultisampleState::default(),
            }),
            backdrop_downsample: backdrop_pass_pipeline(
                gpu,
                &shader,
                "backdrop_downsample",
                "fs_backdrop_downsample",
                surface_info.format,
            ),
            backdrop_blur: backdrop_pass_pipeline(
                gpu,
                &shader,
                "backdrop_blur",
                "fs_backdrop_blur",
                surface_info.format,
            ),
            backdrop_composite: gpu.create_render_pipeline(gpu::RenderPipelineDesc {
                name: "backdrop_composite",
                data_layouts: &[&ShaderBackdropCompositeData::layout()],
                vertex: shader.at("vs_backdrop_composite"),
                vertex_fetches: &[],
                primitive: gpu::PrimitiveState {
                    topology: gpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                fragment: Some(shader.at("fs_backdrop_composite")),
                color_targets: &[gpu::ColorTargetState {
                    format: surface_info.format,
                    blend: Some(gpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: gpu::ColorWrites::default(),
                }],
                multisample_state: gpu::MultisampleState::default(),
            }),
            backdrop_erase: gpu.create_render_pipeline(gpu::RenderPipelineDesc {
                name: "backdrop_erase",
                data_layouts: &[&ShaderBackdropEraseData::layout()],
                vertex: shader.at("vs_backdrop_composite"),
                vertex_fetches: &[],
                primitive: gpu::PrimitiveState {
                    topology: gpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                fragment: Some(shader.at("fs_backdrop_erase")),
                // Scales what is drawn by one minus the fragment's alpha, writing nothing of its own.
                color_targets: &[gpu::ColorTargetState {
                    format: surface_info.format,
                    blend: Some(gpu::BlendState {
                        color: gpu::BlendComponent {
                            src_factor: gpu::BlendFactor::Zero,
                            dst_factor: gpu::BlendFactor::OneMinusSrcAlpha,
                            operation: gpu::BlendOperation::Add,
                        },
                        alpha: gpu::BlendComponent {
                            src_factor: gpu::BlendFactor::Zero,
                            dst_factor: gpu::BlendFactor::OneMinusSrcAlpha,
                            operation: gpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: gpu::ColorWrites::default(),
                }],
                multisample_state: gpu::MultisampleState::default(),
            }),
        }
    }

    fn destroy(&mut self, gpu: &gpu::Context) {
        gpu.destroy_render_pipeline(&mut self.quads);
        gpu.destroy_render_pipeline(&mut self.shadows);
        gpu.destroy_render_pipeline(&mut self.path_rasterization);
        gpu.destroy_render_pipeline(&mut self.paths);
        gpu.destroy_render_pipeline(&mut self.underlines);
        gpu.destroy_render_pipeline(&mut self.mono_sprites);
        gpu.destroy_render_pipeline(&mut self.poly_sprites);
        gpu.destroy_render_pipeline(&mut self.surfaces);
        gpu.destroy_render_pipeline(&mut self.backdrop_downsample);
        gpu.destroy_render_pipeline(&mut self.backdrop_blur);
        gpu.destroy_render_pipeline(&mut self.backdrop_composite);
        gpu.destroy_render_pipeline(&mut self.backdrop_erase);
    }
}

fn backdrop_pass_pipeline(
    gpu: &gpu::Context,
    shader: &gpu::Shader,
    name: &str,
    fragment: &str,
    format: gpu::TextureFormat,
) -> gpu::RenderPipeline {
    use gpu::ShaderData as _;
    gpu.create_render_pipeline(gpu::RenderPipelineDesc {
        name,
        data_layouts: &[&ShaderBackdropPassData::layout()],
        vertex: shader.at("vs_backdrop_pass"),
        vertex_fetches: &[],
        primitive: gpu::PrimitiveState {
            topology: gpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        fragment: Some(shader.at(fragment)),
        color_targets: &[gpu::ColorTargetState {
            format,
            blend: None,
            write_mask: gpu::ColorWrites::default(),
        }],
        multisample_state: gpu::MultisampleState::default(),
    })
}

/// Three scratch textures a backdrop blur works in: a copy of the region, and two for the passes.
struct BackdropTextures {
    source_size: gpu::Extent,
    blur_size: gpu::Extent,
    source: (gpu::Texture, gpu::TextureView),
    front: (gpu::Texture, gpu::TextureView),
    back: (gpu::Texture, gpu::TextureView),
}

impl BackdropTextures {
    fn destroy(self, gpu: &gpu::Context) {
        for (texture, view) in [self.source, self.front, self.back] {
            gpu.destroy_texture_view(view);
            gpu.destroy_texture(texture);
        }
    }
}

/// The device-pixel rectangle a backdrop blur reads: its bounds within the viewport, cut down to
/// the visible part plus the blur's reach.
fn backdrop_region(blur: &BackdropBlur, width: u32, height: u32) -> Option<([u32; 2], [u32; 2], f32, f32)> {
    let sigma = blur.blur_radius.0.max(0.);
    let viewport = Bounds {
        origin: crate::point(ScaledPixels(0.), ScaledPixels(0.)),
        size: crate::size(ScaledPixels(width as f32), ScaledPixels(height as f32)),
    };
    let visible = blur
        .bounds
        .intersect(&blur.content_mask.bounds)
        .intersect(&viewport);
    if visible.is_empty() {
        return None;
    }
    let read = visible
        .dilate(ScaledPixels(sigma * 3.))
        .intersect(&blur.bounds)
        .intersect(&viewport);
    let left = read.origin.x.0.floor().max(0.) as u32;
    let top = read.origin.y.0.floor().max(0.) as u32;
    let right = ((read.origin.x.0 + read.size.width.0).ceil() as u32).min(width);
    let bottom = ((read.origin.y.0 + read.size.height.0).ceil() as u32).min(height);
    if right <= left || bottom <= top {
        return None;
    }
    let scale = match sigma / 3. {
        s if s >= 8. => 8.,
        s if s >= 4. => 4.,
        s if s >= 2. => 2.,
        _ => 1.,
    };
    Some(([left, top], [right - left, bottom - top], scale, sigma / scale))
}

pub struct BladeSurfaceConfig {
    pub size: gpu::Extent,
    pub transparent: bool,
}

//Note: we could see some of these fields moved into `BladeContext`
// so that they are shared between windows. E.g. `pipelines`.
// But that is complicated by the fact that pipelines depend on
// the format and alpha mode.
pub struct BladeRenderer {
    gpu: Arc<gpu::Context>,
    surface: gpu::Surface,
    surface_config: gpu::SurfaceConfig,
    command_encoder: gpu::CommandEncoder,
    last_sync_point: Option<gpu::SyncPoint>,
    pipelines: BladePipelines,
    instance_belt: BufferBelt,
    atlas: Arc<BladeAtlas>,
    atlas_sampler: gpu::Sampler,
    #[cfg(target_os = "macos")]
    core_video_texture_cache: CVMetalTextureCache,
    path_intermediate_texture: gpu::Texture,
    path_intermediate_texture_view: gpu::TextureView,
    path_intermediate_msaa_texture: Option<gpu::Texture>,
    path_intermediate_msaa_texture_view: Option<gpu::TextureView>,
    rendering_parameters: RenderingParameters,
    backdrop_textures: Option<BackdropTextures>,
    /// Textures outgrown during a frame whose commands still use them, freed once it has run.
    retired_backdrop_textures: Vec<BackdropTextures>,
}

impl BladeRenderer {
    pub fn new<I: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle>(
        context: &BladeContext,
        window: &I,
        config: BladeSurfaceConfig,
    ) -> anyhow::Result<Self> {
        let surface_config = gpu::SurfaceConfig {
            size: config.size,
            usage: gpu::TextureUsage::TARGET | gpu::TextureUsage::COPY,
            display_sync: gpu::DisplaySync::Recent,
            color_space: gpu::ColorSpace::Srgb,
            allow_exclusive_full_screen: false,
            transparent: config.transparent,
        };
        let surface = context
            .gpu
            .create_surface_configured(window, surface_config)
            .map_err(|err| anyhow::anyhow!("Failed to create surface: {err:?}"))?;

        let command_encoder = context.gpu.create_command_encoder(gpu::CommandEncoderDesc {
            name: "main",
            buffer_count: 2,
        });
        let rendering_parameters = RenderingParameters::from_env(context);
        let pipelines = BladePipelines::new(
            &context.gpu,
            surface.info(),
            rendering_parameters.path_sample_count,
        );
        let instance_belt = BufferBelt::new(BufferBeltDescriptor {
            memory: gpu::Memory::Shared,
            min_chunk_size: 0x1000,
            alignment: 0x40, // Vulkan `minStorageBufferOffsetAlignment` on Intel Xe
        });
        let atlas = Arc::new(BladeAtlas::new(&context.gpu));
        let atlas_sampler = context.gpu.create_sampler(gpu::SamplerDesc {
            name: "path rasterization sampler",
            mag_filter: gpu::FilterMode::Linear,
            min_filter: gpu::FilterMode::Linear,
            ..Default::default()
        });

        let (path_intermediate_texture, path_intermediate_texture_view) =
            create_path_intermediate_texture(
                &context.gpu,
                surface.info().format,
                config.size.width,
                config.size.height,
            );
        let (path_intermediate_msaa_texture, path_intermediate_msaa_texture_view) =
            create_msaa_texture_if_needed(
                &context.gpu,
                surface.info().format,
                config.size.width,
                config.size.height,
                rendering_parameters.path_sample_count,
            )
            .unzip();

        #[cfg(target_os = "macos")]
        let core_video_texture_cache = unsafe {
            CVMetalTextureCache::new(
                objc2::rc::Retained::as_ptr(&context.gpu.metal_device()) as *mut _
            )
            .unwrap()
        };

        Ok(Self {
            gpu: Arc::clone(&context.gpu),
            surface,
            surface_config,
            command_encoder,
            last_sync_point: None,
            pipelines,
            instance_belt,
            atlas,
            atlas_sampler,
            #[cfg(target_os = "macos")]
            core_video_texture_cache,
            path_intermediate_texture,
            path_intermediate_texture_view,
            path_intermediate_msaa_texture,
            path_intermediate_msaa_texture_view,
            rendering_parameters,
            backdrop_textures: None,
            retired_backdrop_textures: Vec::new(),
        })
    }

    fn wait_for_gpu(&mut self) {
        if let Some(last_sp) = self.last_sync_point.take()
            && !self.gpu.wait_for(&last_sp, MAX_FRAME_TIME_MS)
        {
            log::error!("GPU hung");
            #[cfg(target_os = "linux")]
            if self.gpu.device_information().driver_name == "radv" {
                log::error!(
                    "there's a known bug with amdgpu/radv, try setting ZED_PATH_SAMPLE_COUNT=0 as a workaround"
                );
                log::error!(
                    "if that helps you're running into https://github.com/zed-industries/zed/issues/26143"
                );
            }
            log::error!(
                "your device information is: {:?}",
                self.gpu.device_information()
            );
            while !self.gpu.wait_for(&last_sp, MAX_FRAME_TIME_MS) {}
        }
    }

    pub fn update_drawable_size(&mut self, size: Size<DevicePixels>) {
        self.update_drawable_size_impl(size, false);
    }

    /// Like `update_drawable_size` but skips the check that the size has changed. This is useful in
    /// cases like restoring a window from minimization where the size is the same but the
    /// renderer's swap chain needs to be recreated.
    #[cfg_attr(
        any(target_os = "macos", target_os = "linux", target_os = "freebsd"),
        allow(dead_code)
    )]
    pub fn update_drawable_size_even_if_unchanged(&mut self, size: Size<DevicePixels>) {
        self.update_drawable_size_impl(size, true);
    }

    fn update_drawable_size_impl(&mut self, size: Size<DevicePixels>, always_resize: bool) {
        let gpu_size = gpu::Extent {
            width: size.width.0 as u32,
            height: size.height.0 as u32,
            depth: 1,
        };

        if always_resize || gpu_size != self.surface_config.size {
            self.wait_for_gpu();
            self.surface_config.size = gpu_size;
            self.gpu
                .reconfigure_surface(&mut self.surface, self.surface_config);
            self.gpu.destroy_texture(self.path_intermediate_texture);
            self.gpu
                .destroy_texture_view(self.path_intermediate_texture_view);
            if let Some(msaa_texture) = self.path_intermediate_msaa_texture {
                self.gpu.destroy_texture(msaa_texture);
            }
            if let Some(msaa_view) = self.path_intermediate_msaa_texture_view {
                self.gpu.destroy_texture_view(msaa_view);
            }
            let (path_intermediate_texture, path_intermediate_texture_view) =
                create_path_intermediate_texture(
                    &self.gpu,
                    self.surface.info().format,
                    gpu_size.width,
                    gpu_size.height,
                );
            self.path_intermediate_texture = path_intermediate_texture;
            self.path_intermediate_texture_view = path_intermediate_texture_view;
            let (path_intermediate_msaa_texture, path_intermediate_msaa_texture_view) =
                create_msaa_texture_if_needed(
                    &self.gpu,
                    self.surface.info().format,
                    gpu_size.width,
                    gpu_size.height,
                    self.rendering_parameters.path_sample_count,
                )
                .unzip();
            self.path_intermediate_msaa_texture = path_intermediate_msaa_texture;
            self.path_intermediate_msaa_texture_view = path_intermediate_msaa_texture_view;
        }
    }

    pub fn update_transparency(&mut self, transparent: bool) {
        if transparent != self.surface_config.transparent {
            self.wait_for_gpu();
            self.surface_config.transparent = transparent;
            self.gpu
                .reconfigure_surface(&mut self.surface, self.surface_config);
            self.pipelines.destroy(&self.gpu);
            self.pipelines = BladePipelines::new(
                &self.gpu,
                self.surface.info(),
                self.rendering_parameters.path_sample_count,
            );
        }
    }

    #[cfg_attr(
        any(target_os = "macos", feature = "wayland", target_os = "windows"),
        allow(dead_code)
    )]
    pub fn viewport_size(&self) -> gpu::Extent {
        self.surface_config.size
    }

    pub fn sprite_atlas(&self) -> &Arc<BladeAtlas> {
        &self.atlas
    }

    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn gpu_specs(&self) -> GpuSpecs {
        let info = self.gpu.device_information();

        GpuSpecs {
            is_software_emulated: info.is_software_emulated,
            device_name: info.device_name.clone(),
            driver_name: info.driver_name.clone(),
            driver_info: info.driver_info.clone(),
        }
    }

    #[cfg(target_os = "macos")]
    pub fn layer(&self) -> metal::MetalLayer {
        unsafe { foreign_types::ForeignType::from_ptr(self.layer_ptr()) }
    }

    #[cfg(target_os = "macos")]
    pub fn layer_ptr(&self) -> *mut metal::CAMetalLayer {
        objc2::rc::Retained::as_ptr(&self.surface.metal_layer()) as *mut _
    }

    #[profiling::function]
    fn draw_paths_to_intermediate(
        &mut self,
        paths: &[Path<ScaledPixels>],
        width: f32,
        height: f32,
    ) {
        self.command_encoder
            .init_texture(self.path_intermediate_texture);
        if let Some(msaa_texture) = self.path_intermediate_msaa_texture {
            self.command_encoder.init_texture(msaa_texture);
        }

        let target = if let Some(msaa_view) = self.path_intermediate_msaa_texture_view {
            gpu::RenderTarget {
                view: msaa_view,
                init_op: gpu::InitOp::Clear(gpu::TextureColor::TransparentBlack),
                finish_op: gpu::FinishOp::ResolveTo(self.path_intermediate_texture_view),
            }
        } else {
            gpu::RenderTarget {
                view: self.path_intermediate_texture_view,
                init_op: gpu::InitOp::Clear(gpu::TextureColor::TransparentBlack),
                finish_op: gpu::FinishOp::Store,
            }
        };
        if let mut pass = self.command_encoder.render(
            "rasterize paths",
            gpu::RenderTargetSet {
                colors: &[target],
                depth_stencil: None,
            },
        ) {
            let globals = GlobalParams {
                viewport_size: [width, height],
                premultiplied_alpha: 0,
                pad: 0,
            };
            let mut encoder = pass.with(&self.pipelines.path_rasterization);

            let mut vertices = Vec::new();
            for path in paths {
                vertices.extend(path.vertices.iter().map(|v| PathRasterizationVertex {
                    xy_position: v.xy_position,
                    st_position: v.st_position,
                    color: path.color,
                    fade: path.fade,
                    bounds: path.clipped_bounds(),
                }));
            }
            let vertex_buf = unsafe { self.instance_belt.alloc_typed(&vertices, &self.gpu) };
            encoder.bind(
                0,
                &ShaderPathRasterizationData {
                    globals,
                    b_path_vertices: vertex_buf,
                },
            );
            encoder.draw(0, vertices.len() as u32, 0, 1);
        }
    }

    pub fn destroy(&mut self) {
        self.wait_for_gpu();
        self.atlas.destroy();
        self.gpu.destroy_sampler(self.atlas_sampler);
        self.instance_belt.destroy(&self.gpu);
        self.gpu.destroy_command_encoder(&mut self.command_encoder);
        self.pipelines.destroy(&self.gpu);
        self.gpu.destroy_surface(&mut self.surface);
        self.gpu.destroy_texture(self.path_intermediate_texture);
        self.gpu
            .destroy_texture_view(self.path_intermediate_texture_view);
        if let Some(msaa_texture) = self.path_intermediate_msaa_texture {
            self.gpu.destroy_texture(msaa_texture);
        }
        if let Some(msaa_view) = self.path_intermediate_msaa_texture_view {
            self.gpu.destroy_texture_view(msaa_view);
        }
        if let Some(textures) = self.backdrop_textures.take() {
            textures.destroy(&self.gpu);
        }
        for textures in std::mem::take(&mut self.retired_backdrop_textures) {
            textures.destroy(&self.gpu);
        }
    }

    fn backdrop_textures(&mut self, source: [u32; 2], blur: [u32; 2]) -> &BackdropTextures {
        let fits = |have: gpu::Extent, need: [u32; 2]| have.width >= need[0] && have.height >= need[1];
        let ok = self
            .backdrop_textures
            .as_ref()
            .is_some_and(|t| fits(t.source_size, source) && fits(t.blur_size, blur));
        if !ok {
            let grow = |have: Option<gpu::Extent>, need: [u32; 2]| gpu::Extent {
                width: need[0].max(have.map_or(0, |e| e.width)).next_multiple_of(64),
                height: need[1].max(have.map_or(0, |e| e.height)).next_multiple_of(64),
                depth: 1,
            };
            let old = self.backdrop_textures.take();
            let source_size = grow(old.as_ref().map(|t| t.source_size), source);
            let blur_size = grow(old.as_ref().map(|t| t.blur_size), blur);
            if let Some(old) = old {
                self.retired_backdrop_textures.push(old);
            }
            let format = self.surface.info().format;
            let make = |name: &str, size: gpu::Extent| {
                let texture = self.gpu.create_texture(gpu::TextureDesc {
                    name,
                    format,
                    size,
                    array_layer_count: 1,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: gpu::TextureDimension::D2,
                    usage: gpu::TextureUsage::COPY
                        | gpu::TextureUsage::RESOURCE
                        | gpu::TextureUsage::TARGET,
                    external: None,
                });
                let view = self.gpu.create_texture_view(
                    texture,
                    gpu::TextureViewDesc {
                        name,
                        format,
                        dimension: gpu::ViewDimension::D2,
                        subresources: &Default::default(),
                    },
                );
                (texture, view)
            };
            let textures = BackdropTextures {
                source_size,
                blur_size,
                source: make("backdrop source", source_size),
                front: make("backdrop front", blur_size),
                back: make("backdrop back", blur_size),
            };
            self.command_encoder.init_texture(textures.source.0);
            self.command_encoder.init_texture(textures.front.0);
            self.command_encoder.init_texture(textures.back.0);
            self.backdrop_textures = Some(textures);
        }
        self.backdrop_textures.as_ref().unwrap()
    }

    /// Blurs the frame under `blur` into the front backdrop texture and composites it back.
    fn draw_backdrop_blur(&mut self, blur: &BackdropBlur, reads: BackdropScene, frame: &gpu::Frame, globals: GlobalParams) {
        let size = self.surface_config.size;
        let Some((origin, region, scale, sigma)) = backdrop_region(blur, size.width, size.height)
        else {
            return;
        };
        let target = [
            (region[0] as f32 / scale).ceil() as u32,
            (region[1] as f32 / scale).ceil() as u32,
        ];
        let (source, front, back) = {
            let textures = self.backdrop_textures(region, target);
            (textures.source, textures.front, textures.back)
        };
        {
            let mut transfer = self.command_encoder.transfer("backdrop copy");
            transfer.copy_texture_to_texture(
                gpu::TexturePiece {
                    texture: frame.texture(),
                    mip_level: 0,
                    array_layer: 0,
                    origin: [origin[0], origin[1], 0],
                },
                source.0.into(),
                gpu::Extent {
                    width: region[0],
                    height: region[1],
                    depth: 1,
                },
            );
        }
        let sampler = self.atlas_sampler;
        let mut run = |encoder: &mut gpu::CommandEncoder,
                       pipeline: &gpu::RenderPipeline,
                       input: gpu::TextureView,
                       output: gpu::TextureView,
                       params: BackdropPassParams| {
            if let mut pass = encoder.render(
                "backdrop pass",
                gpu::RenderTargetSet {
                    colors: &[gpu::RenderTarget {
                        view: output,
                        init_op: gpu::InitOp::Load,
                        finish_op: gpu::FinishOp::Store,
                    }],
                    depth_stencil: None,
                },
            ) {
                let mut encoder = pass.with(pipeline);
                encoder.set_viewport(&gpu::Viewport {
                    x: 0.0,
                    y: 0.0,
                    w: target[0] as f32,
                    h: target[1] as f32,
                    depth: 0.0..1.0,
                });
                encoder.set_scissor_rect(&gpu::ScissorRect {
                    x: 0,
                    y: 0,
                    w: target[0],
                    h: target[1],
                });
                encoder.bind(
                    0,
                    &ShaderBackdropPassData {
                        backdrop_pass: params,
                        t_backdrop: input,
                        s_backdrop: sampler,
                    },
                );
                encoder.draw(0, 3, 0, 1);
            }
        };
        run(
            &mut self.command_encoder,
            &self.pipelines.backdrop_downsample,
            source.1,
            front.1,
            BackdropPassParams {
                source_origin: [0., 0.],
                source_size: [region[0] as f32, region[1] as f32],
                direction: [0., 0.],
                scale,
                sigma,
                frame_origin: [origin[0] as f32, origin[1] as f32],
                pad: [0.; 2],
                hidden: reads.hidden,
                hidden_color: reads.hidden_color,
                holes: reads.holes,
            },
        );
        if sigma > 0.01 {
            let blurred = [target[0] as f32, target[1] as f32];
            for (input, output, direction) in [(front.1, back.1, [1., 0.]), (back.1, front.1, [0., 1.])] {
                run(
                    &mut self.command_encoder,
                    &self.pipelines.backdrop_blur,
                    input,
                    output,
                    BackdropPassParams {
                        source_origin: [0., 0.],
                        source_size: blurred,
                        direction,
                        scale: 1.,
                        sigma,
                        frame_origin: [0.; 2],
                        pad: [0.; 2],
                        hidden: [0.; 4],
                        hidden_color: [0.; 4],
                        holes: [[0.; 4]; BACKDROP_HOLES],
                    },
                );
            }
        }
        if let mut pass = self.command_encoder.render(
            "backdrop composite",
            gpu::RenderTargetSet {
                colors: &[gpu::RenderTarget {
                    view: frame.texture_view(),
                    init_op: gpu::InitOp::Load,
                    finish_op: gpu::FinishOp::Store,
                }],
                depth_stencil: None,
            },
        ) {
            let r = &blur.corner_radii;
            let backdrop_blur = BackdropBlurParams {
                bounds: blur.bounds.into(),
                content_mask: blur.content_mask.bounds.into(),
                corner_radii: [r.top_left.0, r.top_right.0, r.bottom_right.0, r.bottom_left.0],
                saturation: blur.saturation,
                brightness: blur.brightness,
                opacity: blur.opacity,
                pad: 0.,
                fade: fade_array(&blur.fade),
            };
            if let Some(rect) = reads.replaced {
                let mut encoder = pass.with(&self.pipelines.backdrop_erase);
                encoder.bind(
                    0,
                    &ShaderBackdropEraseData {
                        globals,
                        backdrop_blur,
                        backdrop_erase: BackdropEraseParams { rect },
                    },
                );
                encoder.draw(0, 4, 0, 1);
            }
            let mut encoder = pass.with(&self.pipelines.backdrop_composite);
            encoder.bind(
                0,
                &ShaderBackdropCompositeData {
                    globals,
                    backdrop_blur,
                    backdrop_composite: BackdropCompositeParams {
                        origin: [origin[0] as f32, origin[1] as f32],
                        scale,
                        pad: 0.,
                        blurred_size: [target[0] as f32, target[1] as f32],
                        pad2: [0.; 2],
                    },
                    t_backdrop: front.1,
                    s_backdrop: sampler,
                },
            );
            encoder.draw(0, 4, 0, 1);
        }
    }

    pub fn draw(&mut self, scene: &Scene) {
        if !self.retired_backdrop_textures.is_empty() {
            self.wait_for_gpu();
            for textures in std::mem::take(&mut self.retired_backdrop_textures) {
                textures.destroy(&self.gpu);
            }
        }
        if scene.backdrop_blurs.is_empty()
            && let Some(textures) = self.backdrop_textures.take()
        {
            self.wait_for_gpu();
            textures.destroy(&self.gpu);
        }
        self.command_encoder.start();
        self.atlas.before_frame(&mut self.command_encoder);

        let frame = {
            profiling::scope!("acquire frame");
            self.surface.acquire_frame()
        };
        self.command_encoder.init_texture(frame.texture());

        let globals = GlobalParams {
            viewport_size: [
                self.surface_config.size.width as f32,
                self.surface_config.size.height as f32,
            ],
            premultiplied_alpha: match self.surface.info().alpha {
                gpu::AlphaMode::Ignored | gpu::AlphaMode::PostMultiplied => 0,
                gpu::AlphaMode::PreMultiplied => 1,
            },
            pad: 0,
        };

        let mut pass = self.command_encoder.render(
            "main",
            gpu::RenderTargetSet {
                colors: &[gpu::RenderTarget {
                    view: frame.texture_view(),
                    init_op: gpu::InitOp::Clear(gpu::TextureColor::TransparentBlack),
                    finish_op: gpu::FinishOp::Store,
                }],
                depth_stencil: None,
            },
        );

        profiling::scope!("render pass");
        for batch in scene.batches() {
            match batch {
                PrimitiveBatch::Quads(quads) => {
                    let instance_buf = unsafe { self.instance_belt.alloc_typed(quads, &self.gpu) };
                    let mut encoder = pass.with(&self.pipelines.quads);
                    encoder.bind(
                        0,
                        &ShaderQuadsData {
                            globals,
                            b_quads: instance_buf,
                        },
                    );
                    encoder.draw(0, 4, 0, quads.len() as u32);
                }
                PrimitiveBatch::Shadows(shadows) => {
                    let instance_buf =
                        unsafe { self.instance_belt.alloc_typed(shadows, &self.gpu) };
                    let mut encoder = pass.with(&self.pipelines.shadows);
                    encoder.bind(
                        0,
                        &ShaderShadowsData {
                            globals,
                            b_shadows: instance_buf,
                        },
                    );
                    encoder.draw(0, 4, 0, shadows.len() as u32);
                }
                PrimitiveBatch::Paths(paths) => {
                    let Some(first_path) = paths.first() else {
                        continue;
                    };
                    drop(pass);
                    self.draw_paths_to_intermediate(
                        paths,
                        self.surface_config.size.width as f32,
                        self.surface_config.size.height as f32,
                    );
                    pass = self.command_encoder.render(
                        "main",
                        gpu::RenderTargetSet {
                            colors: &[gpu::RenderTarget {
                                view: frame.texture_view(),
                                init_op: gpu::InitOp::Load,
                                finish_op: gpu::FinishOp::Store,
                            }],
                            depth_stencil: None,
                        },
                    );
                    let mut encoder = pass.with(&self.pipelines.paths);
                    // When copying paths from the intermediate texture to the drawable,
                    // each pixel must only be copied once, in case of transparent paths.
                    //
                    // If all paths have the same draw order, then their bounds are all
                    // disjoint, so we can copy each path's bounds individually. If this
                    // batch combines different draw orders, we perform a single copy
                    // for a minimal spanning rect.
                    let sprites = if paths.last().unwrap().order == first_path.order {
                        paths
                            .iter()
                            .map(|path| PathSprite {
                                bounds: path.clipped_bounds(),
                            })
                            .collect()
                    } else {
                        let mut bounds = first_path.clipped_bounds();
                        for path in paths.iter().skip(1) {
                            bounds = bounds.union(&path.clipped_bounds());
                        }
                        vec![PathSprite { bounds }]
                    };
                    let instance_buf =
                        unsafe { self.instance_belt.alloc_typed(&sprites, &self.gpu) };
                    encoder.bind(
                        0,
                        &ShaderPathsData {
                            globals,
                            t_sprite: self.path_intermediate_texture_view,
                            s_sprite: self.atlas_sampler,
                            b_path_sprites: instance_buf,
                        },
                    );
                    encoder.draw(0, 4, 0, sprites.len() as u32);
                }
                PrimitiveBatch::Underlines(underlines) => {
                    let instance_buf =
                        unsafe { self.instance_belt.alloc_typed(underlines, &self.gpu) };
                    let mut encoder = pass.with(&self.pipelines.underlines);
                    encoder.bind(
                        0,
                        &ShaderUnderlinesData {
                            globals,
                            b_underlines: instance_buf,
                        },
                    );
                    encoder.draw(0, 4, 0, underlines.len() as u32);
                }
                PrimitiveBatch::MonochromeSprites {
                    texture_id,
                    sprites,
                } => {
                    let tex_info = self.atlas.get_texture_info(texture_id);
                    let instance_buf =
                        unsafe { self.instance_belt.alloc_typed(sprites, &self.gpu) };
                    let mut encoder = pass.with(&self.pipelines.mono_sprites);
                    encoder.bind(
                        0,
                        &ShaderMonoSpritesData {
                            globals,
                            gamma_ratios: self.rendering_parameters.gamma_ratios,
                            grayscale_enhanced_contrast: self
                                .rendering_parameters
                                .grayscale_enhanced_contrast,
                            t_sprite: tex_info.raw_view,
                            s_sprite: self.atlas_sampler,
                            b_mono_sprites: instance_buf,
                        },
                    );
                    encoder.draw(0, 4, 0, sprites.len() as u32);
                }
                PrimitiveBatch::PolychromeSprites {
                    texture_id,
                    sprites,
                } => {
                    let tex_info = self.atlas.get_texture_info(texture_id);
                    let instance_buf =
                        unsafe { self.instance_belt.alloc_typed(sprites, &self.gpu) };
                    let mut encoder = pass.with(&self.pipelines.poly_sprites);
                    encoder.bind(
                        0,
                        &ShaderPolySpritesData {
                            globals,
                            t_sprite: tex_info.raw_view,
                            s_sprite: self.atlas_sampler,
                            b_poly_sprites: instance_buf,
                        },
                    );
                    encoder.draw(0, 4, 0, sprites.len() as u32);
                }
                PrimitiveBatch::BackdropBlurs(blurs) => {
                    drop(pass);
                    for blur in blurs {
                        self.draw_backdrop_blur(blur, BackdropScene::new(scene, blur), &frame, globals);
                    }
                    pass = self.command_encoder.render(
                        "main",
                        gpu::RenderTargetSet {
                            colors: &[gpu::RenderTarget {
                                view: frame.texture_view(),
                                init_op: gpu::InitOp::Load,
                                finish_op: gpu::FinishOp::Store,
                            }],
                            depth_stencil: None,
                        },
                    );
                }
                PrimitiveBatch::Surfaces(surfaces) => {
                    let mut _encoder = pass.with(&self.pipelines.surfaces);

                    for surface in surfaces {
                        #[cfg(not(target_os = "macos"))]
                        {
                            let _ = surface;
                            continue;
                        };

                        #[cfg(target_os = "macos")]
                        {
                            let (t_y, t_cb_cr) = unsafe {
                                use core_foundation::base::TCFType as _;
                                use std::ptr;

                                assert_eq!(
                                        surface.image_buffer.get_pixel_format(),
                                        core_video::pixel_buffer::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
                                    );

                                let y_texture = self
                                    .core_video_texture_cache
                                    .create_texture_from_image(
                                        surface.image_buffer.as_concrete_TypeRef(),
                                        ptr::null(),
                                        metal::MTLPixelFormat::R8Unorm,
                                        surface.image_buffer.get_width_of_plane(0),
                                        surface.image_buffer.get_height_of_plane(0),
                                        0,
                                    )
                                    .unwrap();
                                let cb_cr_texture = self
                                    .core_video_texture_cache
                                    .create_texture_from_image(
                                        surface.image_buffer.as_concrete_TypeRef(),
                                        ptr::null(),
                                        metal::MTLPixelFormat::RG8Unorm,
                                        surface.image_buffer.get_width_of_plane(1),
                                        surface.image_buffer.get_height_of_plane(1),
                                        1,
                                    )
                                    .unwrap();
                                (
                                    gpu::TextureView::from_metal_texture(
                                        &objc2::rc::Retained::retain(
                                            foreign_types::ForeignTypeRef::as_ptr(
                                                y_texture.as_texture_ref(),
                                            )
                                                as *mut objc2::runtime::ProtocolObject<
                                                    dyn objc2_metal::MTLTexture,
                                                >,
                                        )
                                        .unwrap(),
                                        gpu::TexelAspects::COLOR,
                                    ),
                                    gpu::TextureView::from_metal_texture(
                                        &objc2::rc::Retained::retain(
                                            foreign_types::ForeignTypeRef::as_ptr(
                                                cb_cr_texture.as_texture_ref(),
                                            )
                                                as *mut objc2::runtime::ProtocolObject<
                                                    dyn objc2_metal::MTLTexture,
                                                >,
                                        )
                                        .unwrap(),
                                        gpu::TexelAspects::COLOR,
                                    ),
                                )
                            };

                            _encoder.bind(
                                0,
                                &ShaderSurfacesData {
                                    globals,
                                    surface_locals: SurfaceParams {
                                        bounds: surface.bounds.into(),
                                        content_mask: surface.content_mask.bounds.into(),
                                    },
                                    t_y,
                                    t_cb_cr,
                                    s_surface: self.atlas_sampler,
                                },
                            );

                            _encoder.draw(0, 4, 0, 1);
                        }
                    }
                }
            }
        }
        drop(pass);

        self.command_encoder.present(frame);
        let sync_point = self.gpu.submit(&mut self.command_encoder);

        profiling::scope!("finish");
        self.instance_belt.flush(&sync_point);
        self.atlas.after_frame(&sync_point);

        self.wait_for_gpu();
        self.last_sync_point = Some(sync_point);
    }
}

fn create_path_intermediate_texture(
    gpu: &gpu::Context,
    format: gpu::TextureFormat,
    width: u32,
    height: u32,
) -> (gpu::Texture, gpu::TextureView) {
    let texture = gpu.create_texture(gpu::TextureDesc {
        name: "path intermediate",
        format,
        size: gpu::Extent {
            width,
            height,
            depth: 1,
        },
        array_layer_count: 1,
        mip_level_count: 1,
        sample_count: 1,
        dimension: gpu::TextureDimension::D2,
        usage: gpu::TextureUsage::COPY | gpu::TextureUsage::RESOURCE | gpu::TextureUsage::TARGET,
        external: None,
    });
    let texture_view = gpu.create_texture_view(
        texture,
        gpu::TextureViewDesc {
            name: "path intermediate view",
            format,
            dimension: gpu::ViewDimension::D2,
            subresources: &Default::default(),
        },
    );
    (texture, texture_view)
}

fn create_msaa_texture_if_needed(
    gpu: &gpu::Context,
    format: gpu::TextureFormat,
    width: u32,
    height: u32,
    sample_count: u32,
) -> Option<(gpu::Texture, gpu::TextureView)> {
    if sample_count <= 1 {
        return None;
    }
    let texture_msaa = gpu.create_texture(gpu::TextureDesc {
        name: "path intermediate msaa",
        format,
        size: gpu::Extent {
            width,
            height,
            depth: 1,
        },
        array_layer_count: 1,
        mip_level_count: 1,
        sample_count,
        dimension: gpu::TextureDimension::D2,
        usage: gpu::TextureUsage::TARGET,
        external: None,
    });
    let texture_view_msaa = gpu.create_texture_view(
        texture_msaa,
        gpu::TextureViewDesc {
            name: "path intermediate msaa view",
            format,
            dimension: gpu::ViewDimension::D2,
            subresources: &Default::default(),
        },
    );

    Some((texture_msaa, texture_view_msaa))
}

/// A set of parameters that can be set using a corresponding environment variable.
struct RenderingParameters {
    // Env var: ZED_PATH_SAMPLE_COUNT
    // workaround for https://github.com/zed-industries/zed/issues/26143
    path_sample_count: u32,

    // Env var: ZED_FONTS_GAMMA
    // Allowed range [1.0, 2.2], other values are clipped
    // Default: 1.8
    gamma_ratios: [f32; 4],
    // Env var: ZED_FONTS_GRAYSCALE_ENHANCED_CONTRAST
    // Allowed range: [0.0, ..), other values are clipped
    // Default: 1.0
    grayscale_enhanced_contrast: f32,
}

impl RenderingParameters {
    fn from_env(context: &BladeContext) -> Self {
        use std::env;

        let path_sample_count = env::var("ZED_PATH_SAMPLE_COUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                [4, 2, 1]
                    .into_iter()
                    .find(|&n| (context.gpu.capabilities().sample_count_mask & n) != 0)
            })
            .unwrap_or(1);
        let gamma = env::var("ZED_FONTS_GAMMA")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.8_f32)
            .clamp(1.0, 2.2);
        let gamma_ratios = Self::get_gamma_ratios(gamma);
        let grayscale_enhanced_contrast = env::var("ZED_FONTS_GRAYSCALE_ENHANCED_CONTRAST")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0_f32)
            .max(0.0);

        Self {
            path_sample_count,
            gamma_ratios,
            grayscale_enhanced_contrast,
        }
    }

    // Gamma ratios for brightening/darkening edges for better contrast
    // https://github.com/microsoft/terminal/blob/1283c0f5b99a2961673249fa77c6b986efb5086c/src/renderer/atlas/dwrite.cpp#L50
    fn get_gamma_ratios(gamma: f32) -> [f32; 4] {
        const GAMMA_INCORRECT_TARGET_RATIOS: [[f32; 4]; 13] = [
            [0.0000 / 4.0, 0.0000 / 4.0, 0.0000 / 4.0, 0.0000 / 4.0], // gamma = 1.0
            [0.0166 / 4.0, -0.0807 / 4.0, 0.2227 / 4.0, -0.0751 / 4.0], // gamma = 1.1
            [0.0350 / 4.0, -0.1760 / 4.0, 0.4325 / 4.0, -0.1370 / 4.0], // gamma = 1.2
            [0.0543 / 4.0, -0.2821 / 4.0, 0.6302 / 4.0, -0.1876 / 4.0], // gamma = 1.3
            [0.0739 / 4.0, -0.3963 / 4.0, 0.8167 / 4.0, -0.2287 / 4.0], // gamma = 1.4
            [0.0933 / 4.0, -0.5161 / 4.0, 0.9926 / 4.0, -0.2616 / 4.0], // gamma = 1.5
            [0.1121 / 4.0, -0.6395 / 4.0, 1.1588 / 4.0, -0.2877 / 4.0], // gamma = 1.6
            [0.1300 / 4.0, -0.7649 / 4.0, 1.3159 / 4.0, -0.3080 / 4.0], // gamma = 1.7
            [0.1469 / 4.0, -0.8911 / 4.0, 1.4644 / 4.0, -0.3234 / 4.0], // gamma = 1.8
            [0.1627 / 4.0, -1.0170 / 4.0, 1.6051 / 4.0, -0.3347 / 4.0], // gamma = 1.9
            [0.1773 / 4.0, -1.1420 / 4.0, 1.7385 / 4.0, -0.3426 / 4.0], // gamma = 2.0
            [0.1908 / 4.0, -1.2652 / 4.0, 1.8650 / 4.0, -0.3476 / 4.0], // gamma = 2.1
            [0.2031 / 4.0, -1.3864 / 4.0, 1.9851 / 4.0, -0.3501 / 4.0], // gamma = 2.2
        ];

        const NORM13: f32 = ((0x10000 as f64) / (255.0 * 255.0) * 4.0) as f32;
        const NORM24: f32 = ((0x100 as f64) / (255.0) * 4.0) as f32;

        let index = ((gamma * 10.0).round() as usize).clamp(10, 22) - 10;
        let ratios = GAMMA_INCORRECT_TARGET_RATIOS[index];

        [
            ratios[0] * NORM13,
            ratios[1] * NORM24,
            ratios[2] * NORM13,
            ratios[3] * NORM24,
        ]
    }
}
