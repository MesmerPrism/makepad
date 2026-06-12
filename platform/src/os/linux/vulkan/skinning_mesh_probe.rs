use crate::{
    cx_api::{
        XrGpuF32SkinningMeshProbeResult, XrGpuF32SkinningMeshProbeTicket,
        XrGpuF32SkinningMeshVertex, XrGpuSkinningMeshTriangle,
        XR_GPU_F32_SKINNING_MESH_PROBE_SAMPLES,
    },
    os::linux::vulkan_naga::compile_compute_wgsl_to_spirv,
};
use ash::vk;
use std::time::Instant;

use super::{CxVulkan, VulkanBuffer};

const XR_GPU_F32_SKINNING_MESH_PROBE_ENTRY: &str = "compute_main";
const XR_GPU_F32_SKINNING_MESH_PROBE_WGSL: &str = r#"
struct SkinningMeshVertex {
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

@group(0) @binding(0) var<storage, read> vertices: array<SkinningMeshVertex>;
@group(0) @binding(1) var<storage, read> triangles: array<vec4<u32>>;
@group(0) @binding(2) var<storage, read_write> output_positions: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> triangle_observations: array<vec4<u32>>;
@group(0) @binding(4) var<storage, read> params: array<vec4<u32>, 1>;

fn skin_vertex(vertex: SkinningMeshVertex) -> vec4<f32> {
    let p = vertex.bind_position;
    let p0 = vec3<f32>(
        dot(vertex.matrix0_row0, p),
        dot(vertex.matrix0_row1, p),
        dot(vertex.matrix0_row2, p)
    );
    let p1 = vec3<f32>(
        dot(vertex.matrix1_row0, p),
        dot(vertex.matrix1_row1, p),
        dot(vertex.matrix1_row2, p)
    );
    let p2 = vec3<f32>(
        dot(vertex.matrix2_row0, p),
        dot(vertex.matrix2_row1, p),
        dot(vertex.matrix2_row2, p)
    );
    let p3 = vec3<f32>(
        dot(vertex.matrix3_row0, p),
        dot(vertex.matrix3_row1, p),
        dot(vertex.matrix3_row2, p)
    );
    let weights = vertex.joint_weights;
    let total_weight = weights.x + weights.y + weights.z + weights.w;
    if (total_weight > 0.0) {
        let weighted = p0 * weights.x + p1 * weights.y + p2 * weights.z + p3 * weights.w;
        return vec4<f32>(weighted / total_weight, 1.0);
    }
    return vertex.bind_position;
}

@compute @workgroup_size(64)
fn compute_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    let vertex_count = params[0].x;
    let triangle_count = params[0].y;
    if (index < vertex_count) {
        output_positions[index] = skin_vertex(vertices[index]);
    }
    if (index < triangle_count) {
        let tri = triangles[index];
        triangle_observations[index] = vec4<u32>(tri.x, tri.y, tri.z, tri.x ^ tri.y ^ tri.z);
    }
}
"#;

pub(super) struct VulkanXrF32SkinningMeshProbeResources {
    request_id: u64,
    started: Instant,
    vertex_count: usize,
    triangle_count: usize,
    sample_count: usize,
    sample_indices: [u32; XR_GPU_F32_SKINNING_MESH_PROBE_SAMPLES],
    expected_positions: Vec<[f32; 4]>,
    expected_triangles: Vec<[u32; 4]>,
    tolerance: f32,
    queue_submit_serial: u64,
    resource_generation: u64,
    completed: bool,
    vertices: VulkanBuffer,
    triangles: VulkanBuffer,
    output_positions: VulkanBuffer,
    triangle_observations: VulkanBuffer,
    params: VulkanBuffer,
    shader_module: vk::ShaderModule,
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    compute_pipeline: vk::Pipeline,
    descriptor_pool: vk::DescriptorPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
}

impl CxVulkan {
    pub(crate) fn submit_xr_f32_skinning_mesh_probe(
        &mut self,
        vertices: &[XrGpuF32SkinningMeshVertex],
        triangles: &[XrGpuSkinningMeshTriangle],
        sample_vertex_indices: [u32; XR_GPU_F32_SKINNING_MESH_PROBE_SAMPLES],
        sample_count: usize,
        tolerance: f32,
    ) -> Result<XrGpuF32SkinningMeshProbeResult, String> {
        let ticket = self.submit_xr_f32_skinning_mesh_probe_async(
            vertices,
            triangles,
            sample_vertex_indices,
            sample_count,
            tolerance,
        )?;
        self.wait_xr_f32_skinning_mesh_probe(ticket.request_id)
    }

    pub(crate) fn submit_xr_f32_skinning_mesh_probe_async(
        &mut self,
        vertices: &[XrGpuF32SkinningMeshVertex],
        triangles: &[XrGpuSkinningMeshTriangle],
        sample_vertex_indices: [u32; XR_GPU_F32_SKINNING_MESH_PROBE_SAMPLES],
        sample_count: usize,
        tolerance: f32,
    ) -> Result<XrGpuF32SkinningMeshProbeTicket, String> {
        let started = Instant::now();
        if vertices.is_empty() {
            return Err("full f32 skinning mesh probe requires vertices".to_string());
        }
        if triangles.is_empty() {
            return Err("full f32 skinning mesh probe requires triangles".to_string());
        }
        if vertices.len() > u32::MAX as usize || triangles.len() > u32::MAX as usize {
            return Err("full f32 skinning mesh probe exceeds u32 count range".to_string());
        }
        let vertex_count = vertices.len();
        let triangle_count = triangles.len();
        let sample_count = sample_count
            .min(XR_GPU_F32_SKINNING_MESH_PROBE_SAMPLES)
            .min(vertex_count);
        let tolerance = if tolerance.is_finite() && tolerance >= 0.0 {
            tolerance
        } else {
            0.0
        };
        let mut sample_indices = [0; XR_GPU_F32_SKINNING_MESH_PROBE_SAMPLES];
        for index in 0..sample_count {
            let requested = sample_vertex_indices[index] as usize;
            sample_indices[index] = if requested < vertex_count {
                sample_vertex_indices[index]
            } else {
                0
            };
        }
        let expected_positions = vertices
            .iter()
            .map(|vertex| vertex.expected_position)
            .collect::<Vec<_>>();
        let expected_triangles = triangles
            .iter()
            .map(|triangle| triangle.indices)
            .collect::<Vec<_>>();
        let params = [[vertex_count as u32, triangle_count as u32, 0, 0]];

        let vertex_byte_len = std::mem::size_of_val(vertices) as vk::DeviceSize;
        let triangle_byte_len = std::mem::size_of_val(triangles) as vk::DeviceSize;
        let output_position_byte_len =
            (std::mem::size_of::<[f32; 4]>() * vertex_count) as vk::DeviceSize;
        let triangle_observation_byte_len =
            (std::mem::size_of::<[u32; 4]>() * triangle_count) as vk::DeviceSize;
        let params_byte_len = std::mem::size_of_val(&params) as vk::DeviceSize;

        let shader_spv = compile_compute_wgsl_to_spirv(
            XR_GPU_F32_SKINNING_MESH_PROBE_WGSL,
            XR_GPU_F32_SKINNING_MESH_PROBE_ENTRY,
        )?;
        let vertex_buffer =
            self.create_host_buffer_with_data(vk::BufferUsageFlags::STORAGE_BUFFER, vertices)?;
        let triangle_buffer = match self
            .create_host_buffer_with_data(vk::BufferUsageFlags::STORAGE_BUFFER, triangles)
        {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_buffer(vertex_buffer);
                return Err(err);
            }
        };
        let output_positions = match self.create_host_buffer(
            vk::BufferUsageFlags::STORAGE_BUFFER,
            output_position_byte_len,
        ) {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(err);
            }
        };
        let triangle_observations = match self.create_host_buffer(
            vk::BufferUsageFlags::STORAGE_BUFFER,
            triangle_observation_byte_len,
        ) {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_buffer(output_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(err);
            }
        };
        let params_buffer = match self
            .create_host_buffer_with_data(vk::BufferUsageFlags::STORAGE_BUFFER, &params)
        {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_buffer(triangle_observations);
                self.destroy_buffer(output_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(err);
            }
        };

        let shader_module_info = vk::ShaderModuleCreateInfo::default().code(&shader_spv);
        let shader_module =
            match unsafe { self.device.create_shader_module(&shader_module_info, None) } {
                Ok(shader_module) => shader_module,
                Err(err) => {
                    self.destroy_buffer(params_buffer);
                    self.destroy_buffer(triangle_observations);
                    self.destroy_buffer(output_positions);
                    self.destroy_buffer(triangle_buffer);
                    self.destroy_buffer(vertex_buffer);
                    return Err(format!(
                        "create_shader_module(full f32 skinning mesh probe) failed: {err:?}"
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
            vk::DescriptorSetLayoutBinding::default()
                .binding(2)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
            vk::DescriptorSetLayoutBinding::default()
                .binding(3)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
            vk::DescriptorSetLayoutBinding::default()
                .binding(4)
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
                self.destroy_buffer(params_buffer);
                self.destroy_buffer(triangle_observations);
                self.destroy_buffer(output_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(format!(
                    "create_descriptor_set_layout(full f32 skinning mesh probe) failed: {err:?}"
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
                self.destroy_buffer(params_buffer);
                self.destroy_buffer(triangle_observations);
                self.destroy_buffer(output_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(format!(
                    "create_pipeline_layout(full f32 skinning mesh probe) failed: {err:?}"
                ));
            }
        };

        let entry = std::ffi::CString::new(XR_GPU_F32_SKINNING_MESH_PROBE_ENTRY).unwrap();
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
                self.destroy_buffer(params_buffer);
                self.destroy_buffer(triangle_observations);
                self.destroy_buffer(output_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(format!(
                    "create_compute_pipelines(full f32 skinning mesh probe) failed: {err:?}"
                ));
            }
        };

        let descriptor_pool_sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::STORAGE_BUFFER,
            descriptor_count: 5,
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
                self.destroy_buffer(params_buffer);
                self.destroy_buffer(triangle_observations);
                self.destroy_buffer(output_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(format!(
                    "create_descriptor_pool(full f32 skinning mesh probe) failed: {err:?}"
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
                    self.destroy_buffer(params_buffer);
                    self.destroy_buffer(triangle_observations);
                    self.destroy_buffer(output_positions);
                    self.destroy_buffer(triangle_buffer);
                    self.destroy_buffer(vertex_buffer);
                    return Err(format!(
                        "allocate_descriptor_sets(full f32 skinning mesh probe) failed: {err:?}"
                    ));
                }
            }
        };

        let vertex_buffer_info = vk::DescriptorBufferInfo::default()
            .buffer(vertex_buffer.buffer)
            .offset(0)
            .range(vertex_byte_len);
        let triangle_buffer_info = vk::DescriptorBufferInfo::default()
            .buffer(triangle_buffer.buffer)
            .offset(0)
            .range(triangle_byte_len);
        let output_positions_info = vk::DescriptorBufferInfo::default()
            .buffer(output_positions.buffer)
            .offset(0)
            .range(output_position_byte_len);
        let triangle_observations_info = vk::DescriptorBufferInfo::default()
            .buffer(triangle_observations.buffer)
            .offset(0)
            .range(triangle_observation_byte_len);
        let params_buffer_info = vk::DescriptorBufferInfo::default()
            .buffer(params_buffer.buffer)
            .offset(0)
            .range(params_byte_len);
        let vertex_buffer_infos = [vertex_buffer_info];
        let triangle_buffer_infos = [triangle_buffer_info];
        let output_positions_infos = [output_positions_info];
        let triangle_observations_infos = [triangle_observations_info];
        let params_buffer_infos = [params_buffer_info];
        let descriptor_writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&vertex_buffer_infos),
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&triangle_buffer_infos),
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&output_positions_infos),
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(3)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&triangle_observations_infos),
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(4)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&params_buffer_infos),
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
                    self.destroy_buffer(params_buffer);
                    self.destroy_buffer(triangle_observations);
                    self.destroy_buffer(output_positions);
                    self.destroy_buffer(triangle_buffer);
                    self.destroy_buffer(vertex_buffer);
                    return Err(format!(
                        "allocate_command_buffers(full f32 skinning mesh probe) failed: {err:?}"
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
                    self.destroy_buffer(params_buffer);
                    self.destroy_buffer(triangle_observations);
                    self.destroy_buffer(output_positions);
                    self.destroy_buffer(triangle_buffer);
                    self.destroy_buffer(vertex_buffer);
                    return Err(format!(
                        "create_fence(full f32 skinning mesh probe) failed: {err:?}"
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
                        format!("begin_command_buffer(full f32 skinning mesh probe) failed: {e:?}")
                    })?;

                let input_barriers = [
                    vk::BufferMemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::HOST_WRITE)
                        .dst_access_mask(vk::AccessFlags::SHADER_READ)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .buffer(vertex_buffer.buffer)
                        .offset(0)
                        .size(vertex_byte_len),
                    vk::BufferMemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::HOST_WRITE)
                        .dst_access_mask(vk::AccessFlags::SHADER_READ)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .buffer(triangle_buffer.buffer)
                        .offset(0)
                        .size(triangle_byte_len),
                    vk::BufferMemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::HOST_WRITE)
                        .dst_access_mask(vk::AccessFlags::SHADER_READ)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .buffer(params_buffer.buffer)
                        .offset(0)
                        .size(params_byte_len),
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
                let dispatch_count = vertex_count.max(triangle_count);
                let workgroups = (dispatch_count as u32).div_ceil(64).max(1);
                self.device.cmd_dispatch(command_buffer, workgroups, 1, 1);

                let output_barriers = [
                    vk::BufferMemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                        .dst_access_mask(vk::AccessFlags::HOST_READ)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .buffer(output_positions.buffer)
                        .offset(0)
                        .size(output_position_byte_len),
                    vk::BufferMemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                        .dst_access_mask(vk::AccessFlags::HOST_READ)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .buffer(triangle_observations.buffer)
                        .offset(0)
                        .size(triangle_observation_byte_len),
                ];
                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::PipelineStageFlags::HOST,
                    vk::DependencyFlags::empty(),
                    &[],
                    &output_barriers,
                    &[],
                );

                self.device
                    .end_command_buffer(command_buffer)
                    .map_err(|e| {
                        format!("end_command_buffer(full f32 skinning mesh probe) failed: {e:?}")
                    })?;
                self.device
                    .queue_submit(
                        self.queue,
                        &[vk::SubmitInfo::default().command_buffers(&[command_buffer])],
                        fence,
                    )
                    .map_err(|e| {
                        format!("queue_submit(full f32 skinning mesh probe) failed: {e:?}")
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
            self.destroy_buffer(params_buffer);
            self.destroy_buffer(triangle_observations);
            self.destroy_buffer(output_positions);
            self.destroy_buffer(triangle_buffer);
            self.destroy_buffer(vertex_buffer);
            return Err(err);
        }

        let resource_generation = self.xr_f32_skinning_mesh_probe_resources.len() as u64 + 1;
        let request_id = queue_submit_serial;
        self.xr_f32_skinning_mesh_probe_resources
            .push(VulkanXrF32SkinningMeshProbeResources {
                request_id,
                started,
                vertex_count,
                triangle_count,
                sample_count,
                sample_indices,
                expected_positions,
                expected_triangles,
                tolerance,
                queue_submit_serial,
                resource_generation,
                completed: false,
                vertices: vertex_buffer,
                triangles: triangle_buffer,
                output_positions,
                triangle_observations,
                params: params_buffer,
                shader_module,
                descriptor_set_layout,
                pipeline_layout,
                compute_pipeline,
                descriptor_pool,
                command_buffer,
                fence,
            });
        let retained_resource_count = self.xr_f32_skinning_mesh_probe_resources.len();
        let pending_retire_count = retained_resource_count;

        Ok(XrGpuF32SkinningMeshProbeTicket {
            request_id,
            queue_submit_serial,
            resource_generation,
            pending_retire_count,
            retained_resource_count,
        })
    }

    pub(crate) fn poll_xr_f32_skinning_mesh_probe(
        &mut self,
        request_id: u64,
    ) -> Result<Option<XrGpuF32SkinningMeshProbeResult>, String> {
        let Some(resource_index) = self
            .xr_f32_skinning_mesh_probe_resources
            .iter()
            .position(|resource| resource.request_id == request_id)
        else {
            return Ok(None);
        };
        if self.xr_f32_skinning_mesh_probe_resources[resource_index].completed {
            return Ok(None);
        }

        let fence = self.xr_f32_skinning_mesh_probe_resources[resource_index].fence;
        let queue_submit_serial =
            self.xr_f32_skinning_mesh_probe_resources[resource_index].queue_submit_serial;
        match unsafe { self.device.get_fence_status(fence) } {
            Ok(true) => {
                self.gpu_completed_submit_serial =
                    self.gpu_completed_submit_serial.max(queue_submit_serial);
                self.collect_retired_texture_resources();
                self.complete_xr_f32_skinning_mesh_probe(resource_index, false)
                    .map(Some)
            }
            Ok(false) => Ok(None),
            Err(err) => Err(format!(
                "get_fence_status(full f32 skinning mesh probe {request_id}) failed: {err:?}"
            )),
        }
    }

    fn wait_xr_f32_skinning_mesh_probe(
        &mut self,
        request_id: u64,
    ) -> Result<XrGpuF32SkinningMeshProbeResult, String> {
        let resource_index = self
            .xr_f32_skinning_mesh_probe_resources
            .iter()
            .position(|resource| resource.request_id == request_id)
            .ok_or_else(|| format!("full f32 skinning mesh probe {request_id} was not found"))?;
        if self.xr_f32_skinning_mesh_probe_resources[resource_index].completed {
            return Err(format!(
                "full f32 skinning mesh probe {request_id} was already completed"
            ));
        }

        let fence = self.xr_f32_skinning_mesh_probe_resources[resource_index].fence;
        let queue_submit_serial =
            self.xr_f32_skinning_mesh_probe_resources[resource_index].queue_submit_serial;
        unsafe {
            self.device
                .wait_for_fences(&[fence], true, u64::MAX)
                .map_err(|e| {
                    format!("wait_for_fences(full f32 skinning mesh probe) failed: {e:?}")
                })?;
            self.device.queue_wait_idle(self.queue).map_err(|e| {
                format!("queue_wait_idle(full f32 skinning mesh probe) failed: {e:?}")
            })?;
        }
        self.gpu_completed_submit_serial =
            self.gpu_completed_submit_serial.max(queue_submit_serial);
        self.collect_retired_texture_resources();
        self.complete_xr_f32_skinning_mesh_probe(resource_index, true)
    }

    fn complete_xr_f32_skinning_mesh_probe(
        &mut self,
        resource_index: usize,
        queue_wait_idle_performed: bool,
    ) -> Result<XrGpuF32SkinningMeshProbeResult, String> {
        let (
            started,
            vertex_count,
            triangle_count,
            sample_count,
            sample_indices,
            expected_positions,
            expected_triangles,
            tolerance,
            queue_submit_serial,
            resource_generation,
            output_positions_buffer,
            triangle_observations_buffer,
        ) = {
            let resource = self
                .xr_f32_skinning_mesh_probe_resources
                .get(resource_index)
                .ok_or_else(|| {
                    "full f32 skinning mesh probe resource index is stale".to_string()
                })?;
            if resource.completed {
                return Err(format!(
                    "full f32 skinning mesh probe {} was already completed",
                    resource.request_id
                ));
            }
            (
                resource.started,
                resource.vertex_count,
                resource.triangle_count,
                resource.sample_count,
                resource.sample_indices,
                resource.expected_positions.clone(),
                resource.expected_triangles.clone(),
                resource.tolerance,
                resource.queue_submit_serial,
                resource.resource_generation,
                resource.output_positions,
                resource.triangle_observations,
            )
        };
        let output_position_byte_len =
            (std::mem::size_of::<[f32; 4]>() * vertex_count) as vk::DeviceSize;
        let triangle_observation_byte_len =
            (std::mem::size_of::<[u32; 4]>() * triangle_count) as vk::DeviceSize;

        let output_positions = unsafe {
            let mapped_positions = self
                .device
                .map_memory(
                    output_positions_buffer.memory,
                    0,
                    output_position_byte_len,
                    vk::MemoryMapFlags::empty(),
                )
                .map_err(|err| {
                    format!("map_memory(full f32 skinning mesh position readback) failed: {err:?}")
                })?;
            let position_rows =
                std::slice::from_raw_parts(mapped_positions as *const [f32; 4], vertex_count);
            let output_position_rows = position_rows.to_vec();
            self.device.unmap_memory(output_positions_buffer.memory);
            output_position_rows
        };

        let triangle_observations = unsafe {
            let mapped_triangles = self
                .device
                .map_memory(
                    triangle_observations_buffer.memory,
                    0,
                    triangle_observation_byte_len,
                    vk::MemoryMapFlags::empty(),
                )
                .map_err(|err| {
                    format!("map_memory(full f32 skinning mesh triangle readback) failed: {err:?}")
                })?;
            let triangle_rows =
                std::slice::from_raw_parts(mapped_triangles as *const [u32; 4], triangle_count);
            let triangle_observation_rows = triangle_rows.to_vec();
            self.device
                .unmap_memory(triangle_observations_buffer.memory);
            triangle_observation_rows
        };

        let mut mismatched_position_components = 0;
        let mut max_abs_error = 0.0_f32;
        for (index, output) in output_positions.iter().copied().enumerate() {
            let expected = expected_positions[index];
            for component in 0..3 {
                let diff = (output[component] - expected[component]).abs();
                if !diff.is_finite() {
                    max_abs_error = f32::INFINITY;
                    mismatched_position_components += 1;
                } else {
                    max_abs_error = max_abs_error.max(diff);
                    if diff > tolerance {
                        mismatched_position_components += 1;
                    }
                }
            }
        }

        let mut mismatched_triangle_indices = 0;
        for (index, observed) in triangle_observations.iter().copied().enumerate() {
            let expected = expected_triangles[index];
            for component in 0..4 {
                if observed[component] != expected[component] {
                    mismatched_triangle_indices += 1;
                }
            }
        }

        let mut output_sample_positions = [[0.0; 4]; XR_GPU_F32_SKINNING_MESH_PROBE_SAMPLES];
        let mut expected_sample_positions = [[0.0; 4]; XR_GPU_F32_SKINNING_MESH_PROBE_SAMPLES];
        for index in 0..sample_count {
            let vertex_index = sample_indices[index] as usize;
            output_sample_positions[index] = output_positions[vertex_index];
            expected_sample_positions[index] = expected_positions[vertex_index];
        }

        if let Some(resource) = self
            .xr_f32_skinning_mesh_probe_resources
            .get_mut(resource_index)
        {
            resource.completed = true;
        }
        let retained_resource_count = self.xr_f32_skinning_mesh_probe_resources.len();
        let pending_retire_count = self
            .xr_f32_skinning_mesh_probe_resources
            .iter()
            .filter(|resource| !resource.completed)
            .count();

        Ok(XrGpuF32SkinningMeshProbeResult {
            vertex_count,
            triangle_count,
            index_count: triangle_count * 3,
            sample_count,
            sample_vertex_indices: sample_indices,
            output_sample_positions,
            expected_sample_positions,
            checked_position_components: vertex_count * 3,
            mismatched_position_components,
            mismatched_triangle_indices,
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

    pub(super) fn destroy_xr_f32_skinning_mesh_probe_resources(&mut self) {
        let resources = std::mem::take(&mut self.xr_f32_skinning_mesh_probe_resources);
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
            self.destroy_buffer(resource.params);
            self.destroy_buffer(resource.triangle_observations);
            self.destroy_buffer(resource.output_positions);
            self.destroy_buffer(resource.triangles);
            self.destroy_buffer(resource.vertices);
        }
    }
}
