use crate::{
    cx_api::{
        XrGpuF32MeshSdfProbeGrid, XrGpuF32MeshSdfProbeResult, XrGpuF32SkinningMeshVertex,
        XrGpuSkinningMeshTriangle, XR_GPU_F32_MESH_SDF_PROBE_SAMPLES,
    },
    os::linux::vulkan_naga::compile_compute_wgsl_to_spirv,
};
use ash::vk;
use std::time::Instant;

use super::{CxVulkan, VulkanBuffer};

const XR_GPU_F32_MESH_SDF_SKINNING_ENTRY: &str = "skin_main";
const XR_GPU_F32_MESH_SDF_BUILD_ENTRY: &str = "sdf_main";

const XR_GPU_F32_MESH_SDF_SKINNING_WGSL: &str = r#"
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
@group(0) @binding(2) var<storage, read_write> skinned_positions: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> params: array<vec4<u32>, 2>;

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
fn skin_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    let vertex_count = params[0].x;
    if (index < vertex_count) {
        skinned_positions[index] = skin_vertex(vertices[index]);
    }
}
"#;

const XR_GPU_F32_MESH_SDF_BUILD_WGSL: &str = r#"
@group(0) @binding(1) var<storage, read> triangles: array<vec4<u32>>;
@group(0) @binding(2) var<storage, read> skinned_positions: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> sdf_distances: array<f32>;
@group(0) @binding(4) var<storage, read> params: array<vec4<u32>, 2>;
@group(0) @binding(5) var<storage, read> grid: array<vec4<f32>, 1>;

fn closest_point_on_triangle(point: vec3<f32>, a: vec3<f32>, b: vec3<f32>, c: vec3<f32>) -> vec3<f32> {
    let ab = b - a;
    let ac = c - a;
    let ap = point - a;
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if (d1 <= 0.0 && d2 <= 0.0) {
        return a;
    }

    let bp = point - b;
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if (d3 >= 0.0 && d4 <= d3) {
        return b;
    }

    let vc = d1 * d4 - d3 * d2;
    if (vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0) {
        let v = d1 / (d1 - d3);
        return a + ab * v;
    }

    let cp = point - c;
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if (d6 >= 0.0 && d5 <= d6) {
        return c;
    }

    let vb = d5 * d2 - d1 * d6;
    if (vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0) {
        let w = d2 / (d2 - d6);
        return a + ac * w;
    }

    let va = d3 * d6 - d5 * d4;
    if (va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0) {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return b + (c - b) * w;
    }

    let denominator = 1.0 / (va + vb + vc);
    let v = vb * denominator;
    let w = vc * denominator;
    return a + ab * v + ac * w;
}

fn cell_center(linear_index: u32) -> vec3<f32> {
    let width = params[0].w;
    let height = params[1].x;
    let plane = width * height;
    let z = linear_index / plane;
    let remainder = linear_index - z * plane;
    let y = remainder / width;
    let x = remainder - y * width;
    let grid_row = grid[0];
    let origin = grid_row.xyz;
    let voxel_size = grid_row.w;
    return origin + vec3<f32>(
        (f32(x) + 0.5) * voxel_size,
        (f32(y) + 0.5) * voxel_size,
        (f32(z) + 0.5) * voxel_size
    );
}

@compute @workgroup_size(64)
fn sdf_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let linear_index = id.x;
    let vertex_count = params[0].x;
    let triangle_count = params[0].y;
    let voxel_count = params[0].z;
    if (linear_index >= voxel_count) {
        return;
    }

    let point = cell_center(linear_index);
    var best_distance_squared = 3.4028234663852886e38;
    var best_closest = vec3<f32>(0.0, 0.0, 0.0);
    var best_normal = vec3<f32>(0.0, 0.0, 1.0);
    var found = false;

    for (var triangle_index = 0u; triangle_index < triangle_count; triangle_index = triangle_index + 1u) {
        let tri = triangles[triangle_index];
        if (tri.x < vertex_count && tri.y < vertex_count && tri.z < vertex_count) {
            let a = skinned_positions[tri.x].xyz;
            let b = skinned_positions[tri.y].xyz;
            let c = skinned_positions[tri.z].xyz;
            let normal_raw = cross(b - a, c - a);
            let normal_length = length(normal_raw);
            if (normal_length > 0.0000001) {
                let normal = normal_raw / normal_length;
                let closest = closest_point_on_triangle(point, a, b, c);
                let delta = point - closest;
                let distance_squared = dot(delta, delta);
                if (distance_squared < best_distance_squared) {
                    best_distance_squared = distance_squared;
                    best_closest = closest;
                    best_normal = normal;
                    found = true;
                }
            }
        }
    }

    if (found) {
        let distance = sqrt(best_distance_squared);
        if (dot(point - best_closest, best_normal) < 0.0) {
            sdf_distances[linear_index] = -distance;
        } else {
            sdf_distances[linear_index] = distance;
        }
    } else {
        sdf_distances[linear_index] = 0.0;
    }
}
"#;

pub(super) struct VulkanXrF32MeshSdfProbeResources {
    vertices: VulkanBuffer,
    triangles: VulkanBuffer,
    skinned_positions: VulkanBuffer,
    sdf_distances: VulkanBuffer,
    params: VulkanBuffer,
    grid: VulkanBuffer,
    skin_shader_module: vk::ShaderModule,
    sdf_shader_module: vk::ShaderModule,
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    skin_pipeline: vk::Pipeline,
    sdf_pipeline: vk::Pipeline,
    descriptor_pool: vk::DescriptorPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
}

impl CxVulkan {
    pub(crate) fn submit_xr_f32_mesh_sdf_probe(
        &mut self,
        vertices: &[XrGpuF32SkinningMeshVertex],
        triangles: &[XrGpuSkinningMeshTriangle],
        grid: XrGpuF32MeshSdfProbeGrid,
        sample_linear_indices: [u32; XR_GPU_F32_MESH_SDF_PROBE_SAMPLES],
        expected_distances: [f32; XR_GPU_F32_MESH_SDF_PROBE_SAMPLES],
        sample_count: usize,
        tolerance: f32,
    ) -> Result<XrGpuF32MeshSdfProbeResult, String> {
        let started = Instant::now();
        if vertices.is_empty() {
            return Err("f32 mesh SDF probe requires vertices".to_string());
        }
        if triangles.is_empty() {
            return Err("f32 mesh SDF probe requires triangles".to_string());
        }
        if vertices.len() > u32::MAX as usize || triangles.len() > u32::MAX as usize {
            return Err("f32 mesh SDF probe exceeds u32 count range".to_string());
        }
        let dimensions = grid.dimensions;
        if dimensions[0] == 0 || dimensions[1] == 0 || dimensions[2] == 0 {
            return Err("f32 mesh SDF probe requires nonzero grid dimensions".to_string());
        }
        let voxel_count = (dimensions[0] as usize)
            .checked_mul(dimensions[1] as usize)
            .and_then(|count| count.checked_mul(dimensions[2] as usize))
            .ok_or_else(|| "f32 mesh SDF probe voxel count overflow".to_string())?;
        if voxel_count == 0 || voxel_count > u32::MAX as usize {
            return Err("f32 mesh SDF probe voxel count exceeds u32 range".to_string());
        }
        if !grid.origin_voxel_size.iter().copied().all(f32::is_finite)
            || grid.origin_voxel_size[3] <= 0.0
        {
            return Err("f32 mesh SDF probe requires finite positive voxel size".to_string());
        }
        let vertex_count = vertices.len();
        let triangle_count = triangles.len();
        let sample_count = sample_count
            .min(XR_GPU_F32_MESH_SDF_PROBE_SAMPLES)
            .min(voxel_count);
        if sample_count == 0 {
            return Err("f32 mesh SDF probe requires samples".to_string());
        }
        let tolerance = if tolerance.is_finite() && tolerance >= 0.0 {
            tolerance
        } else {
            0.0
        };
        let mut sample_indices = [0; XR_GPU_F32_MESH_SDF_PROBE_SAMPLES];
        for index in 0..sample_count {
            let requested = sample_linear_indices[index] as usize;
            sample_indices[index] = if requested < voxel_count {
                sample_linear_indices[index]
            } else {
                0
            };
        }
        let params = [
            [
                vertex_count as u32,
                triangle_count as u32,
                voxel_count as u32,
                dimensions[0],
            ],
            [dimensions[1], dimensions[2], 0, 0],
        ];
        let grid_params = [grid.origin_voxel_size];

        let vertex_byte_len = std::mem::size_of_val(vertices) as vk::DeviceSize;
        let triangle_byte_len = std::mem::size_of_val(triangles) as vk::DeviceSize;
        let skinned_position_byte_len =
            (std::mem::size_of::<[f32; 4]>() * vertex_count) as vk::DeviceSize;
        let sdf_distance_byte_len = (std::mem::size_of::<f32>() * voxel_count) as vk::DeviceSize;
        let params_byte_len = std::mem::size_of_val(&params) as vk::DeviceSize;
        let grid_byte_len = std::mem::size_of_val(&grid_params) as vk::DeviceSize;

        let skin_shader_spv = compile_compute_wgsl_to_spirv(
            XR_GPU_F32_MESH_SDF_SKINNING_WGSL,
            XR_GPU_F32_MESH_SDF_SKINNING_ENTRY,
        )?;
        let sdf_shader_spv = compile_compute_wgsl_to_spirv(
            XR_GPU_F32_MESH_SDF_BUILD_WGSL,
            XR_GPU_F32_MESH_SDF_BUILD_ENTRY,
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
        let skinned_positions = match self.create_host_buffer(
            vk::BufferUsageFlags::STORAGE_BUFFER,
            skinned_position_byte_len,
        ) {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(err);
            }
        };
        let sdf_distances = match self
            .create_host_buffer(vk::BufferUsageFlags::STORAGE_BUFFER, sdf_distance_byte_len)
        {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_buffer(skinned_positions);
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
                self.destroy_buffer(sdf_distances);
                self.destroy_buffer(skinned_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(err);
            }
        };
        let grid_buffer = match self
            .create_host_buffer_with_data(vk::BufferUsageFlags::STORAGE_BUFFER, &grid_params)
        {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_buffer(params_buffer);
                self.destroy_buffer(sdf_distances);
                self.destroy_buffer(skinned_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(err);
            }
        };

        let skin_shader_module = match unsafe {
            self.device.create_shader_module(
                &vk::ShaderModuleCreateInfo::default().code(&skin_shader_spv),
                None,
            )
        } {
            Ok(shader_module) => shader_module,
            Err(err) => {
                self.destroy_buffer(grid_buffer);
                self.destroy_buffer(params_buffer);
                self.destroy_buffer(sdf_distances);
                self.destroy_buffer(skinned_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(format!(
                    "create_shader_module(f32 mesh SDF skinning probe) failed: {err:?}"
                ));
            }
        };
        let sdf_shader_module = match unsafe {
            self.device.create_shader_module(
                &vk::ShaderModuleCreateInfo::default().code(&sdf_shader_spv),
                None,
            )
        } {
            Ok(shader_module) => shader_module,
            Err(err) => {
                unsafe {
                    self.device.destroy_shader_module(skin_shader_module, None);
                }
                self.destroy_buffer(grid_buffer);
                self.destroy_buffer(params_buffer);
                self.destroy_buffer(sdf_distances);
                self.destroy_buffer(skinned_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(format!(
                    "create_shader_module(f32 mesh SDF build probe) failed: {err:?}"
                ));
            }
        };

        let descriptor_bindings = [
            descriptor_binding(0),
            descriptor_binding(1),
            descriptor_binding(2),
            descriptor_binding(3),
            descriptor_binding(4),
            descriptor_binding(5),
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
                    self.device.destroy_shader_module(sdf_shader_module, None);
                    self.device.destroy_shader_module(skin_shader_module, None);
                }
                self.destroy_buffer(grid_buffer);
                self.destroy_buffer(params_buffer);
                self.destroy_buffer(sdf_distances);
                self.destroy_buffer(skinned_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(format!(
                    "create_descriptor_set_layout(f32 mesh SDF probe) failed: {err:?}"
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
                    self.device.destroy_shader_module(sdf_shader_module, None);
                    self.device.destroy_shader_module(skin_shader_module, None);
                }
                self.destroy_buffer(grid_buffer);
                self.destroy_buffer(params_buffer);
                self.destroy_buffer(sdf_distances);
                self.destroy_buffer(skinned_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(format!(
                    "create_pipeline_layout(f32 mesh SDF probe) failed: {err:?}"
                ));
            }
        };

        let skin_pipeline = match create_compute_pipeline(
            self,
            skin_shader_module,
            pipeline_layout,
            XR_GPU_F32_MESH_SDF_SKINNING_ENTRY,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                unsafe {
                    self.device.destroy_pipeline_layout(pipeline_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    self.device.destroy_shader_module(sdf_shader_module, None);
                    self.device.destroy_shader_module(skin_shader_module, None);
                }
                self.destroy_buffer(grid_buffer);
                self.destroy_buffer(params_buffer);
                self.destroy_buffer(sdf_distances);
                self.destroy_buffer(skinned_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(err);
            }
        };
        let sdf_pipeline = match create_compute_pipeline(
            self,
            sdf_shader_module,
            pipeline_layout,
            XR_GPU_F32_MESH_SDF_BUILD_ENTRY,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                unsafe {
                    self.device.destroy_pipeline(skin_pipeline, None);
                    self.device.destroy_pipeline_layout(pipeline_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    self.device.destroy_shader_module(sdf_shader_module, None);
                    self.device.destroy_shader_module(skin_shader_module, None);
                }
                self.destroy_buffer(grid_buffer);
                self.destroy_buffer(params_buffer);
                self.destroy_buffer(sdf_distances);
                self.destroy_buffer(skinned_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(err);
            }
        };

        let descriptor_pool_sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::STORAGE_BUFFER,
            descriptor_count: 6,
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
                    self.device.destroy_pipeline(sdf_pipeline, None);
                    self.device.destroy_pipeline(skin_pipeline, None);
                    self.device.destroy_pipeline_layout(pipeline_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    self.device.destroy_shader_module(sdf_shader_module, None);
                    self.device.destroy_shader_module(skin_shader_module, None);
                }
                self.destroy_buffer(grid_buffer);
                self.destroy_buffer(params_buffer);
                self.destroy_buffer(sdf_distances);
                self.destroy_buffer(skinned_positions);
                self.destroy_buffer(triangle_buffer);
                self.destroy_buffer(vertex_buffer);
                return Err(format!(
                    "create_descriptor_pool(f32 mesh SDF probe) failed: {err:?}"
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
                        self.device.destroy_pipeline(sdf_pipeline, None);
                        self.device.destroy_pipeline(skin_pipeline, None);
                        self.device.destroy_pipeline_layout(pipeline_layout, None);
                        self.device
                            .destroy_descriptor_set_layout(descriptor_set_layout, None);
                        self.device.destroy_shader_module(sdf_shader_module, None);
                        self.device.destroy_shader_module(skin_shader_module, None);
                    }
                    self.destroy_buffer(grid_buffer);
                    self.destroy_buffer(params_buffer);
                    self.destroy_buffer(sdf_distances);
                    self.destroy_buffer(skinned_positions);
                    self.destroy_buffer(triangle_buffer);
                    self.destroy_buffer(vertex_buffer);
                    return Err(format!(
                        "allocate_descriptor_sets(f32 mesh SDF probe) failed: {err:?}"
                    ));
                }
            }
        };

        let vertex_info = descriptor_buffer_info(&vertex_buffer, vertex_byte_len);
        let triangle_info = descriptor_buffer_info(&triangle_buffer, triangle_byte_len);
        let skinned_info = descriptor_buffer_info(&skinned_positions, skinned_position_byte_len);
        let sdf_info = descriptor_buffer_info(&sdf_distances, sdf_distance_byte_len);
        let params_info = descriptor_buffer_info(&params_buffer, params_byte_len);
        let grid_info = descriptor_buffer_info(&grid_buffer, grid_byte_len);
        let vertex_infos = [vertex_info];
        let triangle_infos = [triangle_info];
        let skinned_infos = [skinned_info];
        let sdf_infos = [sdf_info];
        let params_infos = [params_info];
        let grid_infos = [grid_info];
        let descriptor_writes = [
            write_descriptor(descriptor_set, 0, &vertex_infos),
            write_descriptor(descriptor_set, 1, &triangle_infos),
            write_descriptor(descriptor_set, 2, &skinned_infos),
            write_descriptor(descriptor_set, 3, &sdf_infos),
            write_descriptor(descriptor_set, 4, &params_infos),
            write_descriptor(descriptor_set, 5, &grid_infos),
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
                        self.device.destroy_pipeline(sdf_pipeline, None);
                        self.device.destroy_pipeline(skin_pipeline, None);
                        self.device.destroy_pipeline_layout(pipeline_layout, None);
                        self.device
                            .destroy_descriptor_set_layout(descriptor_set_layout, None);
                        self.device.destroy_shader_module(sdf_shader_module, None);
                        self.device.destroy_shader_module(skin_shader_module, None);
                    }
                    self.destroy_buffer(grid_buffer);
                    self.destroy_buffer(params_buffer);
                    self.destroy_buffer(sdf_distances);
                    self.destroy_buffer(skinned_positions);
                    self.destroy_buffer(triangle_buffer);
                    self.destroy_buffer(vertex_buffer);
                    return Err(format!(
                        "allocate_command_buffers(f32 mesh SDF probe) failed: {err:?}"
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
                        self.device.destroy_pipeline(sdf_pipeline, None);
                        self.device.destroy_pipeline(skin_pipeline, None);
                        self.device.destroy_pipeline_layout(pipeline_layout, None);
                        self.device
                            .destroy_descriptor_set_layout(descriptor_set_layout, None);
                        self.device.destroy_shader_module(sdf_shader_module, None);
                        self.device.destroy_shader_module(skin_shader_module, None);
                    }
                    self.destroy_buffer(grid_buffer);
                    self.destroy_buffer(params_buffer);
                    self.destroy_buffer(sdf_distances);
                    self.destroy_buffer(skinned_positions);
                    self.destroy_buffer(triangle_buffer);
                    self.destroy_buffer(vertex_buffer);
                    return Err(format!("create_fence(f32 mesh SDF probe) failed: {err:?}"));
                }
            }
        };

        let mut queue_submit_serial = 0;
        let mut fence_serial = 0;
        let mut queue_wait_idle_performed = false;
        let command_result = (|| -> Result<(), String> {
            unsafe {
                self.device
                    .begin_command_buffer(
                        command_buffer,
                        &vk::CommandBufferBeginInfo::default()
                            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                    )
                    .map_err(|e| {
                        format!("begin_command_buffer(f32 mesh SDF probe) failed: {e:?}")
                    })?;

                let input_barriers = [
                    host_to_compute_barrier(&vertex_buffer, vertex_byte_len),
                    host_to_compute_barrier(&triangle_buffer, triangle_byte_len),
                    host_to_compute_barrier(&params_buffer, params_byte_len),
                    host_to_compute_barrier(&grid_buffer, grid_byte_len),
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
                    skin_pipeline,
                );
                self.device.cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::COMPUTE,
                    pipeline_layout,
                    0,
                    &[descriptor_set],
                    &[],
                );
                self.device.cmd_dispatch(
                    command_buffer,
                    (vertex_count as u32).div_ceil(64).max(1),
                    1,
                    1,
                );

                let skinned_barriers = [vk::BufferMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(skinned_positions.buffer)
                    .offset(0)
                    .size(skinned_position_byte_len)];
                self.device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &skinned_barriers,
                    &[],
                );

                self.device.cmd_bind_pipeline(
                    command_buffer,
                    vk::PipelineBindPoint::COMPUTE,
                    sdf_pipeline,
                );
                self.device.cmd_dispatch(
                    command_buffer,
                    (voxel_count as u32).div_ceil(64).max(1),
                    1,
                    1,
                );

                let output_barriers = [vk::BufferMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .dst_access_mask(vk::AccessFlags::HOST_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(sdf_distances.buffer)
                    .offset(0)
                    .size(sdf_distance_byte_len)];
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
                    .map_err(|e| format!("end_command_buffer(f32 mesh SDF probe) failed: {e:?}"))?;
                self.device
                    .queue_submit(
                        self.queue,
                        &[vk::SubmitInfo::default().command_buffers(&[command_buffer])],
                        fence,
                    )
                    .map_err(|e| format!("queue_submit(f32 mesh SDF probe) failed: {e:?}"))?;
                self.gpu_submit_serial = self.gpu_submit_serial.saturating_add(1);
                queue_submit_serial = self.gpu_submit_serial;
                self.device
                    .wait_for_fences(&[fence], true, u64::MAX)
                    .map_err(|e| format!("wait_for_fences(f32 mesh SDF probe) failed: {e:?}"))?;
                fence_serial = queue_submit_serial;
                self.device
                    .queue_wait_idle(self.queue)
                    .map_err(|e| format!("queue_wait_idle(f32 mesh SDF probe) failed: {e:?}"))?;
                queue_wait_idle_performed = true;
                self.gpu_completed_submit_serial =
                    self.gpu_completed_submit_serial.max(queue_submit_serial);
                self.collect_retired_texture_resources();
            }
            Ok(())
        })();

        let read_result = if command_result.is_ok() {
            unsafe {
                let mapped = self
                    .device
                    .map_memory(
                        sdf_distances.memory,
                        0,
                        sdf_distance_byte_len,
                        vk::MemoryMapFlags::empty(),
                    )
                    .map_err(|err| {
                        format!("map_memory(f32 mesh SDF distance readback) failed: {err:?}")
                    })?;
                let rows = std::slice::from_raw_parts(mapped as *const f32, voxel_count);
                let output_distances = rows.to_vec();
                self.device.unmap_memory(sdf_distances.memory);
                Ok(output_distances)
            }
        } else {
            Err(command_result
                .err()
                .unwrap_or_else(|| "unknown f32 mesh SDF probe command failure".to_string()))
        };

        let resource_generation = self.xr_f32_mesh_sdf_probe_resources.len() as u64 + 1;
        self.xr_f32_mesh_sdf_probe_resources
            .push(VulkanXrF32MeshSdfProbeResources {
                vertices: vertex_buffer,
                triangles: triangle_buffer,
                skinned_positions,
                sdf_distances,
                params: params_buffer,
                grid: grid_buffer,
                skin_shader_module,
                sdf_shader_module,
                descriptor_set_layout,
                pipeline_layout,
                skin_pipeline,
                sdf_pipeline,
                descriptor_pool,
                command_buffer,
                fence,
            });
        let retained_resource_count = self.xr_f32_mesh_sdf_probe_resources.len();
        let pending_retire_count = retained_resource_count;
        let retired_after_fence_count = 0;

        let dense_distances = read_result?;
        let mut output_samples = [0.0; XR_GPU_F32_MESH_SDF_PROBE_SAMPLES];
        let mut expected_samples = [0.0; XR_GPU_F32_MESH_SDF_PROBE_SAMPLES];
        let mut mismatched_samples = 0;
        let mut max_abs_error = 0.0_f32;
        for index in 0..sample_count {
            let sample_index = sample_indices[index] as usize;
            let output = dense_distances
                .get(sample_index)
                .copied()
                .unwrap_or(f32::NAN);
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

        Ok(XrGpuF32MeshSdfProbeResult {
            vertex_count,
            triangle_count,
            index_count: triangle_count * 3,
            voxel_count,
            sample_count,
            checked_sample_count: sample_count,
            sample_linear_indices: sample_indices,
            output_distances: output_samples,
            expected_distances: expected_samples,
            mismatched_samples,
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

    pub(super) fn destroy_xr_f32_mesh_sdf_probe_resources(&mut self) {
        let resources = std::mem::take(&mut self.xr_f32_mesh_sdf_probe_resources);
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
                if resource.sdf_pipeline != vk::Pipeline::null() {
                    self.device.destroy_pipeline(resource.sdf_pipeline, None);
                }
                if resource.skin_pipeline != vk::Pipeline::null() {
                    self.device.destroy_pipeline(resource.skin_pipeline, None);
                }
                if resource.pipeline_layout != vk::PipelineLayout::null() {
                    self.device
                        .destroy_pipeline_layout(resource.pipeline_layout, None);
                }
                if resource.descriptor_set_layout != vk::DescriptorSetLayout::null() {
                    self.device
                        .destroy_descriptor_set_layout(resource.descriptor_set_layout, None);
                }
                if resource.sdf_shader_module != vk::ShaderModule::null() {
                    self.device
                        .destroy_shader_module(resource.sdf_shader_module, None);
                }
                if resource.skin_shader_module != vk::ShaderModule::null() {
                    self.device
                        .destroy_shader_module(resource.skin_shader_module, None);
                }
            }
            self.destroy_buffer(resource.grid);
            self.destroy_buffer(resource.params);
            self.destroy_buffer(resource.sdf_distances);
            self.destroy_buffer(resource.skinned_positions);
            self.destroy_buffer(resource.triangles);
            self.destroy_buffer(resource.vertices);
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
) -> vk::BufferMemoryBarrier<'_> {
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
    vulkan: &CxVulkan,
    shader_module: vk::ShaderModule,
    pipeline_layout: vk::PipelineLayout,
    entry: &str,
) -> Result<vk::Pipeline, String> {
    let entry = std::ffi::CString::new(entry).unwrap();
    let stage = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(shader_module)
        .name(&entry);
    let compute_pipeline_info = vk::ComputePipelineCreateInfo::default()
        .stage(stage)
        .layout(pipeline_layout);
    match unsafe {
        vulkan.device.create_compute_pipelines(
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
                        vulkan.device.destroy_pipeline(pipeline, None);
                    }
                }
            }
            Err(format!(
                "create_compute_pipelines(f32 mesh SDF probe {entry:?}) failed: {err:?}"
            ))
        }
    }
}
