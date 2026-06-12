use crate::{
    cx_api::{
        XrGpuF32VolumeRaymarchPreviewOutput, XrGpuF32VolumeRaymarchPreviewPixel,
        XrGpuF32VolumeRaymarchPreviewResult, XrGpuF32VolumeRaymarchPreviewTicket,
        XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS,
    },
    os::linux::vulkan_naga::compile_compute_wgsl_to_spirv,
};
use ash::vk;
use std::time::Instant;

use super::{CxVulkan, VulkanBuffer};

const XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_ENTRY: &str = "compute_main";
const XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_WORKGROUP: u32 = 8;
const XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_WGSL: &str = r#"
struct VolumeRaymarchPreviewPixel {
    uv_eye_time: vec4<f32>,
    ray_origin: vec4<f32>,
    ray_direction_step: vec4<f32>,
    volume_params: vec4<f32>,
    expected_rgba: vec4<f32>,
    expected_density_depth_status: vec4<f32>,
};

struct VolumeRaymarchPreviewOutput {
    rgba: vec4<f32>,
    density_depth_status: vec4<f32>,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<VolumeRaymarchPreviewPixel, 32>;
@group(0) @binding(1) var<storage, read_write> outputs: array<VolumeRaymarchPreviewOutput, 32>;

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

fn volume_raymarch_preview_output(pixel: VolumeRaymarchPreviewPixel) -> VolumeRaymarchPreviewOutput {
    let uv = pixel.uv_eye_time;
    let origin = pixel.ray_origin.xyz;
    let direction = pixel.ray_direction_step.xyz;
    let step_count = clamp(pixel.ray_direction_step.w, 1.0, 32.0);
    let step_alpha_scale = clamp(pixel.volume_params.w, 0.001, 4.0);
    let eye_gain = 0.65 + 0.35 * clamp(uv.z, 0.0, 1.0);
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
            let sample_rgb = vec3<f32>(density, density * eye_gain, 1.0 - density);
            let contribution = (1.0 - accum_alpha) * sample_alpha;
            accum_rgb = accum_rgb + sample_rgb * contribution;
            if (hit < 0.5 && density > 0.05) {
                first_depth = unit_depth;
                hit = 1.0;
            }
            accum_alpha = clamp(accum_alpha + contribution, 0.0, 1.0);
        }
    }

    return VolumeRaymarchPreviewOutput(
        vec4<f32>(accum_rgb, accum_alpha),
        vec4<f32>(accum_alpha, first_depth, hit, step_count)
    );
}

@compute @workgroup_size(8)
fn compute_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if (index < 32u) {
        outputs[index] = volume_raymarch_preview_output(input_pixels[index]);
    }
}
"#;

fn expected_xr_gpu_f32_volume_raymarch_preview_outputs(
    pixels: [XrGpuF32VolumeRaymarchPreviewPixel; XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS],
) -> [XrGpuF32VolumeRaymarchPreviewOutput; XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS] {
    let mut expected =
        [XrGpuF32VolumeRaymarchPreviewOutput::default(); XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS];
    for (index, pixel) in pixels.iter().copied().enumerate() {
        expected[index] = XrGpuF32VolumeRaymarchPreviewOutput {
            rgba: pixel.expected_rgba,
            density_depth_status: pixel.expected_density_depth_status,
        };
    }
    expected
}

pub(super) struct VulkanXrF32VolumeRaymarchPreviewResources {
    request_id: u64,
    started: Instant,
    pixels: [XrGpuF32VolumeRaymarchPreviewPixel; XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS],
    expected_outputs:
        [XrGpuF32VolumeRaymarchPreviewOutput; XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS],
    preview_width: usize,
    preview_height: usize,
    eye_count: usize,
    pixel_count: usize,
    tolerance: f32,
    queue_submit_serial: u64,
    resource_generation: u64,
    completed: bool,
    input: VulkanBuffer,
    output: VulkanBuffer,
    shader_module: vk::ShaderModule,
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    compute_pipeline: vk::Pipeline,
    descriptor_pool: vk::DescriptorPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
}

impl CxVulkan {
    pub(crate) fn submit_xr_f32_volume_raymarch_preview(
        &mut self,
        pixels: [XrGpuF32VolumeRaymarchPreviewPixel; XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS],
        preview_width: usize,
        preview_height: usize,
        eye_count: usize,
        pixel_count: usize,
        tolerance: f32,
    ) -> Result<XrGpuF32VolumeRaymarchPreviewResult, String> {
        let ticket = self.submit_xr_f32_volume_raymarch_preview_async(
            pixels,
            preview_width,
            preview_height,
            eye_count,
            pixel_count,
            tolerance,
        )?;
        self.wait_xr_f32_volume_raymarch_preview(ticket.request_id)
    }

    pub(crate) fn submit_xr_f32_volume_raymarch_preview_async(
        &mut self,
        pixels: [XrGpuF32VolumeRaymarchPreviewPixel; XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS],
        preview_width: usize,
        preview_height: usize,
        eye_count: usize,
        pixel_count: usize,
        tolerance: f32,
    ) -> Result<XrGpuF32VolumeRaymarchPreviewTicket, String> {
        let started = Instant::now();
        let preview_width = preview_width.max(1);
        let preview_height = preview_height.max(1);
        let eye_count = eye_count.max(1);
        let pixel_count = pixel_count
            .min(XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS)
            .min(
                preview_width
                    .saturating_mul(preview_height)
                    .saturating_mul(eye_count),
            );
        let tolerance = if tolerance.is_finite() && tolerance >= 0.0 {
            tolerance
        } else {
            0.0
        };
        let expected_outputs = expected_xr_gpu_f32_volume_raymarch_preview_outputs(pixels);
        let input_byte_len = std::mem::size_of::<
            [XrGpuF32VolumeRaymarchPreviewPixel; XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS],
        >() as vk::DeviceSize;
        let output_byte_len = std::mem::size_of::<
            [XrGpuF32VolumeRaymarchPreviewOutput; XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS],
        >() as vk::DeviceSize;

        let shader_spv = compile_compute_wgsl_to_spirv(
            XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_WGSL,
            XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_ENTRY,
        )?;
        let input =
            self.create_host_buffer_with_data(vk::BufferUsageFlags::STORAGE_BUFFER, &pixels)?;
        let output =
            match self.create_host_buffer(vk::BufferUsageFlags::STORAGE_BUFFER, output_byte_len) {
                Ok(buffer) => buffer,
                Err(err) => {
                    self.destroy_buffer(input);
                    return Err(err);
                }
            };

        let shader_module_info = vk::ShaderModuleCreateInfo::default().code(&shader_spv);
        let shader_module =
            match unsafe { self.device.create_shader_module(&shader_module_info, None) } {
                Ok(shader_module) => shader_module,
                Err(err) => {
                    self.destroy_buffer(output);
                    self.destroy_buffer(input);
                    return Err(format!(
                        "create_shader_module(f32 volume raymarch preview) failed: {err:?}"
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
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
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
                    self.device.destroy_shader_module(shader_module, None);
                }
                self.destroy_buffer(output);
                self.destroy_buffer(input);
                return Err(format!(
                    "create_descriptor_set_layout(f32 volume raymarch preview) failed: {err:?}"
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
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    self.device.destroy_shader_module(shader_module, None);
                }
                self.destroy_buffer(output);
                self.destroy_buffer(input);
                return Err(format!(
                    "create_pipeline_layout(f32 volume raymarch preview) failed: {err:?}"
                ));
            }
        };

        let entry = std::ffi::CString::new(XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_ENTRY).unwrap();
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
                    self.device.destroy_pipeline_layout(pipeline_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    self.device.destroy_shader_module(shader_module, None);
                }
                self.destroy_buffer(output);
                self.destroy_buffer(input);
                return Err(format!(
                    "create_compute_pipelines(f32 volume raymarch preview) failed: {err:?}"
                ));
            }
        };

        let descriptor_pool_sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::STORAGE_BUFFER,
            descriptor_count: 2,
        }];
        let descriptor_pool_info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(1)
            .pool_sizes(&descriptor_pool_sizes);
        let descriptor_pool = match unsafe {
            self.device
                .create_descriptor_pool(&descriptor_pool_info, None)
        } {
            Ok(pool) => pool,
            Err(err) => {
                unsafe {
                    self.device.destroy_pipeline(compute_pipeline, None);
                    self.device.destroy_pipeline_layout(pipeline_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    self.device.destroy_shader_module(shader_module, None);
                }
                self.destroy_buffer(output);
                self.destroy_buffer(input);
                return Err(format!(
                    "create_descriptor_pool(f32 volume raymarch preview) failed: {err:?}"
                ));
            }
        };
        let descriptor_set = {
            let alloc_info = vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(descriptor_pool)
                .set_layouts(&set_layouts);
            match unsafe { self.device.allocate_descriptor_sets(&alloc_info) } {
                Ok(sets) => sets[0],
                Err(err) => {
                    unsafe {
                        self.device.destroy_descriptor_pool(descriptor_pool, None);
                        self.device.destroy_pipeline(compute_pipeline, None);
                        self.device.destroy_pipeline_layout(pipeline_layout, None);
                        self.device
                            .destroy_descriptor_set_layout(descriptor_set_layout, None);
                        self.device.destroy_shader_module(shader_module, None);
                    }
                    self.destroy_buffer(output);
                    self.destroy_buffer(input);
                    return Err(format!(
                        "allocate_descriptor_sets(f32 volume raymarch preview) failed: {err:?}"
                    ));
                }
            }
        };

        let input_buffer_info = vk::DescriptorBufferInfo::default()
            .buffer(input.buffer)
            .offset(0)
            .range(input_byte_len);
        let output_buffer_info = vk::DescriptorBufferInfo::default()
            .buffer(output.buffer)
            .offset(0)
            .range(output_byte_len);
        let input_buffer_infos = [input_buffer_info];
        let output_buffer_infos = [output_buffer_info];
        let descriptor_writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&input_buffer_infos),
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&output_buffer_infos),
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
                    unsafe {
                        self.device.destroy_descriptor_pool(descriptor_pool, None);
                        self.device.destroy_pipeline(compute_pipeline, None);
                        self.device.destroy_pipeline_layout(pipeline_layout, None);
                        self.device
                            .destroy_descriptor_set_layout(descriptor_set_layout, None);
                        self.device.destroy_shader_module(shader_module, None);
                    }
                    self.destroy_buffer(output);
                    self.destroy_buffer(input);
                    return Err(format!(
                        "allocate_command_buffers(f32 volume raymarch preview) failed: {err:?}"
                    ));
                }
            }
        };
        let fence = {
            let fence_info = vk::FenceCreateInfo::default();
            match unsafe { self.device.create_fence(&fence_info, None) } {
                Ok(fence) => fence,
                Err(err) => {
                    unsafe {
                        self.device
                            .free_command_buffers(self.command_pool, &[command_buffer]);
                        self.device.destroy_descriptor_pool(descriptor_pool, None);
                        self.device.destroy_pipeline(compute_pipeline, None);
                        self.device.destroy_pipeline_layout(pipeline_layout, None);
                        self.device
                            .destroy_descriptor_set_layout(descriptor_set_layout, None);
                        self.device.destroy_shader_module(shader_module, None);
                    }
                    self.destroy_buffer(output);
                    self.destroy_buffer(input);
                    return Err(format!(
                        "create_fence(f32 volume raymarch preview) failed: {err:?}"
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
                        format!("begin_command_buffer(f32 volume raymarch preview) failed: {e:?}")
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
                let dispatch_x = ((XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS as u32)
                    + XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_WORKGROUP
                    - 1)
                    / XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_WORKGROUP;
                self.device.cmd_dispatch(command_buffer, dispatch_x, 1, 1);

                let output_barrier = vk::BufferMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .dst_access_mask(vk::AccessFlags::HOST_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(output.buffer)
                    .offset(0)
                    .size(output_byte_len);
                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::PipelineStageFlags::HOST,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[output_barrier],
                    &[],
                );

                self.device
                    .end_command_buffer(command_buffer)
                    .map_err(|e| {
                        format!("end_command_buffer(f32 volume raymarch preview) failed: {e:?}")
                    })?;
                self.device
                    .queue_submit(
                        self.queue,
                        &[vk::SubmitInfo::default().command_buffers(&[command_buffer])],
                        fence,
                    )
                    .map_err(|e| {
                        format!("queue_submit(f32 volume raymarch preview) failed: {e:?}")
                    })?;
                self.gpu_submit_serial = self.gpu_submit_serial.saturating_add(1);
                queue_submit_serial = self.gpu_submit_serial;
            }
            Ok(())
        })();
        if let Err(err) = command_result {
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
                self.device.destroy_descriptor_pool(descriptor_pool, None);
                self.device.destroy_pipeline(compute_pipeline, None);
                self.device.destroy_pipeline_layout(pipeline_layout, None);
                self.device
                    .destroy_descriptor_set_layout(descriptor_set_layout, None);
                self.device.destroy_shader_module(shader_module, None);
            }
            self.destroy_buffer(output);
            self.destroy_buffer(input);
            return Err(err);
        }

        let resource_generation = self.xr_f32_volume_raymarch_preview_resources.len() as u64 + 1;
        let request_id = queue_submit_serial;
        self.xr_f32_volume_raymarch_preview_resources.push(
            VulkanXrF32VolumeRaymarchPreviewResources {
                request_id,
                started,
                pixels,
                expected_outputs,
                preview_width,
                preview_height,
                eye_count,
                pixel_count,
                tolerance,
                queue_submit_serial,
                resource_generation,
                completed: false,
                input,
                output,
                shader_module,
                descriptor_set_layout,
                pipeline_layout,
                compute_pipeline,
                descriptor_pool,
                command_buffer,
                fence,
            },
        );
        let retained_resource_count = self.xr_f32_volume_raymarch_preview_resources.len();
        let pending_retire_count = retained_resource_count;

        Ok(XrGpuF32VolumeRaymarchPreviewTicket {
            request_id,
            queue_submit_serial,
            resource_generation,
            pending_retire_count,
            retained_resource_count,
        })
    }

    pub(crate) fn poll_xr_f32_volume_raymarch_preview(
        &mut self,
        request_id: u64,
    ) -> Result<Option<XrGpuF32VolumeRaymarchPreviewResult>, String> {
        let Some(resource_index) = self
            .xr_f32_volume_raymarch_preview_resources
            .iter()
            .position(|resource| resource.request_id == request_id)
        else {
            return Ok(None);
        };
        if self.xr_f32_volume_raymarch_preview_resources[resource_index].completed {
            return Ok(None);
        }

        let fence = self.xr_f32_volume_raymarch_preview_resources[resource_index].fence;
        let queue_submit_serial =
            self.xr_f32_volume_raymarch_preview_resources[resource_index].queue_submit_serial;
        match unsafe { self.device.get_fence_status(fence) } {
            Ok(true) => {
                self.gpu_completed_submit_serial =
                    self.gpu_completed_submit_serial.max(queue_submit_serial);
                self.collect_retired_texture_resources();
                self.complete_xr_f32_volume_raymarch_preview(resource_index, false)
                    .map(Some)
            }
            Ok(false) => Ok(None),
            Err(err) => Err(format!(
                "get_fence_status(f32 volume raymarch preview {request_id}) failed: {err:?}"
            )),
        }
    }

    fn wait_xr_f32_volume_raymarch_preview(
        &mut self,
        request_id: u64,
    ) -> Result<XrGpuF32VolumeRaymarchPreviewResult, String> {
        let resource_index = self
            .xr_f32_volume_raymarch_preview_resources
            .iter()
            .position(|resource| resource.request_id == request_id)
            .ok_or_else(|| format!("f32 volume raymarch preview {request_id} was not found"))?;
        if self.xr_f32_volume_raymarch_preview_resources[resource_index].completed {
            return Err(format!(
                "f32 volume raymarch preview {request_id} was already completed"
            ));
        }

        let fence = self.xr_f32_volume_raymarch_preview_resources[resource_index].fence;
        let queue_submit_serial =
            self.xr_f32_volume_raymarch_preview_resources[resource_index].queue_submit_serial;
        unsafe {
            self.device
                .wait_for_fences(&[fence], true, u64::MAX)
                .map_err(|e| {
                    format!("wait_for_fences(f32 volume raymarch preview) failed: {e:?}")
                })?;
            self.device.queue_wait_idle(self.queue).map_err(|e| {
                format!("queue_wait_idle(f32 volume raymarch preview) failed: {e:?}")
            })?;
        }
        self.gpu_completed_submit_serial =
            self.gpu_completed_submit_serial.max(queue_submit_serial);
        self.collect_retired_texture_resources();
        self.complete_xr_f32_volume_raymarch_preview(resource_index, true)
    }

    fn complete_xr_f32_volume_raymarch_preview(
        &mut self,
        resource_index: usize,
        queue_wait_idle_performed: bool,
    ) -> Result<XrGpuF32VolumeRaymarchPreviewResult, String> {
        let (
            started,
            pixels,
            expected_outputs,
            preview_width,
            preview_height,
            eye_count,
            pixel_count,
            tolerance,
            queue_submit_serial,
            resource_generation,
            output_buffer,
        ) = {
            let resource = self
                .xr_f32_volume_raymarch_preview_resources
                .get(resource_index)
                .ok_or_else(|| "f32 volume raymarch preview resource index is stale".to_string())?;
            if resource.completed {
                return Err(format!(
                    "f32 volume raymarch preview {} was already completed",
                    resource.request_id
                ));
            }
            (
                resource.started,
                resource.pixels,
                resource.expected_outputs,
                resource.preview_width,
                resource.preview_height,
                resource.eye_count,
                resource.pixel_count,
                resource.tolerance,
                resource.queue_submit_serial,
                resource.resource_generation,
                resource.output,
            )
        };
        let output_byte_len = std::mem::size_of::<
            [XrGpuF32VolumeRaymarchPreviewOutput; XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS],
        >() as vk::DeviceSize;
        let outputs = unsafe {
            let mapped = self
                .device
                .map_memory(
                    output_buffer.memory,
                    0,
                    output_byte_len,
                    vk::MemoryMapFlags::empty(),
                )
                .map_err(|err| {
                    format!("map_memory(f32 volume raymarch preview readback) failed: {err:?}")
                })?;
            let rows = std::slice::from_raw_parts(
                mapped as *const XrGpuF32VolumeRaymarchPreviewOutput,
                XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS,
            );
            let mut outputs = [XrGpuF32VolumeRaymarchPreviewOutput::default();
                XR_GPU_F32_VOLUME_RAYMARCH_PREVIEW_PIXELS];
            outputs.copy_from_slice(rows);
            self.device.unmap_memory(output_buffer.memory);
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
            for component in 0..4 {
                let diff = (outputs[index].density_depth_status[component]
                    - expected_outputs[index].density_depth_status[component])
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
            .xr_f32_volume_raymarch_preview_resources
            .get_mut(resource_index)
        {
            resource.completed = true;
        }
        let retained_resource_count = self.xr_f32_volume_raymarch_preview_resources.len();
        let pending_retire_count = self
            .xr_f32_volume_raymarch_preview_resources
            .iter()
            .filter(|resource| !resource.completed)
            .count();

        Ok(XrGpuF32VolumeRaymarchPreviewResult {
            pixels,
            outputs,
            expected_outputs,
            preview_width,
            preview_height,
            eye_count,
            pixel_count,
            component_count: pixel_count * 8,
            mismatched_components,
            max_abs_error,
            tolerance,
            queue_submit_serial,
            fence_serial: queue_submit_serial,
            resource_generation,
            pending_retire_count,
            retained_resource_count,
            retired_after_fence_count: 0,
            queue_wait_idle_performed,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    pub(super) fn destroy_xr_f32_volume_raymarch_preview_resources(&mut self) {
        let resources = std::mem::take(&mut self.xr_f32_volume_raymarch_preview_resources);
        for resource in resources {
            unsafe {
                if resource.fence != vk::Fence::null() {
                    self.device.destroy_fence(resource.fence, None);
                }
                if resource.command_buffer != vk::CommandBuffer::null()
                    && self.command_pool != vk::CommandPool::null()
                {
                    self.device
                        .free_command_buffers(self.command_pool, &[resource.command_buffer]);
                }
                if resource.descriptor_pool != vk::DescriptorPool::null() {
                    self.device
                        .destroy_descriptor_pool(resource.descriptor_pool, None);
                }
                if resource.compute_pipeline != vk::Pipeline::null() {
                    self.device
                        .destroy_pipeline(resource.compute_pipeline, None);
                }
                if resource.pipeline_layout != vk::PipelineLayout::null() {
                    self.device
                        .destroy_pipeline_layout(resource.pipeline_layout, None);
                }
                if resource.descriptor_set_layout != vk::DescriptorSetLayout::null() {
                    self.device
                        .destroy_descriptor_set_layout(resource.descriptor_set_layout, None);
                }
                if resource.shader_module != vk::ShaderModule::null() {
                    self.device
                        .destroy_shader_module(resource.shader_module, None);
                }
            }
            self.destroy_buffer(resource.output);
            self.destroy_buffer(resource.input);
        }
    }
}
