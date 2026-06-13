use crate::{
    cx_api::{
        XrGpuF32ForceProbeResult, XrGpuF32ForceProbeSample, XrGpuStorageBufferProbeResult,
        XrGpuU32ComputeProbeResult, XR_GPU_F32_FORCE_PROBE_SAMPLES, XR_GPU_U32_COMPUTE_PROBE_WORDS,
    },
    os::linux::vulkan_naga::compile_compute_wgsl_to_spirv,
};
use ash::vk;
use std::time::Instant;

use super::{CxVulkan, VulkanBuffer};

const XR_GPU_U32_COMPUTE_PROBE_ENTRY: &str = "compute_main";
const XR_GPU_U32_COMPUTE_PROBE_XOR_A: u32 = 0xA5A5_5A5A;
const XR_GPU_U32_COMPUTE_PROBE_XOR_B: u32 = 0x5DF0_ADF1;
const XR_GPU_U32_COMPUTE_PROBE_INDEX_STEP: u32 = 17;
const XR_GPU_U32_COMPUTE_PROBE_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read> input_words: array<u32, 4>;
@group(0) @binding(1) var<storage, read_write> output_words: array<u32, 4>;

@compute @workgroup_size(4)
fn compute_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if (index < 4u) {
        let mixed = (input_words[index] ^ 2779077210u) + (index * 17u);
        output_words[index] = mixed ^ 1576054257u;
    }
}
"#;
const XR_GPU_F32_FORCE_PROBE_ENTRY: &str = "compute_main";
const XR_GPU_F32_FORCE_PROBE_WGSL: &str = r#"
struct ForceProbeSample {
    position_radius: vec4<f32>,
    distance_target_strength: vec4<f32>,
    outward: vec4<f32>,
    expected_acceleration: vec4<f32>,
};

@group(0) @binding(0) var<storage, read> input_samples: array<ForceProbeSample, 4>;
@group(0) @binding(1) var<storage, read_write> output_accelerations: array<vec4<f32>, 4>;

@compute @workgroup_size(4)
fn compute_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if (index < 4u) {
        let sample = input_samples[index];
        let distance = sample.distance_target_strength.x;
        let target_distance = sample.distance_target_strength.y;
        let strength = sample.distance_target_strength.z;
        let error = distance - target_distance;
        let scale = -error * strength;
        output_accelerations[index] = vec4<f32>(sample.outward.xyz * scale, 0.0);
    }
}
"#;
fn expected_xr_gpu_u32_compute_probe_words(
    input_words: [u32; XR_GPU_U32_COMPUTE_PROBE_WORDS],
) -> [u32; XR_GPU_U32_COMPUTE_PROBE_WORDS] {
    let mut expected = [0; XR_GPU_U32_COMPUTE_PROBE_WORDS];
    for (index, word) in input_words.iter().copied().enumerate() {
        expected[index] = (word ^ XR_GPU_U32_COMPUTE_PROBE_XOR_A)
            .wrapping_add((index as u32).wrapping_mul(XR_GPU_U32_COMPUTE_PROBE_INDEX_STEP))
            ^ XR_GPU_U32_COMPUTE_PROBE_XOR_B;
    }
    expected
}

fn expected_xr_gpu_f32_force_probe_accelerations(
    samples: [XrGpuF32ForceProbeSample; XR_GPU_F32_FORCE_PROBE_SAMPLES],
) -> [[f32; 4]; XR_GPU_F32_FORCE_PROBE_SAMPLES] {
    let mut expected = [[0.0; 4]; XR_GPU_F32_FORCE_PROBE_SAMPLES];
    for (index, sample) in samples.iter().copied().enumerate() {
        expected[index] = sample.expected_acceleration;
    }
    expected
}

pub(super) struct VulkanXrU32ComputeProbeResources {
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

pub(super) struct VulkanXrF32ForceProbeResources {
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

pub(super) struct VulkanXrStorageBufferProbeResources {
    storage: VulkanBuffer,
    readback: VulkanBuffer,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
}

impl CxVulkan {
    pub(crate) fn submit_xr_storage_buffer_probe(
        &mut self,
        requested_bytes: usize,
        pattern: u32,
    ) -> Result<XrGpuStorageBufferProbeResult, String> {
        let started = Instant::now();
        let byte_len = Self::align_device_size(requested_bytes.max(4) as vk::DeviceSize, 4);
        let word_count = (byte_len / 4) as usize;
        let storage = self.create_host_buffer(
            vk::BufferUsageFlags::STORAGE_BUFFER
                | vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST,
            byte_len,
        )?;
        let readback = match self.create_host_buffer(vk::BufferUsageFlags::TRANSFER_DST, byte_len) {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_buffer(storage);
                return Err(err);
            }
        };

        let command_buffer = {
            let alloc_info = vk::CommandBufferAllocateInfo::default()
                .command_pool(self.command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            match unsafe { self.device.allocate_command_buffers(&alloc_info) } {
                Ok(buffers) => buffers[0],
                Err(err) => {
                    self.destroy_buffer(readback);
                    self.destroy_buffer(storage);
                    return Err(format!(
                        "allocate_command_buffers(storage probe) failed: {err:?}"
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
                    }
                    self.destroy_buffer(readback);
                    self.destroy_buffer(storage);
                    return Err(format!("create_fence(storage probe) failed: {err:?}"));
                }
            }
        };

        let command_result = (|| -> Result<(), String> {
            unsafe {
                self.device
                    .begin_command_buffer(
                        command_buffer,
                        &vk::CommandBufferBeginInfo::default()
                            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                    )
                    .map_err(|e| format!("begin_command_buffer(storage probe) failed: {e:?}"))?;
                self.device
                    .cmd_fill_buffer(command_buffer, storage.buffer, 0, byte_len, pattern);

                let storage_barrier = vk::BufferMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(storage.buffer)
                    .offset(0)
                    .size(byte_len);
                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[storage_barrier],
                    &[],
                );

                let copy_region = vk::BufferCopy::default()
                    .src_offset(0)
                    .dst_offset(0)
                    .size(byte_len);
                self.device.cmd_copy_buffer(
                    command_buffer,
                    storage.buffer,
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
                    .size(byte_len);
                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::HOST,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[readback_barrier],
                    &[],
                );

                self.device
                    .end_command_buffer(command_buffer)
                    .map_err(|e| format!("end_command_buffer(storage probe) failed: {e:?}"))?;
                self.device
                    .queue_submit(
                        self.queue,
                        &[vk::SubmitInfo::default().command_buffers(&[command_buffer])],
                        fence,
                    )
                    .map_err(|e| format!("queue_submit(storage probe) failed: {e:?}"))?;
                self.device
                    .wait_for_fences(&[fence], true, u64::MAX)
                    .map_err(|e| format!("wait_for_fences(storage probe) failed: {e:?}"))?;
            }
            Ok(())
        })();

        let read_result = if command_result.is_ok() {
            unsafe {
                match self.device.map_memory(
                    readback.memory,
                    0,
                    byte_len,
                    vk::MemoryMapFlags::empty(),
                ) {
                    Ok(mapped) => {
                        let words = std::slice::from_raw_parts(mapped as *const u32, word_count);
                        let first_word = words.first().copied().unwrap_or(0);
                        let mismatched_words =
                            words.iter().filter(|word| **word != pattern).count();
                        self.device.unmap_memory(readback.memory);
                        Ok((first_word, mismatched_words))
                    }
                    Err(err) => Err(format!(
                        "map_memory(storage probe readback) failed: {err:?}"
                    )),
                }
            }
        } else {
            Err(command_result
                .err()
                .unwrap_or_else(|| "unknown storage probe command failure".to_string()))
        };

        self.xr_storage_buffer_probe_resources
            .push(VulkanXrStorageBufferProbeResources {
                storage,
                readback,
                command_buffer,
                fence,
            });

        let (first_word, mismatched_words) = read_result?;
        Ok(XrGpuStorageBufferProbeResult {
            requested_bytes,
            storage_buffer_bytes: byte_len as usize,
            readback_bytes: byte_len as usize,
            pattern,
            first_word,
            word_count,
            mismatched_words,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    pub(crate) fn submit_xr_u32_compute_probe(
        &mut self,
        input_words: [u32; XR_GPU_U32_COMPUTE_PROBE_WORDS],
    ) -> Result<XrGpuU32ComputeProbeResult, String> {
        let started = Instant::now();
        let mut queue_submit_serial = 0;
        let mut fence_serial = 0;
        let mut queue_wait_idle_performed = false;
        let expected_words = expected_xr_gpu_u32_compute_probe_words(input_words);
        let byte_len =
            std::mem::size_of::<[u32; XR_GPU_U32_COMPUTE_PROBE_WORDS]>() as vk::DeviceSize;
        let shader_spv = compile_compute_wgsl_to_spirv(
            XR_GPU_U32_COMPUTE_PROBE_WGSL,
            XR_GPU_U32_COMPUTE_PROBE_ENTRY,
        )?;
        let input =
            self.create_host_buffer_with_data(vk::BufferUsageFlags::STORAGE_BUFFER, &input_words)?;
        let output = match self.create_host_buffer(vk::BufferUsageFlags::STORAGE_BUFFER, byte_len) {
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
                        "create_shader_module(u32 compute probe) failed: {err:?}"
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
                    "create_descriptor_set_layout(u32 compute probe) failed: {err:?}"
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
                    "create_pipeline_layout(u32 compute probe) failed: {err:?}"
                ));
            }
        };

        let entry = std::ffi::CString::new(XR_GPU_U32_COMPUTE_PROBE_ENTRY).unwrap();
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
                    "create_compute_pipelines(u32 compute probe) failed: {err:?}"
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
                    "create_descriptor_pool(u32 compute probe) failed: {err:?}"
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
                        "allocate_descriptor_sets(u32 compute probe) failed: {err:?}"
                    ));
                }
            }
        };

        let input_buffer_info = vk::DescriptorBufferInfo::default()
            .buffer(input.buffer)
            .offset(0)
            .range(byte_len);
        let output_buffer_info = vk::DescriptorBufferInfo::default()
            .buffer(output.buffer)
            .offset(0)
            .range(byte_len);
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
                        "allocate_command_buffers(u32 compute probe) failed: {err:?}"
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
                    return Err(format!("create_fence(u32 compute probe) failed: {err:?}"));
                }
            }
        };

        let command_result = (|| -> Result<(), String> {
            unsafe {
                self.device
                    .begin_command_buffer(
                        command_buffer,
                        &vk::CommandBufferBeginInfo::default()
                            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                    )
                    .map_err(|e| {
                        format!("begin_command_buffer(u32 compute probe) failed: {e:?}")
                    })?;

                let input_barrier = vk::BufferMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::HOST_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(input.buffer)
                    .offset(0)
                    .size(byte_len);
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
                    .size(byte_len);
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
                    .map_err(|e| format!("end_command_buffer(u32 compute probe) failed: {e:?}"))?;
                self.device
                    .queue_submit(
                        self.queue,
                        &[vk::SubmitInfo::default().command_buffers(&[command_buffer])],
                        fence,
                    )
                    .map_err(|e| format!("queue_submit(u32 compute probe) failed: {e:?}"))?;
                self.gpu_submit_serial = self.gpu_submit_serial.saturating_add(1);
                queue_submit_serial = self.gpu_submit_serial;
                self.device
                    .wait_for_fences(&[fence], true, u64::MAX)
                    .map_err(|e| format!("wait_for_fences(u32 compute probe) failed: {e:?}"))?;
                fence_serial = queue_submit_serial;
                self.device
                    .queue_wait_idle(self.queue)
                    .map_err(|e| format!("queue_wait_idle(u32 compute probe) failed: {e:?}"))?;
                queue_wait_idle_performed = true;
                self.gpu_completed_submit_serial =
                    self.gpu_completed_submit_serial.max(queue_submit_serial);
                self.collect_retired_texture_resources();
            }
            Ok(())
        })();

        let read_result = if command_result.is_ok() {
            unsafe {
                match self.device.map_memory(
                    output.memory,
                    0,
                    byte_len,
                    vk::MemoryMapFlags::empty(),
                ) {
                    Ok(mapped) => {
                        let words = std::slice::from_raw_parts(
                            mapped as *const u32,
                            XR_GPU_U32_COMPUTE_PROBE_WORDS,
                        );
                        let mut output_words = [0; XR_GPU_U32_COMPUTE_PROBE_WORDS];
                        output_words.copy_from_slice(words);
                        self.device.unmap_memory(output.memory);
                        Ok(output_words)
                    }
                    Err(err) => Err(format!(
                        "map_memory(u32 compute probe readback) failed: {err:?}"
                    )),
                }
            }
        } else {
            Err(command_result
                .err()
                .unwrap_or_else(|| "unknown u32 compute probe command failure".to_string()))
        };

        let resource_generation = self.xr_u32_compute_probe_resources.len() as u64 + 1;
        self.xr_u32_compute_probe_resources
            .push(VulkanXrU32ComputeProbeResources {
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
        let retained_resource_count = self.xr_u32_compute_probe_resources.len();
        let pending_retire_count = retained_resource_count;
        let retired_after_fence_count = 0;

        let output_words = read_result?;
        let mismatched_words = output_words
            .iter()
            .zip(expected_words.iter())
            .filter(|(output, expected)| output != expected)
            .count();
        Ok(XrGpuU32ComputeProbeResult {
            input_words,
            output_words,
            expected_words,
            word_count: XR_GPU_U32_COMPUTE_PROBE_WORDS,
            mismatched_words,
            queue_submit_serial,
            fence_serial,
            resource_generation,
            pending_retire_count,
            retained_resource_count,
            retired_after_fence_count,
            queue_wait_idle_performed,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    pub(crate) fn submit_xr_f32_force_probe(
        &mut self,
        samples: [XrGpuF32ForceProbeSample; XR_GPU_F32_FORCE_PROBE_SAMPLES],
        sample_count: usize,
        tolerance: f32,
    ) -> Result<XrGpuF32ForceProbeResult, String> {
        let started = Instant::now();
        let mut queue_submit_serial = 0;
        let mut fence_serial = 0;
        let mut queue_wait_idle_performed = false;
        let sample_count = sample_count.min(XR_GPU_F32_FORCE_PROBE_SAMPLES);
        let tolerance = if tolerance.is_finite() && tolerance >= 0.0 {
            tolerance
        } else {
            0.0
        };
        let expected_accelerations = expected_xr_gpu_f32_force_probe_accelerations(samples);
        let input_byte_len = std::mem::size_of::<
            [XrGpuF32ForceProbeSample; XR_GPU_F32_FORCE_PROBE_SAMPLES],
        >() as vk::DeviceSize;
        let output_byte_len =
            std::mem::size_of::<[[f32; 4]; XR_GPU_F32_FORCE_PROBE_SAMPLES]>() as vk::DeviceSize;
        let shader_spv = compile_compute_wgsl_to_spirv(
            XR_GPU_F32_FORCE_PROBE_WGSL,
            XR_GPU_F32_FORCE_PROBE_ENTRY,
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
                        "create_shader_module(f32 force probe) failed: {err:?}"
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
                    "create_descriptor_set_layout(f32 force probe) failed: {err:?}"
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
                    "create_pipeline_layout(f32 force probe) failed: {err:?}"
                ));
            }
        };

        let entry = std::ffi::CString::new(XR_GPU_F32_FORCE_PROBE_ENTRY).unwrap();
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
                    "create_compute_pipelines(f32 force probe) failed: {err:?}"
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
                    "create_descriptor_pool(f32 force probe) failed: {err:?}"
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
                        "allocate_descriptor_sets(f32 force probe) failed: {err:?}"
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
                        "allocate_command_buffers(f32 force probe) failed: {err:?}"
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
                    return Err(format!("create_fence(f32 force probe) failed: {err:?}"));
                }
            }
        };

        let command_result = (|| -> Result<(), String> {
            unsafe {
                self.device
                    .begin_command_buffer(
                        command_buffer,
                        &vk::CommandBufferBeginInfo::default()
                            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                    )
                    .map_err(|e| format!("begin_command_buffer(f32 force probe) failed: {e:?}"))?;

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
                    .map_err(|e| format!("end_command_buffer(f32 force probe) failed: {e:?}"))?;
                self.device
                    .queue_submit(
                        self.queue,
                        &[vk::SubmitInfo::default().command_buffers(&[command_buffer])],
                        fence,
                    )
                    .map_err(|e| format!("queue_submit(f32 force probe) failed: {e:?}"))?;
                self.gpu_submit_serial = self.gpu_submit_serial.saturating_add(1);
                queue_submit_serial = self.gpu_submit_serial;
                self.device
                    .wait_for_fences(&[fence], true, u64::MAX)
                    .map_err(|e| format!("wait_for_fences(f32 force probe) failed: {e:?}"))?;
                fence_serial = queue_submit_serial;
                self.device
                    .queue_wait_idle(self.queue)
                    .map_err(|e| format!("queue_wait_idle(f32 force probe) failed: {e:?}"))?;
                queue_wait_idle_performed = true;
                self.gpu_completed_submit_serial =
                    self.gpu_completed_submit_serial.max(queue_submit_serial);
                self.collect_retired_texture_resources();
            }
            Ok(())
        })();

        let read_result = if command_result.is_ok() {
            unsafe {
                match self.device.map_memory(
                    output.memory,
                    0,
                    output_byte_len,
                    vk::MemoryMapFlags::empty(),
                ) {
                    Ok(mapped) => {
                        let rows = std::slice::from_raw_parts(
                            mapped as *const [f32; 4],
                            XR_GPU_F32_FORCE_PROBE_SAMPLES,
                        );
                        let mut output_accelerations = [[0.0; 4]; XR_GPU_F32_FORCE_PROBE_SAMPLES];
                        output_accelerations.copy_from_slice(rows);
                        self.device.unmap_memory(output.memory);
                        Ok(output_accelerations)
                    }
                    Err(err) => Err(format!(
                        "map_memory(f32 force probe readback) failed: {err:?}"
                    )),
                }
            }
        } else {
            Err(command_result
                .err()
                .unwrap_or_else(|| "unknown f32 force probe command failure".to_string()))
        };

        let resource_generation = self.xr_f32_force_probe_resources.len() as u64 + 1;
        self.xr_f32_force_probe_resources
            .push(VulkanXrF32ForceProbeResources {
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
        let retained_resource_count = self.xr_f32_force_probe_resources.len();
        let pending_retire_count = retained_resource_count;
        let retired_after_fence_count = 0;

        let output_accelerations = read_result?;
        let mut mismatched_components = 0;
        let mut max_abs_error = 0.0_f32;
        for index in 0..sample_count {
            for component in 0..3 {
                let diff = (output_accelerations[index][component]
                    - expected_accelerations[index][component])
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

        Ok(XrGpuF32ForceProbeResult {
            samples,
            output_accelerations,
            expected_accelerations,
            sample_count,
            component_count: sample_count * 3,
            mismatched_components,
            max_abs_error,
            tolerance,
            queue_submit_serial,
            fence_serial,
            resource_generation,
            pending_retire_count,
            retained_resource_count,
            retired_after_fence_count,
            queue_wait_idle_performed,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    pub(super) fn destroy_xr_u32_compute_probe_resources(&mut self) {
        let resources = std::mem::take(&mut self.xr_u32_compute_probe_resources);
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

    pub(super) fn destroy_xr_f32_force_probe_resources(&mut self) {
        let resources = std::mem::take(&mut self.xr_f32_force_probe_resources);
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

    pub(super) fn destroy_xr_storage_buffer_probe_resources(&mut self) {
        let resources = std::mem::take(&mut self.xr_storage_buffer_probe_resources);
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
            }
            self.destroy_buffer(resource.readback);
            self.destroy_buffer(resource.storage);
        }
    }
}
