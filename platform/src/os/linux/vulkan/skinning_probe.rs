use crate::{
    cx_api::{
        XrGpuF32SkinningProbeResult, XrGpuF32SkinningProbeSample, XrGpuF32SkinningProbeTicket,
        XR_GPU_F32_SKINNING_PROBE_SAMPLES,
    },
    os::linux::vulkan_naga::compile_compute_wgsl_to_spirv,
};
use ash::vk;
use std::time::Instant;

use super::{CxVulkan, VulkanBuffer};

const XR_GPU_F32_SKINNING_PROBE_ENTRY: &str = "compute_main";
const XR_GPU_F32_SKINNING_PROBE_WGSL: &str = r#"
struct SkinningProbeSample {
    bind_position: vec4<f32>,
    joint_weights: vec4<f32>,
    matrix0_row0: vec4<f32>,
    matrix0_row1: vec4<f32>,
    matrix0_row2: vec4<f32>,
    matrix0_row3: vec4<f32>,
    matrix1_row0: vec4<f32>,
    matrix1_row1: vec4<f32>,
    matrix1_row2: vec4<f32>,
    matrix1_row3: vec4<f32>,
    matrix2_row0: vec4<f32>,
    matrix2_row1: vec4<f32>,
    matrix2_row2: vec4<f32>,
    matrix2_row3: vec4<f32>,
    matrix3_row0: vec4<f32>,
    matrix3_row1: vec4<f32>,
    matrix3_row2: vec4<f32>,
    matrix3_row3: vec4<f32>,
    expected_position: vec4<f32>,
};

@group(0) @binding(0) var<storage, read> input_samples: array<SkinningProbeSample, 4>;
@group(0) @binding(1) var<storage, read_write> output_positions: array<vec4<f32>, 4>;

@compute @workgroup_size(4)
fn compute_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if (index < 4u) {
        let sample = input_samples[index];
        let p = sample.bind_position;
        let p0 = vec3<f32>(
            dot(sample.matrix0_row0, p),
            dot(sample.matrix0_row1, p),
            dot(sample.matrix0_row2, p)
        );
        let p1 = vec3<f32>(
            dot(sample.matrix1_row0, p),
            dot(sample.matrix1_row1, p),
            dot(sample.matrix1_row2, p)
        );
        let p2 = vec3<f32>(
            dot(sample.matrix2_row0, p),
            dot(sample.matrix2_row1, p),
            dot(sample.matrix2_row2, p)
        );
        let p3 = vec3<f32>(
            dot(sample.matrix3_row0, p),
            dot(sample.matrix3_row1, p),
            dot(sample.matrix3_row2, p)
        );
        let weights = sample.joint_weights;
        let total_weight = weights.x + weights.y + weights.z + weights.w;
        if (total_weight > 0.0) {
            let weighted = p0 * weights.x + p1 * weights.y + p2 * weights.z + p3 * weights.w;
            output_positions[index] = vec4<f32>(weighted / total_weight, 1.0);
        } else {
            output_positions[index] = sample.bind_position;
        }
    }
}
"#;

fn expected_xr_gpu_f32_skinning_probe_positions(
    samples: [XrGpuF32SkinningProbeSample; XR_GPU_F32_SKINNING_PROBE_SAMPLES],
) -> [[f32; 4]; XR_GPU_F32_SKINNING_PROBE_SAMPLES] {
    let mut expected = [[0.0; 4]; XR_GPU_F32_SKINNING_PROBE_SAMPLES];
    for (index, sample) in samples.iter().copied().enumerate() {
        expected[index] = sample.expected_position;
    }
    expected
}

pub(super) struct VulkanXrF32SkinningProbeResources {
    request_id: u64,
    started: Instant,
    samples: [XrGpuF32SkinningProbeSample; XR_GPU_F32_SKINNING_PROBE_SAMPLES],
    expected_positions: [[f32; 4]; XR_GPU_F32_SKINNING_PROBE_SAMPLES],
    sample_count: usize,
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
    pub(crate) fn submit_xr_f32_skinning_probe(
        &mut self,
        samples: [XrGpuF32SkinningProbeSample; XR_GPU_F32_SKINNING_PROBE_SAMPLES],
        sample_count: usize,
        tolerance: f32,
    ) -> Result<XrGpuF32SkinningProbeResult, String> {
        let ticket = self.submit_xr_f32_skinning_probe_async(samples, sample_count, tolerance)?;
        self.wait_xr_f32_skinning_probe(ticket.request_id)
    }

    pub(crate) fn submit_xr_f32_skinning_probe_async(
        &mut self,
        samples: [XrGpuF32SkinningProbeSample; XR_GPU_F32_SKINNING_PROBE_SAMPLES],
        sample_count: usize,
        tolerance: f32,
    ) -> Result<XrGpuF32SkinningProbeTicket, String> {
        let started = Instant::now();
        let sample_count = sample_count.min(XR_GPU_F32_SKINNING_PROBE_SAMPLES);
        let tolerance = if tolerance.is_finite() && tolerance >= 0.0 {
            tolerance
        } else {
            0.0
        };
        let expected_positions = expected_xr_gpu_f32_skinning_probe_positions(samples);
        let input_byte_len = std::mem::size_of::<
            [XrGpuF32SkinningProbeSample; XR_GPU_F32_SKINNING_PROBE_SAMPLES],
        >() as vk::DeviceSize;
        let output_byte_len =
            std::mem::size_of::<[[f32; 4]; XR_GPU_F32_SKINNING_PROBE_SAMPLES]>() as vk::DeviceSize;

        let shader_spv = compile_compute_wgsl_to_spirv(
            XR_GPU_F32_SKINNING_PROBE_WGSL,
            XR_GPU_F32_SKINNING_PROBE_ENTRY,
        )?;
        let input =
            self.create_host_buffer_with_data(vk::BufferUsageFlags::STORAGE_BUFFER, &samples)?;
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
                        "create_shader_module(f32 skinning probe) failed: {err:?}"
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
                    "create_descriptor_set_layout(f32 skinning probe) failed: {err:?}"
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
                    "create_pipeline_layout(f32 skinning probe) failed: {err:?}"
                ));
            }
        };

        let entry = std::ffi::CString::new(XR_GPU_F32_SKINNING_PROBE_ENTRY).unwrap();
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
                    "create_compute_pipelines(f32 skinning probe) failed: {err:?}"
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
                    "create_descriptor_pool(f32 skinning probe) failed: {err:?}"
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
                        "allocate_descriptor_sets(f32 skinning probe) failed: {err:?}"
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
                        "allocate_command_buffers(f32 skinning probe) failed: {err:?}"
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
                    return Err(format!("create_fence(f32 skinning probe) failed: {err:?}"));
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
                        format!("begin_command_buffer(f32 skinning probe) failed: {e:?}")
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
                self.device.cmd_dispatch(command_buffer, 1, 1, 1);

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
                    .map_err(|e| format!("end_command_buffer(f32 skinning probe) failed: {e:?}"))?;
                self.device
                    .queue_submit(
                        self.queue,
                        &[vk::SubmitInfo::default().command_buffers(&[command_buffer])],
                        fence,
                    )
                    .map_err(|e| format!("queue_submit(f32 skinning probe) failed: {e:?}"))?;
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

        let resource_generation = self.xr_f32_skinning_probe_resources.len() as u64 + 1;
        let request_id = queue_submit_serial;
        self.xr_f32_skinning_probe_resources
            .push(VulkanXrF32SkinningProbeResources {
                request_id,
                started,
                samples,
                expected_positions,
                sample_count,
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
            });
        let retained_resource_count = self.xr_f32_skinning_probe_resources.len();
        let pending_retire_count = retained_resource_count;

        Ok(XrGpuF32SkinningProbeTicket {
            request_id,
            queue_submit_serial,
            resource_generation,
            pending_retire_count,
            retained_resource_count,
        })
    }

    pub(crate) fn poll_xr_f32_skinning_probe(
        &mut self,
        request_id: u64,
    ) -> Result<Option<XrGpuF32SkinningProbeResult>, String> {
        let Some(resource_index) = self
            .xr_f32_skinning_probe_resources
            .iter()
            .position(|resource| resource.request_id == request_id)
        else {
            return Ok(None);
        };
        if self.xr_f32_skinning_probe_resources[resource_index].completed {
            return Ok(None);
        }

        let fence = self.xr_f32_skinning_probe_resources[resource_index].fence;
        let queue_submit_serial =
            self.xr_f32_skinning_probe_resources[resource_index].queue_submit_serial;
        match unsafe { self.device.get_fence_status(fence) } {
            Ok(true) => {
                self.gpu_completed_submit_serial =
                    self.gpu_completed_submit_serial.max(queue_submit_serial);
                self.collect_retired_texture_resources();
                self.complete_xr_f32_skinning_probe(resource_index, false)
                    .map(Some)
            }
            Ok(false) => Ok(None),
            Err(err) => Err(format!(
                "get_fence_status(f32 skinning probe {request_id}) failed: {err:?}"
            )),
        }
    }

    fn wait_xr_f32_skinning_probe(
        &mut self,
        request_id: u64,
    ) -> Result<XrGpuF32SkinningProbeResult, String> {
        let resource_index = self
            .xr_f32_skinning_probe_resources
            .iter()
            .position(|resource| resource.request_id == request_id)
            .ok_or_else(|| format!("f32 skinning probe {request_id} was not found"))?;
        if self.xr_f32_skinning_probe_resources[resource_index].completed {
            return Err(format!(
                "f32 skinning probe {request_id} was already completed"
            ));
        }

        let fence = self.xr_f32_skinning_probe_resources[resource_index].fence;
        let queue_submit_serial =
            self.xr_f32_skinning_probe_resources[resource_index].queue_submit_serial;
        unsafe {
            self.device
                .wait_for_fences(&[fence], true, u64::MAX)
                .map_err(|e| format!("wait_for_fences(f32 skinning probe) failed: {e:?}"))?;
            self.device
                .queue_wait_idle(self.queue)
                .map_err(|e| format!("queue_wait_idle(f32 skinning probe) failed: {e:?}"))?;
        }
        self.gpu_completed_submit_serial =
            self.gpu_completed_submit_serial.max(queue_submit_serial);
        self.collect_retired_texture_resources();
        self.complete_xr_f32_skinning_probe(resource_index, true)
    }

    fn complete_xr_f32_skinning_probe(
        &mut self,
        resource_index: usize,
        queue_wait_idle_performed: bool,
    ) -> Result<XrGpuF32SkinningProbeResult, String> {
        let (
            started,
            samples,
            expected_positions,
            sample_count,
            tolerance,
            queue_submit_serial,
            resource_generation,
            output_buffer,
        ) = {
            let resource = self
                .xr_f32_skinning_probe_resources
                .get(resource_index)
                .ok_or_else(|| "f32 skinning probe resource index is stale".to_string())?;
            if resource.completed {
                return Err(format!(
                    "f32 skinning probe {} was already completed",
                    resource.request_id
                ));
            }
            (
                resource.started,
                resource.samples,
                resource.expected_positions,
                resource.sample_count,
                resource.tolerance,
                resource.queue_submit_serial,
                resource.resource_generation,
                resource.output,
            )
        };
        let output_byte_len =
            std::mem::size_of::<[[f32; 4]; XR_GPU_F32_SKINNING_PROBE_SAMPLES]>() as vk::DeviceSize;
        let output_positions = unsafe {
            let mapped = self
                .device
                .map_memory(
                    output_buffer.memory,
                    0,
                    output_byte_len,
                    vk::MemoryMapFlags::empty(),
                )
                .map_err(|err| {
                    format!("map_memory(f32 skinning probe readback) failed: {err:?}")
                })?;
            let rows = std::slice::from_raw_parts(
                mapped as *const [f32; 4],
                XR_GPU_F32_SKINNING_PROBE_SAMPLES,
            );
            let mut output_positions = [[0.0; 4]; XR_GPU_F32_SKINNING_PROBE_SAMPLES];
            output_positions.copy_from_slice(rows);
            self.device.unmap_memory(output_buffer.memory);
            output_positions
        };

        let mut mismatched_components = 0;
        let mut max_abs_error = 0.0_f32;
        for index in 0..sample_count {
            for component in 0..3 {
                let diff = (output_positions[index][component]
                    - expected_positions[index][component])
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

        if let Some(resource) = self.xr_f32_skinning_probe_resources.get_mut(resource_index) {
            resource.completed = true;
        }
        let retained_resource_count = self.xr_f32_skinning_probe_resources.len();
        let pending_retire_count = self
            .xr_f32_skinning_probe_resources
            .iter()
            .filter(|resource| !resource.completed)
            .count();

        Ok(XrGpuF32SkinningProbeResult {
            samples,
            output_positions,
            expected_positions,
            sample_count,
            component_count: sample_count * 3,
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

    pub(super) fn destroy_xr_f32_skinning_probe_resources(&mut self) {
        let resources = std::mem::take(&mut self.xr_f32_skinning_probe_resources);
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
