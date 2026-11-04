use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use wgpu::util::DeviceExt;

/// Longest side worth decoding: the camera frame the picture fills is no
/// larger in practice.
pub const MAX_PICTURE_DIMENSION: u32 = 1920;

/// Decoded RGBA8 pixels of a camera background picture.
pub struct BackgroundPicture {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

impl BackgroundPicture {
    /// `None` unless `rgba` holds exactly `width * height` pixels.
    pub fn new(rgba: Vec<u8>, width: u32, height: u32) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        (width > 0 && height > 0 && rgba.len() == expected).then_some(Self {
            rgba,
            width,
            height,
        })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

pub(crate) fn shader_source() -> String {
    [
        include_str!("shaders/mask_composite.wgsl"),
        include_str!("shaders/picture_composite.wgsl"),
    ]
    .join("\n")
}

/// Scale and offset taking a camera-frame UV to the picture UV that covers
/// the frame (cropped to fill, never stretched). The camera layer mirrors its
/// whole frame afterwards, so a mirrored camera gets a pre-flipped picture
/// and the picture still reads the right way round.
pub(crate) fn cover_transform(frame: (u32, u32), picture: (u32, u32), mirrored: bool) -> [f32; 4] {
    let frame_aspect = frame.0.max(1) as f32 / frame.1.max(1) as f32;
    let picture_aspect = picture.0.max(1) as f32 / picture.1.max(1) as f32;
    let (scale_x, scale_y) = if picture_aspect > frame_aspect {
        (frame_aspect / picture_aspect, 1.0)
    } else {
        (1.0, picture_aspect / frame_aspect)
    };
    let offset_x = (1.0 - scale_x) / 2.0;
    let offset_y = (1.0 - scale_y) / 2.0;
    if mirrored {
        [-scale_x, scale_y, offset_x + scale_x, offset_y]
    } else {
        [scale_x, scale_y, offset_x, offset_y]
    }
}

/// The value of a future that is already complete, as wgpu's native error
/// scopes are.
fn ready<F: Future>(future: F) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

pub(crate) struct PicturePipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

pub(crate) struct PicturePassInputs<'a> {
    pub sharp: &'a wgpu::TextureView,
    pub room: &'a wgpu::TextureView,
    pub mask: &'a wgpu::TextureView,
    pub matte: &'a wgpu::TextureView,
    pub output: &'a wgpu::TextureView,
}

impl PicturePipeline {
    /// Built inside an error scope: a device that rejects it leaves the
    /// camera blurred instead of taking the process down.
    pub fn new(device: &wgpu::Device, output_format: wgpu::TextureFormat) -> anyhow::Result<Self> {
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = Self::build(device, output_format);
        if let Some(Some(error)) = ready(device.pop_error_scope()) {
            anyhow::bail!("camera background picture pipeline: {error}");
        }
        Ok(pipeline)
    }

    fn build(device: &wgpu::Device, output_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Camera Picture Shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source().into()),
        });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Camera Picture Bind Group Layout"),
            entries: &[
                texture(0),
                texture(1),
                texture(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture(4),
                texture(5),
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Camera Picture Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Camera Picture Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_picture"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: output_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            bind_group_layout,
            sampler,
        }
    }

    /// Draws nothing until `picture.prepare` has run.
    pub fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        picture: &PictureTexture,
        inputs: PicturePassInputs<'_>,
    ) {
        let Some((_, uniforms)) = &picture.uniforms else {
            return;
        };
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Camera Picture Bind Group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(inputs.sharp),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(inputs.room),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(inputs.mask),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(inputs.matte),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&picture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: uniforms.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Camera Picture Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: inputs.output,
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// A picture on the GPU, with the cover transform for the last frame size.
pub(crate) struct PictureTexture {
    source: Arc<BackgroundPicture>,
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    uniforms: Option<(([u32; 2], bool), wgpu::Buffer)>,
}

impl PictureTexture {
    /// `None` when the picture is larger than this device can hold.
    pub fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: Arc<BackgroundPicture>,
    ) -> Option<Self> {
        let (width, height) = source.dimensions();
        let limit = device.limits().max_texture_dimension_2d;
        if width > limit || height > limit {
            tracing::warn!(width, height, limit, "camera background picture too large");
            return None;
        }
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Camera Background Picture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            &source.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            size,
        );
        Some(Self {
            view: texture.create_view(&Default::default()),
            _texture: texture,
            source,
            uniforms: None,
        })
    }

    pub fn is(&self, source: &Arc<BackgroundPicture>) -> bool {
        Arc::ptr_eq(&self.source, source)
    }

    pub fn prepare(&mut self, device: &wgpu::Device, frame: (u32, u32), mirrored: bool) {
        let key = ([frame.0, frame.1], mirrored);
        if self
            .uniforms
            .as_ref()
            .is_none_or(|(cached, _)| *cached != key)
        {
            let transform = cover_transform(frame, self.source.dimensions(), mirrored);
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Camera Picture Uniforms"),
                contents: bytemuck::cast_slice(&transform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            self.uniforms = Some((key, buffer));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture_uv(transform: [f32; 4], uv: (f32, f32)) -> (f32, f32) {
        (
            uv.0 * transform[0] + transform[2],
            uv.1 * transform[1] + transform[3],
        )
    }

    fn close(actual: (f32, f32), expected: (f32, f32)) -> bool {
        (actual.0 - expected.0).abs() < 1e-5 && (actual.1 - expected.1).abs() < 1e-5
    }

    #[test]
    fn a_wider_picture_is_cropped_left_and_right() {
        let transform = cover_transform((1280, 720), (2560, 720), false);
        let visible = (1280.0 / 720.0) / (2560.0 / 720.0);
        assert!(close(
            picture_uv(transform, (0.0, 0.0)),
            ((1.0 - visible) / 2.0, 0.0)
        ));
        assert!(close(
            picture_uv(transform, (1.0, 1.0)),
            ((1.0 + visible) / 2.0, 1.0)
        ));
        assert!(close(picture_uv(transform, (0.5, 0.5)), (0.5, 0.5)));
    }

    #[test]
    fn a_taller_picture_is_cropped_top_and_bottom() {
        let transform = cover_transform((1280, 720), (1000, 1000), false);
        let visible = 1.0 / (1280.0 / 720.0);
        assert!(close(
            picture_uv(transform, (0.0, 0.0)),
            (0.0, (1.0 - visible) / 2.0)
        ));
        assert!(close(
            picture_uv(transform, (1.0, 1.0)),
            (1.0, (1.0 + visible) / 2.0)
        ));
    }

    #[test]
    fn a_picture_of_the_same_shape_fills_exactly() {
        let transform = cover_transform((1280, 720), (1920, 1080), false);
        assert!(close(picture_uv(transform, (0.0, 0.0)), (0.0, 0.0)));
        assert!(close(picture_uv(transform, (1.0, 1.0)), (1.0, 1.0)));
    }

    #[test]
    fn a_mirrored_camera_gets_a_flipped_picture_over_the_same_window() {
        let plain = cover_transform((1280, 720), (2560, 720), false);
        let mirrored = cover_transform((1280, 720), (2560, 720), true);
        for u in [0.0, 0.25, 0.5, 1.0] {
            let flipped = picture_uv(mirrored, (u, 0.3));
            let straight = picture_uv(plain, (1.0 - u, 0.3));
            assert!(
                close(flipped, straight),
                "u {u}: {flipped:?} vs {straight:?}"
            );
        }
    }

    #[test]
    fn degenerate_sizes_do_not_divide_by_zero() {
        for transform in [
            cover_transform((0, 0), (100, 100), false),
            cover_transform((100, 100), (0, 0), true),
        ] {
            assert!(transform.iter().all(|value| value.is_finite()));
        }
    }

    #[test]
    fn pictures_must_hold_every_pixel() {
        assert!(BackgroundPicture::new(vec![0; 16], 2, 2).is_some());
        assert!(BackgroundPicture::new(vec![0; 15], 2, 2).is_none());
        assert!(BackgroundPicture::new(Vec::new(), 0, 0).is_none());
        assert!(BackgroundPicture::new(vec![0; 4], u32::MAX, u32::MAX).is_none());
    }

    #[test]
    fn the_picture_shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(&shader_source()).expect("parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("validates");
        assert!(
            module
                .entry_points
                .iter()
                .any(|entry| entry.name == "fs_picture")
        );
    }
}
