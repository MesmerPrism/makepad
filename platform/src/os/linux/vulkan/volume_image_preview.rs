use crate::{
    cx_api::{
        XrGpuF32VolumeImagePreviewOutput, XrGpuF32VolumeImagePreviewPixel,
        XrGpuF32VolumeImagePreviewResult, XrGpuF32VolumeImagePreviewTextureAdoption,
        XrGpuF32VolumeImagePreviewTicket, XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS,
    },
    os::linux::vulkan_naga::compile_compute_wgsl_to_spirv,
    texture::TextureId,
};
use ash::vk;
use std::time::Instant;

use super::{CxVulkan, VulkanBuffer, VulkanTextureResource};

const XR_GPU_F32_VOLUME_IMAGE_PREVIEW_ENTRY: &str = "compute_main";
const XR_GPU_F32_VOLUME_IMAGE_PREVIEW_SAMPLE_ENTRY: &str = "sample_main";
const XR_GPU_F32_VOLUME_IMAGE_PREVIEW_WORKGROUP: u32 = 8;
const XR_GPU_F32_VOLUME_IMAGE_PREVIEW_EYE_COUNT: usize = 2;
const XR_GPU_F32_VOLUME_IMAGE_PREVIEW_IMAGE_LAYERS: usize = 1;
const XR_GPU_F32_VOLUME_IMAGE_PREVIEW_EYE_TILE_MIN: usize = 4;
const XR_GPU_F32_VOLUME_IMAGE_PREVIEW_EYE_TILE_MAX: usize = 256;
const XR_GPU_F32_VOLUME_IMAGE_PREVIEW_FORMAT: vk::Format = vk::Format::R32G32B32A32_SFLOAT;
const XR_GPU_F32_VOLUME_IMAGE_PREVIEW_WGSL: &str = r#"
struct VolumeImagePreviewPixel {
    uv_eye_time: vec4<f32>,
    ray_origin: vec4<f32>,
    ray_direction_step: vec4<f32>,
    volume_params: vec4<f32>,
    expected_rgba: vec4<f32>,
    expected_density_depth_status: vec4<f32>,
};

struct VolumeImagePreviewOutput {
    rgba: vec4<f32>,
    density_depth_status: vec4<f32>,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<VolumeImagePreviewPixel, 32>;
@group(0) @binding(1) var output_image: texture_storage_2d<rgba32float, write>;

fn triangle_wave(value: f32) -> f32 {
    return abs(fract(value) * 2.0 - 1.0);
}

fn volume_density(p: vec3<f32>, uv: vec4<f32>, params: vec4<f32>) -> f32 {
    let frequency = max(params.x, 0.001);
    let phase = params.y;
    let opacity = clamp(params.z, 0.0, 4.0);
    let wave_a =
        triangle_wave((p.x + uv.x * 0.25 + p.z * 0.5) * frequency + uv.w * 0.07 + phase);
    let wave_b =
        triangle_wave((p.y - p.z * 0.35 + uv.y * 0.25) * frequency * 0.75 - uv.w * 0.11 + phase * 0.5);
    let interference = clamp(1.0 - abs(wave_a - wave_b), 0.0, 1.0);
    return clamp(interference * opacity, 0.0, 1.0);
}

fn volume_image_preview_output(pixel: VolumeImagePreviewPixel) -> VolumeImagePreviewOutput {
    let uv = pixel.uv_eye_time;
    let origin = pixel.ray_origin.xyz;
    let direction = pixel.ray_direction_step.xyz;
    let step_count = clamp(pixel.ray_direction_step.w, 1.0, 32.0);
    let step_alpha_scale = clamp(pixel.volume_params.w, 0.001, 4.0);
    var accum_rgb = vec3<f32>(0.0, 0.0, 0.0);
    var accum_alpha = 0.0;
    var first_depth = 0.0;
    var hit = 0.0;

    for (var step = 0u; step < 32u; step = step + 1u) {
        let step_f = f32(step);
        if (step_f < step_count) {
            let unit_depth = (step_f + 0.5) / step_count;
            let p = origin + direction * unit_depth;
            let density = volume_density(p, uv, pixel.volume_params);
            let sample_alpha = clamp(density * step_alpha_scale / step_count, 0.0, 1.0);
            let sample_rgb = vec3<f32>(density, density, density);
            let contribution = (1.0 - accum_alpha) * sample_alpha;
            accum_rgb = accum_rgb + sample_rgb * contribution;
            if (hit < 0.5 && density > 0.05) {
                first_depth = unit_depth;
                hit = 1.0;
            }
            accum_alpha = clamp(accum_alpha + contribution, 0.0, 1.0);
        }
    }

    return VolumeImagePreviewOutput(
        vec4<f32>(accum_rgb, accum_alpha),
        vec4<f32>(accum_alpha, first_depth, hit, step_count)
    );
}

fn volume_image_preview_pixel_from_uv(
    surface_uv: vec2<f32>,
    eye_index: f32,
    source_pixel: VolumeImagePreviewPixel
) -> VolumeImagePreviewOutput {
    let eye_offset = (eye_index - 0.5) * 0.08;
    let origin = vec3<f32>(surface_uv.x - 0.5 + eye_offset, surface_uv.y - 0.5, -0.72);
    let direction = vec3<f32>(
        (surface_uv.x - 0.5) * 0.42 + eye_offset * 0.25,
        (surface_uv.y - 0.5) * 0.32,
        1.0
    );
    let pixel = VolumeImagePreviewPixel(
        vec4<f32>(surface_uv.x, surface_uv.y, eye_index, source_pixel.uv_eye_time.w),
        vec4<f32>(origin, 0.0),
        vec4<f32>(direction, source_pixel.ray_direction_step.w),
        source_pixel.volume_params,
        vec4<f32>(0.0, 0.0, 0.0, 0.0),
        vec4<f32>(0.0, 0.0, 0.0, 0.0)
    );
    return volume_image_preview_output(pixel);
}

@compute @workgroup_size(8, 8, 1)
fn compute_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_image);
    if (id.x >= dims.x || id.y >= dims.y) {
        return;
    }
    let eye_count = 2u;
    let tile_width = max(dims.x / eye_count, 1u);
    let eye_index = min(id.x / tile_width, eye_count - 1u);
    let local_x = id.x - eye_index * tile_width;
    let surface_uv = vec2<f32>(
        (f32(local_x) + 0.5) / f32(tile_width),
        (f32(id.y) + 0.5) / f32(max(dims.y, 1u))
    );
    let output = volume_image_preview_pixel_from_uv(surface_uv, f32(eye_index), input_pixels[0]);
    textureStore(output_image, vec2<i32>(i32(id.x), i32(id.y)), output.rgba);
}
"#;
const XR_GPU_F32_VOLUME_IMAGE_PREVIEW_SAMPLE_WGSL: &str = r#"
struct VolumeTextureSampleOutput {
    rgba: vec4<f32>,
};

@group(0) @binding(0) var sampled_image: texture_2d<f32>;
@group(0) @binding(1) var sampled_image_sampler: sampler;
@group(0) @binding(2) var<storage, read_write> sampled_outputs: array<VolumeTextureSampleOutput, 32>;

@compute @workgroup_size(8)
fn sample_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if (index < 32u) {
        let dims = textureDimensions(sampled_image);
        let eye_count = 2u;
        let sample_grid_width = 4u;
        let sample_grid_height = 4u;
        let tile_width = max(dims.x / eye_count, 1u);
        let tile_height = max(dims.y, 1u);
        let samples_per_eye = sample_grid_width * sample_grid_height;
        let eye_index = min(index / samples_per_eye, eye_count - 1u);
        let local_index = index % samples_per_eye;
        let sample_x = local_index % sample_grid_width;
        let sample_y = local_index / sample_grid_width;
        let pixel_x = min((sample_x * tile_width) / sample_grid_width + tile_width / (sample_grid_width * 2u), tile_width - 1u);
        let pixel_y = min((sample_y * tile_height) / sample_grid_height + tile_height / (sample_grid_height * 2u), tile_height - 1u);
        let atlas_x = eye_index * tile_width + pixel_x;
        let uv = (vec2<f32>(f32(atlas_x), f32(pixel_y)) + vec2<f32>(0.5, 0.5))
            / vec2<f32>(f32(max(dims.x, 1u)), f32(max(dims.y, 1u)));
        sampled_outputs[index] = VolumeTextureSampleOutput(
            textureSampleLevel(sampled_image, sampled_image_sampler, uv, 0.0)
        );
    }
}
"#;

#[derive(Clone, Copy)]
struct VulkanXrF32VolumeImagePreviewImage {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    width: u32,
    height: u32,
    layers: u32,
}

pub(super) struct VulkanXrF32VolumeImagePreviewResources {
    request_id: u64,
    started: Instant,
    pixels: [XrGpuF32VolumeImagePreviewPixel; XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS],
    expected_outputs: [XrGpuF32VolumeImagePreviewOutput; XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS],
    image_width: usize,
    image_height: usize,
    image_layers: usize,
    eye_tile_width: usize,
    eye_tile_height: usize,
    eye_count: usize,
    pixel_count: usize,
    tolerance: f32,
    queue_submit_serial: u64,
    resource_generation: u64,
    completed: bool,
    input: VulkanBuffer,
    image: VulkanXrF32VolumeImagePreviewImage,
    readback: VulkanBuffer,
    sampled_readback: VulkanBuffer,
    sampler: vk::Sampler,
    shader_module: vk::ShaderModule,
    sample_shader_module: vk::ShaderModule,
    descriptor_set_layout: vk::DescriptorSetLayout,
    sample_descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    sample_pipeline_layout: vk::PipelineLayout,
    compute_pipeline: vk::Pipeline,
    sample_compute_pipeline: vk::Pipeline,
    descriptor_pool: vk::DescriptorPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
}

fn expected_xr_gpu_f32_volume_image_preview_outputs(
    pixels: [XrGpuF32VolumeImagePreviewPixel; XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS],
) -> [XrGpuF32VolumeImagePreviewOutput; XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS] {
    let mut expected =
        [XrGpuF32VolumeImagePreviewOutput::default(); XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS];
    for (index, pixel) in pixels.iter().copied().enumerate() {
        expected[index] = XrGpuF32VolumeImagePreviewOutput {
            rgba: pixel.expected_rgba,
        };
    }
    expected
}

impl CxVulkan {
    pub(crate) fn submit_xr_f32_volume_image_preview(
        &mut self,
        pixels: [XrGpuF32VolumeImagePreviewPixel; XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS],
        eye_tile_width: usize,
        eye_tile_height: usize,
        eye_count: usize,
        pixel_count: usize,
        tolerance: f32,
    ) -> Result<XrGpuF32VolumeImagePreviewResult, String> {
        let ticket = self.submit_xr_f32_volume_image_preview_async(
            pixels,
            eye_tile_width,
            eye_tile_height,
            eye_count,
            pixel_count,
            tolerance,
        )?;
        self.wait_xr_f32_volume_image_preview(ticket.request_id)
    }

    pub(crate) fn submit_xr_f32_volume_image_preview_async(
        &mut self,
        pixels: [XrGpuF32VolumeImagePreviewPixel; XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS],
        eye_tile_width: usize,
        eye_tile_height: usize,
        eye_count: usize,
        pixel_count: usize,
        tolerance: f32,
    ) -> Result<XrGpuF32VolumeImagePreviewTicket, String> {
        if eye_count != XR_GPU_F32_VOLUME_IMAGE_PREVIEW_EYE_COUNT {
            return Err(format!(
                "f32 volume image preview currently requires {} stereo eyes",
                XR_GPU_F32_VOLUME_IMAGE_PREVIEW_EYE_COUNT
            ));
        }
        if !(XR_GPU_F32_VOLUME_IMAGE_PREVIEW_EYE_TILE_MIN
            ..=XR_GPU_F32_VOLUME_IMAGE_PREVIEW_EYE_TILE_MAX)
            .contains(&eye_tile_width)
            || !(XR_GPU_F32_VOLUME_IMAGE_PREVIEW_EYE_TILE_MIN
                ..=XR_GPU_F32_VOLUME_IMAGE_PREVIEW_EYE_TILE_MAX)
                .contains(&eye_tile_height)
        {
            return Err(format!(
                "f32 volume image preview eye tiles must be within {}..={} pixels",
                XR_GPU_F32_VOLUME_IMAGE_PREVIEW_EYE_TILE_MIN,
                XR_GPU_F32_VOLUME_IMAGE_PREVIEW_EYE_TILE_MAX
            ));
        }

        let started = Instant::now();
        let pixel_count = pixel_count.min(XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS);
        if pixel_count == 0 {
            return Err("f32 volume image preview requires at least one sample pixel".to_string());
        }
        let image_width = eye_tile_width
            .checked_mul(eye_count)
            .ok_or_else(|| "f32 volume image preview width overflow".to_string())?;
        let image_height = eye_tile_height;
        let tolerance = if tolerance.is_finite() && tolerance >= 0.0 {
            tolerance
        } else {
            0.0
        };
        let expected_outputs = expected_xr_gpu_f32_volume_image_preview_outputs(pixels);
        let input_byte_len = std::mem::size_of::<
            [XrGpuF32VolumeImagePreviewPixel; XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS],
        >() as vk::DeviceSize;
        let readback_byte_len = std::mem::size_of::<
            [XrGpuF32VolumeImagePreviewOutput; XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS],
        >() as vk::DeviceSize;
        let image_readback_byte_len = (image_width as vk::DeviceSize)
            .saturating_mul(image_height as vk::DeviceSize)
            .saturating_mul(
                std::mem::size_of::<XrGpuF32VolumeImagePreviewOutput>() as vk::DeviceSize
            );

        let shader_spv = compile_compute_wgsl_to_spirv(
            XR_GPU_F32_VOLUME_IMAGE_PREVIEW_WGSL,
            XR_GPU_F32_VOLUME_IMAGE_PREVIEW_ENTRY,
        )?;
        let sample_shader_spv = compile_compute_wgsl_to_spirv(
            XR_GPU_F32_VOLUME_IMAGE_PREVIEW_SAMPLE_WGSL,
            XR_GPU_F32_VOLUME_IMAGE_PREVIEW_SAMPLE_ENTRY,
        )?;
        let input =
            self.create_host_buffer_with_data(vk::BufferUsageFlags::STORAGE_BUFFER, &pixels)?;
        let image = match self
            .create_xr_f32_volume_image_preview_image(image_width as u32, image_height as u32)
        {
            Ok(image) => image,
            Err(err) => {
                self.destroy_buffer(input);
                return Err(err);
            }
        };
        let readback = match self
            .create_host_buffer(vk::BufferUsageFlags::TRANSFER_DST, image_readback_byte_len)
        {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_xr_f32_volume_image_preview_image(image);
                self.destroy_buffer(input);
                return Err(err);
            }
        };
        let sampled_readback = match self
            .create_host_buffer(vk::BufferUsageFlags::STORAGE_BUFFER, readback_byte_len)
        {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_buffer(readback);
                self.destroy_xr_f32_volume_image_preview_image(image);
                self.destroy_buffer(input);
                return Err(err);
            }
        };
        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::NEAREST)
            .min_filter(vk::Filter::NEAREST)
            .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .min_lod(0.0)
            .max_lod(0.0);
        let sampler = match unsafe { self.device.create_sampler(&sampler_info, None) } {
            Ok(sampler) => sampler,
            Err(err) => {
                self.destroy_buffer(sampled_readback);
                self.destroy_buffer(readback);
                self.destroy_xr_f32_volume_image_preview_image(image);
                self.destroy_buffer(input);
                return Err(format!(
                    "create_sampler(f32 volume image preview sample pass) failed: {err:?}"
                ));
            }
        };

        let shader_module_info = vk::ShaderModuleCreateInfo::default().code(&shader_spv);
        let shader_module =
            match unsafe { self.device.create_shader_module(&shader_module_info, None) } {
                Ok(shader_module) => shader_module,
                Err(err) => {
                    unsafe {
                        self.device.destroy_sampler(sampler, None);
                    }
                    self.destroy_buffer(sampled_readback);
                    self.destroy_buffer(readback);
                    self.destroy_xr_f32_volume_image_preview_image(image);
                    self.destroy_buffer(input);
                    return Err(format!(
                        "create_shader_module(f32 volume image preview) failed: {err:?}"
                    ));
                }
            };
        let sample_shader_module_info =
            vk::ShaderModuleCreateInfo::default().code(&sample_shader_spv);
        let sample_shader_module = match unsafe {
            self.device
                .create_shader_module(&sample_shader_module_info, None)
        } {
            Ok(shader_module) => shader_module,
            Err(err) => {
                unsafe {
                    self.device.destroy_shader_module(shader_module, None);
                    self.device.destroy_sampler(sampler, None);
                }
                self.destroy_buffer(sampled_readback);
                self.destroy_buffer(readback);
                self.destroy_xr_f32_volume_image_preview_image(image);
                self.destroy_buffer(input);
                return Err(format!(
                    "create_shader_module(f32 volume image preview sample pass) failed: {err:?}"
                ));
            }
        };

        let descriptor_bindings = [
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
            vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
        ];
        let descriptor_set_layout_info =
            vk::DescriptorSetLayoutCreateInfo::default().bindings(&descriptor_bindings);
        let descriptor_set_layout = match unsafe {
            self.device
                .create_descriptor_set_layout(&descriptor_set_layout_info, None)
        } {
            Ok(layout) => layout,
            Err(err) => {
                unsafe {
                    self.device
                        .destroy_shader_module(sample_shader_module, None);
                    self.device.destroy_shader_module(shader_module, None);
                    self.device.destroy_sampler(sampler, None);
                }
                self.destroy_buffer(sampled_readback);
                self.destroy_buffer(readback);
                self.destroy_xr_f32_volume_image_preview_image(image);
                self.destroy_buffer(input);
                return Err(format!(
                    "create_descriptor_set_layout(f32 volume image preview) failed: {err:?}"
                ));
            }
        };
        let sample_descriptor_bindings = [
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
            vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
            vk::DescriptorSetLayoutBinding::default()
                .binding(2)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
        ];
        let sample_descriptor_set_layout_info =
            vk::DescriptorSetLayoutCreateInfo::default().bindings(&sample_descriptor_bindings);
        let sample_descriptor_set_layout = match unsafe {
            self.device
                .create_descriptor_set_layout(&sample_descriptor_set_layout_info, None)
        } {
            Ok(layout) => layout,
            Err(err) => {
                unsafe {
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    self.device
                        .destroy_shader_module(sample_shader_module, None);
                    self.device.destroy_shader_module(shader_module, None);
                    self.device.destroy_sampler(sampler, None);
                }
                self.destroy_buffer(sampled_readback);
                self.destroy_buffer(readback);
                self.destroy_xr_f32_volume_image_preview_image(image);
                self.destroy_buffer(input);
                return Err(format!(
                    "create_descriptor_set_layout(f32 volume image preview sample pass) failed: {err:?}"
                ));
            }
        };

        let set_layouts = [descriptor_set_layout];
        let pipeline_layout_info =
            vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts);
        let pipeline_layout = match unsafe {
            self.device
                .create_pipeline_layout(&pipeline_layout_info, None)
        } {
            Ok(layout) => layout,
            Err(err) => {
                unsafe {
                    self.device
                        .destroy_descriptor_set_layout(sample_descriptor_set_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    self.device
                        .destroy_shader_module(sample_shader_module, None);
                    self.device.destroy_shader_module(shader_module, None);
                    self.device.destroy_sampler(sampler, None);
                }
                self.destroy_buffer(sampled_readback);
                self.destroy_buffer(readback);
                self.destroy_xr_f32_volume_image_preview_image(image);
                self.destroy_buffer(input);
                return Err(format!(
                    "create_pipeline_layout(f32 volume image preview) failed: {err:?}"
                ));
            }
        };
        let sample_set_layouts = [sample_descriptor_set_layout];
        let sample_pipeline_layout_info =
            vk::PipelineLayoutCreateInfo::default().set_layouts(&sample_set_layouts);
        let sample_pipeline_layout = match unsafe {
            self.device
                .create_pipeline_layout(&sample_pipeline_layout_info, None)
        } {
            Ok(layout) => layout,
            Err(err) => {
                unsafe {
                    self.device.destroy_pipeline_layout(pipeline_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(sample_descriptor_set_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    self.device
                        .destroy_shader_module(sample_shader_module, None);
                    self.device.destroy_shader_module(shader_module, None);
                    self.device.destroy_sampler(sampler, None);
                }
                self.destroy_buffer(sampled_readback);
                self.destroy_buffer(readback);
                self.destroy_xr_f32_volume_image_preview_image(image);
                self.destroy_buffer(input);
                return Err(format!(
                    "create_pipeline_layout(f32 volume image preview sample pass) failed: {err:?}"
                ));
            }
        };

        let entry = std::ffi::CString::new(XR_GPU_F32_VOLUME_IMAGE_PREVIEW_ENTRY).unwrap();
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(shader_module)
            .name(&entry);
        let compute_pipeline_info = vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(pipeline_layout);
        let compute_pipeline = match unsafe {
            self.device.create_compute_pipelines(
                vk::PipelineCache::null(),
                &[compute_pipeline_info],
                None,
            )
        } {
            Ok(mut pipelines) => pipelines.remove(0),
            Err((pipelines, err)) => {
                unsafe {
                    for pipeline in pipelines {
                        if pipeline != vk::Pipeline::null() {
                            self.device.destroy_pipeline(pipeline, None);
                        }
                    }
                    self.device
                        .destroy_pipeline_layout(sample_pipeline_layout, None);
                    self.device.destroy_pipeline_layout(pipeline_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(sample_descriptor_set_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    self.device
                        .destroy_shader_module(sample_shader_module, None);
                    self.device.destroy_shader_module(shader_module, None);
                    self.device.destroy_sampler(sampler, None);
                }
                self.destroy_buffer(sampled_readback);
                self.destroy_buffer(readback);
                self.destroy_xr_f32_volume_image_preview_image(image);
                self.destroy_buffer(input);
                return Err(format!(
                    "create_compute_pipelines(f32 volume image preview) failed: {err:?}"
                ));
            }
        };
        let sample_entry =
            std::ffi::CString::new(XR_GPU_F32_VOLUME_IMAGE_PREVIEW_SAMPLE_ENTRY).unwrap();
        let sample_stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(sample_shader_module)
            .name(&sample_entry);
        let sample_compute_pipeline_info = vk::ComputePipelineCreateInfo::default()
            .stage(sample_stage)
            .layout(sample_pipeline_layout);
        let sample_compute_pipeline = match unsafe {
            self.device.create_compute_pipelines(
                vk::PipelineCache::null(),
                &[sample_compute_pipeline_info],
                None,
            )
        } {
            Ok(mut pipelines) => pipelines.remove(0),
            Err((pipelines, err)) => {
                unsafe {
                    for pipeline in pipelines {
                        if pipeline != vk::Pipeline::null() {
                            self.device.destroy_pipeline(pipeline, None);
                        }
                    }
                    self.device.destroy_pipeline(compute_pipeline, None);
                    self.device
                        .destroy_pipeline_layout(sample_pipeline_layout, None);
                    self.device.destroy_pipeline_layout(pipeline_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(sample_descriptor_set_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    self.device
                        .destroy_shader_module(sample_shader_module, None);
                    self.device.destroy_shader_module(shader_module, None);
                    self.device.destroy_sampler(sampler, None);
                }
                self.destroy_buffer(sampled_readback);
                self.destroy_buffer(readback);
                self.destroy_xr_f32_volume_image_preview_image(image);
                self.destroy_buffer(input);
                return Err(format!(
                    "create_compute_pipelines(f32 volume image preview sample pass) failed: {err:?}"
                ));
            }
        };

        let descriptor_pool_sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_BUFFER,
                descriptor_count: 2,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_IMAGE,
                descriptor_count: 1,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLED_IMAGE,
                descriptor_count: 1,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLER,
                descriptor_count: 1,
            },
        ];
        let descriptor_pool_info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(2)
            .pool_sizes(&descriptor_pool_sizes);
        let descriptor_pool = match unsafe {
            self.device
                .create_descriptor_pool(&descriptor_pool_info, None)
        } {
            Ok(pool) => pool,
            Err(err) => {
                self.destroy_xr_f32_volume_image_preview_gpu_handles(
                    vk::Fence::null(),
                    vk::CommandBuffer::null(),
                    vk::DescriptorPool::null(),
                    compute_pipeline,
                    pipeline_layout,
                    descriptor_set_layout,
                    shader_module,
                    sample_compute_pipeline,
                    sample_pipeline_layout,
                    sample_descriptor_set_layout,
                    sample_shader_module,
                    sampler,
                );
                self.destroy_buffer(sampled_readback);
                self.destroy_buffer(readback);
                self.destroy_xr_f32_volume_image_preview_image(image);
                self.destroy_buffer(input);
                return Err(format!(
                    "create_descriptor_pool(f32 volume image preview) failed: {err:?}"
                ));
            }
        };
        let (descriptor_set, sample_descriptor_set) = {
            let allocation_set_layouts = [descriptor_set_layout, sample_descriptor_set_layout];
            let alloc_info = vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(descriptor_pool)
                .set_layouts(&allocation_set_layouts);
            match unsafe { self.device.allocate_descriptor_sets(&alloc_info) } {
                Ok(sets) => (sets[0], sets[1]),
                Err(err) => {
                    self.destroy_xr_f32_volume_image_preview_gpu_handles(
                        vk::Fence::null(),
                        vk::CommandBuffer::null(),
                        descriptor_pool,
                        compute_pipeline,
                        pipeline_layout,
                        descriptor_set_layout,
                        shader_module,
                        sample_compute_pipeline,
                        sample_pipeline_layout,
                        sample_descriptor_set_layout,
                        sample_shader_module,
                        sampler,
                    );
                    self.destroy_buffer(sampled_readback);
                    self.destroy_buffer(readback);
                    self.destroy_xr_f32_volume_image_preview_image(image);
                    self.destroy_buffer(input);
                    return Err(format!(
                        "allocate_descriptor_sets(f32 volume image preview) failed: {err:?}"
                    ));
                }
            }
        };

        let input_buffer_info = vk::DescriptorBufferInfo::default()
            .buffer(input.buffer)
            .offset(0)
            .range(input_byte_len);
        let sampled_output_buffer_info = vk::DescriptorBufferInfo::default()
            .buffer(sampled_readback.buffer)
            .offset(0)
            .range(readback_byte_len);
        let image_info = vk::DescriptorImageInfo::default()
            .image_view(image.view)
            .image_layout(vk::ImageLayout::GENERAL);
        let sampled_image_info = vk::DescriptorImageInfo::default()
            .image_view(image.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        let sampler_info = vk::DescriptorImageInfo::default().sampler(sampler);
        let input_buffer_infos = [input_buffer_info];
        let sampled_output_buffer_infos = [sampled_output_buffer_info];
        let image_infos = [image_info];
        let sampled_image_infos = [sampled_image_info];
        let sampler_infos = [sampler_info];
        let descriptor_writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&input_buffer_infos),
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&image_infos),
            vk::WriteDescriptorSet::default()
                .dst_set(sample_descriptor_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&sampled_image_infos),
            vk::WriteDescriptorSet::default()
                .dst_set(sample_descriptor_set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .image_info(&sampler_infos),
            vk::WriteDescriptorSet::default()
                .dst_set(sample_descriptor_set)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&sampled_output_buffer_infos),
        ];
        unsafe {
            self.device.update_descriptor_sets(&descriptor_writes, &[]);
        }

        let command_buffer = {
            let alloc_info = vk::CommandBufferAllocateInfo::default()
                .command_pool(self.command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            match unsafe { self.device.allocate_command_buffers(&alloc_info) } {
                Ok(buffers) => buffers[0],
                Err(err) => {
                    self.destroy_xr_f32_volume_image_preview_gpu_handles(
                        vk::Fence::null(),
                        vk::CommandBuffer::null(),
                        descriptor_pool,
                        compute_pipeline,
                        pipeline_layout,
                        descriptor_set_layout,
                        shader_module,
                        sample_compute_pipeline,
                        sample_pipeline_layout,
                        sample_descriptor_set_layout,
                        sample_shader_module,
                        sampler,
                    );
                    self.destroy_buffer(sampled_readback);
                    self.destroy_buffer(readback);
                    self.destroy_xr_f32_volume_image_preview_image(image);
                    self.destroy_buffer(input);
                    return Err(format!(
                        "allocate_command_buffers(f32 volume image preview) failed: {err:?}"
                    ));
                }
            }
        };
        let fence = {
            let fence_info = vk::FenceCreateInfo::default();
            match unsafe { self.device.create_fence(&fence_info, None) } {
                Ok(fence) => fence,
                Err(err) => {
                    self.destroy_xr_f32_volume_image_preview_gpu_handles(
                        vk::Fence::null(),
                        command_buffer,
                        descriptor_pool,
                        compute_pipeline,
                        pipeline_layout,
                        descriptor_set_layout,
                        shader_module,
                        sample_compute_pipeline,
                        sample_pipeline_layout,
                        sample_descriptor_set_layout,
                        sample_shader_module,
                        sampler,
                    );
                    self.destroy_buffer(sampled_readback);
                    self.destroy_buffer(readback);
                    self.destroy_xr_f32_volume_image_preview_image(image);
                    self.destroy_buffer(input);
                    return Err(format!(
                        "create_fence(f32 volume image preview) failed: {err:?}"
                    ));
                }
            }
        };

        let mut queue_submit_serial = 0;
        let command_result = (|| -> Result<(), String> {
            unsafe {
                self.device
                    .begin_command_buffer(
                        command_buffer,
                        &vk::CommandBufferBeginInfo::default()
                            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                    )
                    .map_err(|e| {
                        format!("begin_command_buffer(f32 volume image preview) failed: {e:?}")
                    })?;

                let input_barrier = vk::BufferMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::HOST_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(input.buffer)
                    .offset(0)
                    .size(input_byte_len);
                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::HOST,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[input_barrier],
                    &[],
                );

                let image_to_general = vk::ImageMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::empty())
                    .dst_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image.image)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .base_mip_level(0)
                            .level_count(1)
                            .base_array_layer(0)
                            .layer_count(image.layers),
                    );
                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[image_to_general],
                );

                self.device.cmd_bind_pipeline(
                    command_buffer,
                    vk::PipelineBindPoint::COMPUTE,
                    compute_pipeline,
                );
                self.device.cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::COMPUTE,
                    pipeline_layout,
                    0,
                    &[descriptor_set],
                    &[],
                );
                let dispatch_x = (image.width + XR_GPU_F32_VOLUME_IMAGE_PREVIEW_WORKGROUP - 1)
                    / XR_GPU_F32_VOLUME_IMAGE_PREVIEW_WORKGROUP;
                let dispatch_y = (image.height + XR_GPU_F32_VOLUME_IMAGE_PREVIEW_WORKGROUP - 1)
                    / XR_GPU_F32_VOLUME_IMAGE_PREVIEW_WORKGROUP;
                self.device
                    .cmd_dispatch(command_buffer, dispatch_x, dispatch_y, 1);

                let sample_dispatch_x = ((XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS as u32)
                    + XR_GPU_F32_VOLUME_IMAGE_PREVIEW_WORKGROUP
                    - 1)
                    / XR_GPU_F32_VOLUME_IMAGE_PREVIEW_WORKGROUP;

                let image_to_transfer = vk::ImageMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image.image)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .base_mip_level(0)
                            .level_count(1)
                            .base_array_layer(0)
                            .layer_count(image.layers),
                    );
                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[image_to_transfer],
                );

                let copy_region = vk::BufferImageCopy::default()
                    .buffer_offset(0)
                    .buffer_row_length(0)
                    .buffer_image_height(0)
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .mip_level(0)
                            .base_array_layer(0)
                            .layer_count(image.layers),
                    )
                    .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                    .image_extent(vk::Extent3D {
                        width: image.width,
                        height: image.height,
                        depth: 1,
                    });
                self.device.cmd_copy_image_to_buffer(
                    command_buffer,
                    image.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    readback.buffer,
                    &[copy_region],
                );

                let readback_barrier = vk::BufferMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::HOST_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(readback.buffer)
                    .offset(0)
                    .size(image_readback_byte_len);
                let image_to_shader = vk::ImageMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ)
                    .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image.image)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .base_mip_level(0)
                            .level_count(1)
                            .base_array_layer(0)
                            .layer_count(image.layers),
                    );
                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::HOST
                        | vk::PipelineStageFlags::FRAGMENT_SHADER
                        | vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[readback_barrier],
                    &[image_to_shader],
                );

                self.device.cmd_bind_pipeline(
                    command_buffer,
                    vk::PipelineBindPoint::COMPUTE,
                    sample_compute_pipeline,
                );
                self.device.cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::COMPUTE,
                    sample_pipeline_layout,
                    0,
                    &[sample_descriptor_set],
                    &[],
                );
                self.device
                    .cmd_dispatch(command_buffer, sample_dispatch_x, 1, 1);

                let sampled_readback_barrier = vk::BufferMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .dst_access_mask(vk::AccessFlags::HOST_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(sampled_readback.buffer)
                    .offset(0)
                    .size(readback_byte_len);
                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::PipelineStageFlags::HOST,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[sampled_readback_barrier],
                    &[],
                );

                self.device
                    .end_command_buffer(command_buffer)
                    .map_err(|e| {
                        format!("end_command_buffer(f32 volume image preview) failed: {e:?}")
                    })?;
                self.device
                    .queue_submit(
                        self.queue,
                        &[vk::SubmitInfo::default().command_buffers(&[command_buffer])],
                        fence,
                    )
                    .map_err(|e| format!("queue_submit(f32 volume image preview) failed: {e:?}"))?;
                self.gpu_submit_serial = self.gpu_submit_serial.saturating_add(1);
                queue_submit_serial = self.gpu_submit_serial;
            }
            Ok(())
        })();
        if let Err(err) = command_result {
            self.destroy_xr_f32_volume_image_preview_gpu_handles(
                fence,
                command_buffer,
                descriptor_pool,
                compute_pipeline,
                pipeline_layout,
                descriptor_set_layout,
                shader_module,
                sample_compute_pipeline,
                sample_pipeline_layout,
                sample_descriptor_set_layout,
                sample_shader_module,
                sampler,
            );
            self.destroy_buffer(sampled_readback);
            self.destroy_buffer(readback);
            self.destroy_xr_f32_volume_image_preview_image(image);
            self.destroy_buffer(input);
            return Err(err);
        }

        let resource_generation = self.xr_f32_volume_image_preview_resources.len() as u64 + 1;
        let request_id = queue_submit_serial;
        self.xr_f32_volume_image_preview_resources
            .push(VulkanXrF32VolumeImagePreviewResources {
                request_id,
                started,
                pixels,
                expected_outputs,
                image_width,
                image_height,
                image_layers: XR_GPU_F32_VOLUME_IMAGE_PREVIEW_IMAGE_LAYERS,
                eye_tile_width,
                eye_tile_height,
                eye_count,
                pixel_count,
                tolerance,
                queue_submit_serial,
                resource_generation,
                completed: false,
                input,
                image,
                readback,
                sampled_readback,
                sampler,
                shader_module,
                sample_shader_module,
                descriptor_set_layout,
                sample_descriptor_set_layout,
                pipeline_layout,
                sample_pipeline_layout,
                compute_pipeline,
                sample_compute_pipeline,
                descriptor_pool,
                command_buffer,
                fence,
            });
        let retained_resource_count = self.xr_f32_volume_image_preview_resources.len();
        let pending_retire_count = retained_resource_count;

        Ok(XrGpuF32VolumeImagePreviewTicket {
            request_id,
            queue_submit_serial,
            resource_generation,
            pending_retire_count,
            retained_resource_count,
        })
    }

    pub(crate) fn poll_xr_f32_volume_image_preview(
        &mut self,
        request_id: u64,
    ) -> Result<Option<XrGpuF32VolumeImagePreviewResult>, String> {
        let Some(resource_index) = self
            .xr_f32_volume_image_preview_resources
            .iter()
            .position(|resource| resource.request_id == request_id)
        else {
            return Ok(None);
        };
        if self.xr_f32_volume_image_preview_resources[resource_index].completed {
            return Ok(None);
        }

        let fence = self.xr_f32_volume_image_preview_resources[resource_index].fence;
        let queue_submit_serial =
            self.xr_f32_volume_image_preview_resources[resource_index].queue_submit_serial;
        match unsafe { self.device.get_fence_status(fence) } {
            Ok(true) => {
                self.gpu_completed_submit_serial =
                    self.gpu_completed_submit_serial.max(queue_submit_serial);
                self.collect_retired_texture_resources();
                self.complete_xr_f32_volume_image_preview(resource_index, false)
                    .map(Some)
            }
            Ok(false) => Ok(None),
            Err(err) => Err(format!(
                "get_fence_status(f32 volume image preview {request_id}) failed: {err:?}"
            )),
        }
    }

    pub(crate) fn adopt_xr_f32_volume_image_preview_texture(
        &mut self,
        request_id: u64,
        texture_id: TextureId,
    ) -> Result<XrGpuF32VolumeImagePreviewTextureAdoption, String> {
        let resource_index = self
            .xr_f32_volume_image_preview_resources
            .iter()
            .position(|resource| resource.request_id == request_id)
            .ok_or_else(|| format!("f32 volume image preview {request_id} was not found"))?;
        let texture_key = Self::texture_key(texture_id);
        let (
            image_width,
            image_height,
            image_layers,
            queue_submit_serial,
            resource_generation,
            image,
        ) = {
            let resource = self
                .xr_f32_volume_image_preview_resources
                .get_mut(resource_index)
                .ok_or_else(|| "f32 volume image preview resource index is stale".to_string())?;
            if !resource.completed {
                return Err(format!(
                    "f32 volume image preview {request_id} is not complete"
                ));
            }
            if resource.image.image == vk::Image::null() {
                return Err(format!(
                    "f32 volume image preview {request_id} image was already adopted"
                ));
            }
            let empty_image = VulkanXrF32VolumeImagePreviewImage {
                image: vk::Image::null(),
                memory: vk::DeviceMemory::null(),
                view: vk::ImageView::null(),
                width: resource.image.width,
                height: resource.image.height,
                layers: resource.image.layers,
            };
            (
                resource.image_width,
                resource.image_height,
                resource.image_layers,
                resource.queue_submit_serial,
                resource.resource_generation,
                std::mem::replace(&mut resource.image, empty_image),
            )
        };

        let texture_resource_generation = self.textures.len() as u64 + 1;
        let replaced_existing_texture_resource =
            if let Some(old_resource) = self.textures.remove(&texture_key) {
                self.retire_texture_resource(old_resource);
                true
            } else {
                false
            };
        self.textures.insert(
            texture_key,
            VulkanTextureResource {
                image: image.image,
                memory: image.memory,
                view: image.view,
                face_views: [vk::ImageView::null(); 6],
                width: image.width,
                height: image.height,
                layers: image.layers,
                is_cube: false,
                format: XR_GPU_F32_VOLUME_IMAGE_PREVIEW_FORMAT,
                layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                hardware_buffer: None,
                sampler: None,
                ycbcr_conversion: None,
                ycbcr_conversion_metadata: None,
                owns_sampler_ycbcr_conversion: false,
                owns_image: true,
            },
        );

        Ok(XrGpuF32VolumeImagePreviewTextureAdoption {
            request_id,
            texture_id,
            image_width,
            image_height,
            image_layers,
            queue_submit_serial,
            resource_generation,
            texture_resource_generation,
            replaced_existing_texture_resource,
            runtime_texture_bound: true,
            cpu_texture_upload_performed: false,
            zero_copy_vulkan_image: true,
            image_ownership_transferred: true,
        })
    }

    fn wait_xr_f32_volume_image_preview(
        &mut self,
        request_id: u64,
    ) -> Result<XrGpuF32VolumeImagePreviewResult, String> {
        let resource_index = self
            .xr_f32_volume_image_preview_resources
            .iter()
            .position(|resource| resource.request_id == request_id)
            .ok_or_else(|| format!("f32 volume image preview {request_id} was not found"))?;
        if self.xr_f32_volume_image_preview_resources[resource_index].completed {
            return Err(format!(
                "f32 volume image preview {request_id} was already completed"
            ));
        }

        let fence = self.xr_f32_volume_image_preview_resources[resource_index].fence;
        let queue_submit_serial =
            self.xr_f32_volume_image_preview_resources[resource_index].queue_submit_serial;
        unsafe {
            self.device
                .wait_for_fences(&[fence], true, u64::MAX)
                .map_err(|e| format!("wait_for_fences(f32 volume image preview) failed: {e:?}"))?;
            self.device
                .queue_wait_idle(self.queue)
                .map_err(|e| format!("queue_wait_idle(f32 volume image preview) failed: {e:?}"))?;
        }
        self.gpu_completed_submit_serial =
            self.gpu_completed_submit_serial.max(queue_submit_serial);
        self.collect_retired_texture_resources();
        self.complete_xr_f32_volume_image_preview(resource_index, true)
    }

    fn complete_xr_f32_volume_image_preview(
        &mut self,
        resource_index: usize,
        queue_wait_idle_performed: bool,
    ) -> Result<XrGpuF32VolumeImagePreviewResult, String> {
        let (
            started,
            pixels,
            expected_outputs,
            image_width,
            image_height,
            image_layers,
            eye_tile_width,
            eye_tile_height,
            eye_count,
            pixel_count,
            tolerance,
            queue_submit_serial,
            resource_generation,
            sampled_readback_buffer,
        ) = {
            let resource = self
                .xr_f32_volume_image_preview_resources
                .get(resource_index)
                .ok_or_else(|| "f32 volume image preview resource index is stale".to_string())?;
            if resource.completed {
                return Err(format!(
                    "f32 volume image preview {} was already completed",
                    resource.request_id
                ));
            }
            (
                resource.started,
                resource.pixels,
                resource.expected_outputs,
                resource.image_width,
                resource.image_height,
                resource.image_layers,
                resource.eye_tile_width,
                resource.eye_tile_height,
                resource.eye_count,
                resource.pixel_count,
                resource.tolerance,
                resource.queue_submit_serial,
                resource.resource_generation,
                resource.sampled_readback,
            )
        };
        let readback_byte_len = std::mem::size_of::<
            [XrGpuF32VolumeImagePreviewOutput; XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS],
        >() as vk::DeviceSize;
        let outputs = unsafe {
            let mapped = self
                .device
                .map_memory(
                    sampled_readback_buffer.memory,
                    0,
                    readback_byte_len,
                    vk::MemoryMapFlags::empty(),
                )
                .map_err(|err| {
                    format!("map_memory(f32 volume image preview readback) failed: {err:?}")
                })?;
            let rows = std::slice::from_raw_parts(
                mapped as *const XrGpuF32VolumeImagePreviewOutput,
                XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS,
            );
            let mut outputs = [XrGpuF32VolumeImagePreviewOutput::default();
                XR_GPU_F32_VOLUME_IMAGE_PREVIEW_PIXELS];
            outputs.copy_from_slice(rows);
            self.device.unmap_memory(sampled_readback_buffer.memory);
            outputs
        };

        let mut mismatched_components = 0;
        let mut max_abs_error = 0.0_f32;
        for index in 0..pixel_count {
            for component in 0..4 {
                let diff = (outputs[index].rgba[component]
                    - expected_outputs[index].rgba[component])
                    .abs();
                if !diff.is_finite() {
                    max_abs_error = f32::INFINITY;
                    mismatched_components += 1;
                } else {
                    max_abs_error = max_abs_error.max(diff);
                    if diff > tolerance {
                        mismatched_components += 1;
                    }
                }
            }
        }

        if let Some(resource) = self
            .xr_f32_volume_image_preview_resources
            .get_mut(resource_index)
        {
            resource.completed = true;
        }
        let retained_resource_count = self.xr_f32_volume_image_preview_resources.len();
        let pending_retire_count = self
            .xr_f32_volume_image_preview_resources
            .iter()
            .filter(|resource| !resource.completed)
            .count();

        Ok(XrGpuF32VolumeImagePreviewResult {
            pixels,
            outputs,
            expected_outputs,
            image_width,
            image_height,
            image_layers,
            eye_tile_width,
            eye_tile_height,
            eye_count,
            pixel_count,
            component_count: pixel_count * 4,
            mismatched_components,
            max_abs_error,
            tolerance,
            queue_submit_serial,
            fence_serial: queue_submit_serial,
            resource_generation,
            pending_retire_count,
            retained_resource_count,
            retired_after_fence_count: 0,
            storage_image_written: true,
            transfer_readback_performed: true,
            sampled_image_usage: true,
            sampled_texture_bound: true,
            queue_wait_idle_performed,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    fn create_xr_f32_volume_image_preview_image(
        &self,
        width: u32,
        height: u32,
    ) -> Result<VulkanXrF32VolumeImagePreviewImage, String> {
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(XR_GPU_F32_VOLUME_IMAGE_PREVIEW_FORMAT)
            .extent(vk::Extent3D {
                width: width.max(1),
                height: height.max(1),
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(XR_GPU_F32_VOLUME_IMAGE_PREVIEW_IMAGE_LAYERS as u32)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::STORAGE
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_SRC,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { self.device.create_image(&image_info, None) }
            .map_err(|e| format!("create_image(f32 volume image preview) failed: {e:?}"))?;
        let memory_req = unsafe { self.device.get_image_memory_requirements(image) };
        let memory_type_index = self
            .find_memory_type(
                memory_req.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .or_else(|_| {
                self.find_memory_type(
                    memory_req.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
            })?;
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(memory_req.size)
            .memory_type_index(memory_type_index);
        let memory = match unsafe { self.device.allocate_memory(&alloc_info, None) } {
            Ok(memory) => memory,
            Err(e) => {
                unsafe {
                    self.device.destroy_image(image, None);
                }
                return Err(format!(
                    "allocate_memory(f32 volume image preview image) failed: {e:?}"
                ));
            }
        };
        unsafe {
            if let Err(e) = self.device.bind_image_memory(image, memory, 0) {
                self.device.free_memory(memory, None);
                self.device.destroy_image(image, None);
                return Err(format!(
                    "bind_image_memory(f32 volume image preview) failed: {e:?}"
                ));
            }
        }
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(XR_GPU_F32_VOLUME_IMAGE_PREVIEW_FORMAT)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(XR_GPU_F32_VOLUME_IMAGE_PREVIEW_IMAGE_LAYERS as u32),
            );
        let view = match unsafe { self.device.create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(e) => {
                unsafe {
                    self.device.free_memory(memory, None);
                    self.device.destroy_image(image, None);
                }
                return Err(format!(
                    "create_image_view(f32 volume image preview) failed: {e:?}"
                ));
            }
        };

        Ok(VulkanXrF32VolumeImagePreviewImage {
            image,
            memory,
            view,
            width: width.max(1),
            height: height.max(1),
            layers: XR_GPU_F32_VOLUME_IMAGE_PREVIEW_IMAGE_LAYERS as u32,
        })
    }

    fn destroy_xr_f32_volume_image_preview_image(&self, image: VulkanXrF32VolumeImagePreviewImage) {
        unsafe {
            if image.view != vk::ImageView::null() {
                self.device.destroy_image_view(image.view, None);
            }
            if image.image != vk::Image::null() {
                self.device.destroy_image(image.image, None);
            }
            if image.memory != vk::DeviceMemory::null() {
                self.device.free_memory(image.memory, None);
            }
        }
    }

    fn destroy_xr_f32_volume_image_preview_gpu_handles(
        &self,
        fence: vk::Fence,
        command_buffer: vk::CommandBuffer,
        descriptor_pool: vk::DescriptorPool,
        compute_pipeline: vk::Pipeline,
        pipeline_layout: vk::PipelineLayout,
        descriptor_set_layout: vk::DescriptorSetLayout,
        shader_module: vk::ShaderModule,
        sample_compute_pipeline: vk::Pipeline,
        sample_pipeline_layout: vk::PipelineLayout,
        sample_descriptor_set_layout: vk::DescriptorSetLayout,
        sample_shader_module: vk::ShaderModule,
        sampler: vk::Sampler,
    ) {
        unsafe {
            if fence != vk::Fence::null() {
                self.device.destroy_fence(fence, None);
            }
            if command_buffer != vk::CommandBuffer::null()
                && self.command_pool != vk::CommandPool::null()
            {
                self.device
                    .free_command_buffers(self.command_pool, &[command_buffer]);
            }
            if descriptor_pool != vk::DescriptorPool::null() {
                self.device.destroy_descriptor_pool(descriptor_pool, None);
            }
            if sample_compute_pipeline != vk::Pipeline::null() {
                self.device.destroy_pipeline(sample_compute_pipeline, None);
            }
            if compute_pipeline != vk::Pipeline::null() {
                self.device.destroy_pipeline(compute_pipeline, None);
            }
            if sample_pipeline_layout != vk::PipelineLayout::null() {
                self.device
                    .destroy_pipeline_layout(sample_pipeline_layout, None);
            }
            if pipeline_layout != vk::PipelineLayout::null() {
                self.device.destroy_pipeline_layout(pipeline_layout, None);
            }
            if sample_descriptor_set_layout != vk::DescriptorSetLayout::null() {
                self.device
                    .destroy_descriptor_set_layout(sample_descriptor_set_layout, None);
            }
            if descriptor_set_layout != vk::DescriptorSetLayout::null() {
                self.device
                    .destroy_descriptor_set_layout(descriptor_set_layout, None);
            }
            if sample_shader_module != vk::ShaderModule::null() {
                self.device
                    .destroy_shader_module(sample_shader_module, None);
            }
            if shader_module != vk::ShaderModule::null() {
                self.device.destroy_shader_module(shader_module, None);
            }
            if sampler != vk::Sampler::null() {
                self.device.destroy_sampler(sampler, None);
            }
        }
    }

    pub(super) fn destroy_xr_f32_volume_image_preview_resources(&mut self) {
        let resources = std::mem::take(&mut self.xr_f32_volume_image_preview_resources);
        for resource in resources {
            self.destroy_xr_f32_volume_image_preview_gpu_handles(
                resource.fence,
                resource.command_buffer,
                resource.descriptor_pool,
                resource.compute_pipeline,
                resource.pipeline_layout,
                resource.descriptor_set_layout,
                resource.shader_module,
                resource.sample_compute_pipeline,
                resource.sample_pipeline_layout,
                resource.sample_descriptor_set_layout,
                resource.sample_shader_module,
                resource.sampler,
            );
            self.destroy_buffer(resource.sampled_readback);
            self.destroy_buffer(resource.readback);
            self.destroy_xr_f32_volume_image_preview_image(resource.image);
            self.destroy_buffer(resource.input);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_volume_image_preview_compute_shaders() {
        compile_compute_wgsl_to_spirv(
            XR_GPU_F32_VOLUME_IMAGE_PREVIEW_WGSL,
            XR_GPU_F32_VOLUME_IMAGE_PREVIEW_ENTRY,
        )
        .expect("storage-image write shader compiles");
        compile_compute_wgsl_to_spirv(
            XR_GPU_F32_VOLUME_IMAGE_PREVIEW_SAMPLE_WGSL,
            XR_GPU_F32_VOLUME_IMAGE_PREVIEW_SAMPLE_ENTRY,
        )
        .expect("sampled-image read shader compiles");
    }
}
