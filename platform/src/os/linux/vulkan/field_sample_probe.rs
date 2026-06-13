use crate::{
    cx_api::{XrGpuF32FieldSampleProbeResult, XR_GPU_F32_FIELD_SAMPLE_PROBE_SAMPLES},
    os::linux::vulkan_naga::compile_compute_wgsl_to_spirv,
};
use ash::vk;
use std::time::Instant;

use super::{CxVulkan, VulkanBuffer};

const XR_GPU_F32_FIELD_SAMPLE_PROBE_ENTRY: &str = "sample_main";
const XR_GPU_F32_FIELD_SAMPLE_PROBE_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read> sdf_distances: array<f32>;
@group(0) @binding(1) var<storage, read> sample_indices: array<u32, 8>;
@group(0) @binding(2) var<storage, read_write> output_distances: array<f32, 8>;
@group(0) @binding(3) var<storage, read> params: array<vec4<u32>, 1>;

@compute @workgroup_size(8)
fn sample_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    let sample_count = params[0].x;
    let voxel_count = params[0].y;
    if (index >= sample_count) {
        return;
    }

    let linear_index = sample_indices[index];
    if (linear_index < voxel_count) {
        output_distances[index] = sdf_distances[linear_index];
    } else {
        output_distances[index] = sdf_distances[0u];
    }
}
"#;

pub(super) struct VulkanXrF32FieldSampleProbeProgram {
    generation: u64,
    shader_module: vk::ShaderModule,
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    compute_pipeline: vk::Pipeline,
}

#[derive(Clone, Copy)]
struct VulkanXrF32FieldSampleProbeProgramUse {
    generation: u64,
    program_reused: bool,
    shader_compiled_this_submit: bool,
    pipeline_created_this_submit: bool,
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    compute_pipeline: vk::Pipeline,
}

pub(super) struct VulkanXrF32FieldSampleProbeResources {
    sample_indices: VulkanBuffer,
    output: VulkanBuffer,
    params: VulkanBuffer,
    descriptor_pool: vk::DescriptorPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
}

impl CxVulkan {
    fn ensure_xr_f32_field_sample_probe_program(
        &mut self,
    ) -> Result<VulkanXrF32FieldSampleProbeProgramUse, String> {
        if let Some(program) = self.xr_f32_field_sample_probe_program.as_ref() {
            return Ok(VulkanXrF32FieldSampleProbeProgramUse {
                generation: program.generation,
                program_reused: true,
                shader_compiled_this_submit: false,
                pipeline_created_this_submit: false,
                descriptor_set_layout: program.descriptor_set_layout,
                pipeline_layout: program.pipeline_layout,
                compute_pipeline: program.compute_pipeline,
            });
        }

        let shader_spv = compile_compute_wgsl_to_spirv(
            XR_GPU_F32_FIELD_SAMPLE_PROBE_WGSL,
            XR_GPU_F32_FIELD_SAMPLE_PROBE_ENTRY,
        )?;
        let shader_module = unsafe {
            self.device
                .create_shader_module(
                    &vk::ShaderModuleCreateInfo::default().code(&shader_spv),
                    None,
                )
                .map_err(|err| {
                    format!("create_shader_module(f32 field sample probe) failed: {err:?}")
                })?
        };

        let descriptor_bindings = [
            descriptor_binding(0),
            descriptor_binding(1),
            descriptor_binding(2),
            descriptor_binding(3),
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
                return Err(format!(
                    "create_descriptor_set_layout(f32 field sample probe) failed: {err:?}"
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
                return Err(format!(
                    "create_pipeline_layout(f32 field sample probe) failed: {err:?}"
                ));
            }
        };

        let compute_pipeline = match create_compute_pipeline(
            self,
            shader_module,
            pipeline_layout,
            XR_GPU_F32_FIELD_SAMPLE_PROBE_ENTRY,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                unsafe {
                    self.device.destroy_pipeline_layout(pipeline_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    self.device.destroy_shader_module(shader_module, None);
                }
                return Err(err);
            }
        };

        let generation = 1;
        self.xr_f32_field_sample_probe_program = Some(VulkanXrF32FieldSampleProbeProgram {
            generation,
            shader_module,
            descriptor_set_layout,
            pipeline_layout,
            compute_pipeline,
        });

        Ok(VulkanXrF32FieldSampleProbeProgramUse {
            generation,
            program_reused: false,
            shader_compiled_this_submit: true,
            pipeline_created_this_submit: true,
            descriptor_set_layout,
            pipeline_layout,
            compute_pipeline,
        })
    }

    pub(crate) fn submit_xr_f32_field_sample_probe(
        &mut self,
        sample_linear_indices: [u32; XR_GPU_F32_FIELD_SAMPLE_PROBE_SAMPLES],
        expected_distances: [f32; XR_GPU_F32_FIELD_SAMPLE_PROBE_SAMPLES],
        sample_count: usize,
        tolerance: f32,
    ) -> Result<XrGpuF32FieldSampleProbeResult, String> {
        let started = Instant::now();
        let source_field = self
            .xr_f32_mesh_sdf_resident_field_for_sampling()
            .ok_or_else(|| {
                "f32 field sample probe requires a resident mesh SDF field".to_string()
            })?;
        let voxel_count = source_field
            .logical_voxel_count()
            .ok_or_else(|| "f32 field sample probe voxel count overflow".to_string())?;
        let logical_sdf_distance_byte_len = source_field
            .logical_sdf_distance_byte_len()
            .ok_or_else(|| "f32 field sample probe byte count overflow".to_string())?;
        if voxel_count == 0 || voxel_count > u32::MAX as usize {
            return Err("f32 field sample probe requires a nonzero u32 voxel count".to_string());
        }
        if source_field.sdf_distance_byte_len < logical_sdf_distance_byte_len {
            return Err("f32 field sample probe resident field is smaller than grid".to_string());
        }
        let sample_count = sample_count
            .min(XR_GPU_F32_FIELD_SAMPLE_PROBE_SAMPLES)
            .min(voxel_count);
        if sample_count == 0 {
            return Err("f32 field sample probe requires samples".to_string());
        }
        let tolerance = if tolerance.is_finite() && tolerance >= 0.0 {
            tolerance
        } else {
            0.0
        };

        let mut sample_indices = [0_u32; XR_GPU_F32_FIELD_SAMPLE_PROBE_SAMPLES];
        for index in 0..sample_count {
            let requested = sample_linear_indices[index] as usize;
            sample_indices[index] = if requested < voxel_count {
                sample_linear_indices[index]
            } else {
                0
            };
        }
        let params = [[sample_count as u32, voxel_count as u32, 0, 0]];
        let sample_index_byte_len = std::mem::size_of_val(&sample_indices) as vk::DeviceSize;
        let output_byte_len =
            std::mem::size_of::<[f32; XR_GPU_F32_FIELD_SAMPLE_PROBE_SAMPLES]>() as vk::DeviceSize;
        let params_byte_len = std::mem::size_of_val(&params) as vk::DeviceSize;

        let program = self.ensure_xr_f32_field_sample_probe_program()?;
        let sample_index_buffer = self
            .create_host_buffer_with_data(vk::BufferUsageFlags::STORAGE_BUFFER, &sample_indices)?;
        let output_buffer =
            match self.create_host_buffer(vk::BufferUsageFlags::STORAGE_BUFFER, output_byte_len) {
                Ok(buffer) => buffer,
                Err(err) => {
                    self.destroy_buffer(sample_index_buffer);
                    return Err(err);
                }
            };
        let params_buffer = match self
            .create_host_buffer_with_data(vk::BufferUsageFlags::STORAGE_BUFFER, &params)
        {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_buffer(output_buffer);
                self.destroy_buffer(sample_index_buffer);
                return Err(err);
            }
        };

        let set_layouts = [program.descriptor_set_layout];
        let descriptor_pool_sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::STORAGE_BUFFER,
            descriptor_count: 4,
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
                self.destroy_buffer(params_buffer);
                self.destroy_buffer(output_buffer);
                self.destroy_buffer(sample_index_buffer);
                return Err(format!(
                    "create_descriptor_pool(f32 field sample probe) failed: {err:?}"
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
                    }
                    self.destroy_buffer(params_buffer);
                    self.destroy_buffer(output_buffer);
                    self.destroy_buffer(sample_index_buffer);
                    return Err(format!(
                        "allocate_descriptor_sets(f32 field sample probe) failed: {err:?}"
                    ));
                }
            }
        };

        let sdf_info = descriptor_buffer_info(
            &source_field.sdf_distances,
            source_field.sdf_distance_byte_len,
        );
        let sample_index_info = descriptor_buffer_info(&sample_index_buffer, sample_index_byte_len);
        let output_info = descriptor_buffer_info(&output_buffer, output_byte_len);
        let params_info = descriptor_buffer_info(&params_buffer, params_byte_len);
        let sdf_infos = [sdf_info];
        let sample_index_infos = [sample_index_info];
        let output_infos = [output_info];
        let params_infos = [params_info];
        let descriptor_writes = [
            write_descriptor(descriptor_set, 0, &sdf_infos),
            write_descriptor(descriptor_set, 1, &sample_index_infos),
            write_descriptor(descriptor_set, 2, &output_infos),
            write_descriptor(descriptor_set, 3, &params_infos),
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
                    }
                    self.destroy_buffer(params_buffer);
                    self.destroy_buffer(output_buffer);
                    self.destroy_buffer(sample_index_buffer);
                    return Err(format!(
                        "allocate_command_buffers(f32 field sample probe) failed: {err:?}"
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
                    }
                    self.destroy_buffer(params_buffer);
                    self.destroy_buffer(output_buffer);
                    self.destroy_buffer(sample_index_buffer);
                    return Err(format!(
                        "create_fence(f32 field sample probe) failed: {err:?}"
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
                        format!("begin_command_buffer(f32 field sample probe) failed: {e:?}")
                    })?;

                let input_barriers = [
                    vk::BufferMemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::HOST_READ)
                        .dst_access_mask(vk::AccessFlags::SHADER_READ)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .buffer(source_field.sdf_distances.buffer)
                        .offset(0)
                        .size(source_field.sdf_distance_byte_len),
                    host_to_compute_barrier(&sample_index_buffer, sample_index_byte_len),
                    host_to_compute_barrier(&params_buffer, params_byte_len),
                ];
                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::HOST,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &input_barriers,
                    &[],
                );

                self.device.cmd_bind_pipeline(
                    command_buffer,
                    vk::PipelineBindPoint::COMPUTE,
                    program.compute_pipeline,
                );
                self.device.cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::COMPUTE,
                    program.pipeline_layout,
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
                    .buffer(output_buffer.buffer)
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
                        format!("end_command_buffer(f32 field sample probe) failed: {e:?}")
                    })?;
                self.device
                    .queue_submit(
                        self.queue,
                        &[vk::SubmitInfo::default().command_buffers(&[command_buffer])],
                        fence,
                    )
                    .map_err(|e| format!("queue_submit(f32 field sample probe) failed: {e:?}"))?;
                self.gpu_submit_serial = self.gpu_submit_serial.saturating_add(1);
                queue_submit_serial = self.gpu_submit_serial;
                self.device
                    .wait_for_fences(&[fence], true, u64::MAX)
                    .map_err(|e| {
                        format!("wait_for_fences(f32 field sample probe) failed: {e:?}")
                    })?;
                self.gpu_completed_submit_serial =
                    self.gpu_completed_submit_serial.max(queue_submit_serial);
                self.collect_retired_texture_resources();
            }
            Ok(())
        })();

        let read_result = if command_result.is_ok() {
            unsafe {
                match self.device.map_memory(
                    output_buffer.memory,
                    0,
                    output_byte_len,
                    vk::MemoryMapFlags::empty(),
                ) {
                    Ok(mapped) => {
                        let rows = std::slice::from_raw_parts(
                            mapped as *const f32,
                            XR_GPU_F32_FIELD_SAMPLE_PROBE_SAMPLES,
                        );
                        let mut output_distances = [0.0; XR_GPU_F32_FIELD_SAMPLE_PROBE_SAMPLES];
                        output_distances.copy_from_slice(rows);
                        self.device.unmap_memory(output_buffer.memory);
                        Ok(output_distances)
                    }
                    Err(err) => Err(format!(
                        "map_memory(f32 field sample probe readback) failed: {err:?}"
                    )),
                }
            }
        } else {
            Err(command_result
                .err()
                .unwrap_or_else(|| "unknown f32 field sample probe command failure".to_string()))
        };

        let resource_generation = self.xr_f32_field_sample_probe_resources.len() as u64 + 1;
        self.xr_f32_field_sample_probe_resources
            .push(VulkanXrF32FieldSampleProbeResources {
                sample_indices: sample_index_buffer,
                output: output_buffer,
                params: params_buffer,
                descriptor_pool,
                command_buffer,
                fence,
            });
        let retained_resource_count = self.xr_f32_field_sample_probe_resources.len();
        let pending_retire_count = retained_resource_count;
        let output_distances = read_result?;

        let mut output_samples = [0.0; XR_GPU_F32_FIELD_SAMPLE_PROBE_SAMPLES];
        let mut expected_samples = [0.0; XR_GPU_F32_FIELD_SAMPLE_PROBE_SAMPLES];
        let mut mismatched_samples = 0;
        let mut max_abs_error = 0.0_f32;
        for index in 0..sample_count {
            let output = output_distances[index];
            let expected = expected_distances[index];
            output_samples[index] = output;
            expected_samples[index] = expected;
            let diff = (output - expected).abs();
            if !diff.is_finite() {
                max_abs_error = f32::INFINITY;
                mismatched_samples += 1;
            } else {
                max_abs_error = max_abs_error.max(diff);
                if diff > tolerance {
                    mismatched_samples += 1;
                }
            }
        }

        Ok(XrGpuF32FieldSampleProbeResult {
            sample_count,
            checked_sample_count: sample_count,
            sample_linear_indices: sample_indices,
            output_distances: output_samples,
            expected_distances: expected_samples,
            mismatched_samples,
            max_abs_error,
            tolerance,
            queue_submit_serial,
            fence_serial: queue_submit_serial,
            resource_generation,
            program_generation: program.generation,
            program_reused: program.program_reused,
            shader_compiled_this_submit: program.shader_compiled_this_submit,
            pipeline_created_this_submit: program.pipeline_created_this_submit,
            source_field_generation: source_field.generation,
            source_field_buffer_resident: true,
            source_field_buffer_bytes: source_field.sdf_distance_byte_len as u64,
            sample_index_buffer_bytes: sample_index_byte_len as u64,
            sample_output_buffer_bytes: output_byte_len as u64,
            pending_retire_count,
            retained_resource_count,
            retired_after_fence_count: 0,
            queue_wait_idle_performed: false,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    pub(super) fn destroy_xr_f32_field_sample_probe_resources(&mut self) {
        let resources = std::mem::take(&mut self.xr_f32_field_sample_probe_resources);
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
            }
            self.destroy_buffer(resource.params);
            self.destroy_buffer(resource.output);
            self.destroy_buffer(resource.sample_indices);
        }
    }

    pub(super) fn destroy_xr_f32_field_sample_probe_program(&mut self) {
        if let Some(program) = self.xr_f32_field_sample_probe_program.take() {
            unsafe {
                if program.compute_pipeline != vk::Pipeline::null() {
                    self.device.destroy_pipeline(program.compute_pipeline, None);
                }
                if program.pipeline_layout != vk::PipelineLayout::null() {
                    self.device
                        .destroy_pipeline_layout(program.pipeline_layout, None);
                }
                if program.descriptor_set_layout != vk::DescriptorSetLayout::null() {
                    self.device
                        .destroy_descriptor_set_layout(program.descriptor_set_layout, None);
                }
                if program.shader_module != vk::ShaderModule::null() {
                    self.device
                        .destroy_shader_module(program.shader_module, None);
                }
            }
        }
    }
}

fn descriptor_binding(binding: u32) -> vk::DescriptorSetLayoutBinding<'static> {
    vk::DescriptorSetLayoutBinding::default()
        .binding(binding)
        .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::COMPUTE)
}

fn descriptor_buffer_info(
    buffer: &VulkanBuffer,
    range: vk::DeviceSize,
) -> vk::DescriptorBufferInfo {
    vk::DescriptorBufferInfo::default()
        .buffer(buffer.buffer)
        .offset(0)
        .range(range)
}

fn write_descriptor<'a>(
    descriptor_set: vk::DescriptorSet,
    binding: u32,
    buffer_infos: &'a [vk::DescriptorBufferInfo],
) -> vk::WriteDescriptorSet<'a> {
    vk::WriteDescriptorSet::default()
        .dst_set(descriptor_set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
        .buffer_info(buffer_infos)
}

fn host_to_compute_barrier(
    buffer: &VulkanBuffer,
    size: vk::DeviceSize,
) -> vk::BufferMemoryBarrier<'static> {
    vk::BufferMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::HOST_WRITE)
        .dst_access_mask(vk::AccessFlags::SHADER_READ)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .buffer(buffer.buffer)
        .offset(0)
        .size(size)
}

fn create_compute_pipeline(
    cx: &CxVulkan,
    shader_module: vk::ShaderModule,
    pipeline_layout: vk::PipelineLayout,
    entry_name: &str,
) -> Result<vk::Pipeline, String> {
    let entry = std::ffi::CString::new(entry_name).unwrap();
    let stage = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(shader_module)
        .name(&entry);
    let compute_pipeline_info = vk::ComputePipelineCreateInfo::default()
        .stage(stage)
        .layout(pipeline_layout);
    match unsafe {
        cx.device.create_compute_pipelines(
            vk::PipelineCache::null(),
            &[compute_pipeline_info],
            None,
        )
    } {
        Ok(mut pipelines) => Ok(pipelines.remove(0)),
        Err((pipelines, err)) => {
            unsafe {
                for pipeline in pipelines {
                    if pipeline != vk::Pipeline::null() {
                        cx.device.destroy_pipeline(pipeline, None);
                    }
                }
            }
            Err(format!(
                "create_compute_pipelines(f32 field sample probe) failed: {err:?}"
            ))
        }
    }
}
