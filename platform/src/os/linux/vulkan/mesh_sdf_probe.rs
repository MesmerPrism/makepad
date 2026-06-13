use crate::{
    cx_api::{
        XrGpuF32MeshSdfProbeGrid, XrGpuF32MeshSdfProbeResult, XrGpuF32MeshSdfProbeTicket,
        XrGpuF32SkinningMeshVertex, XrGpuSkinningMeshTriangle, XR_GPU_F32_MESH_SDF_PROBE_SAMPLES,
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

pub(super) struct VulkanXrF32MeshSdfProbeProgram {
    generation: u64,
    skin_shader_module: vk::ShaderModule,
    sdf_shader_module: vk::ShaderModule,
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    skin_pipeline: vk::Pipeline,
    sdf_pipeline: vk::Pipeline,
}

#[derive(Clone, Copy)]
struct VulkanXrF32MeshSdfProbeProgramUse {
    generation: u64,
    program_reused: bool,
    shader_compiled_this_submit: bool,
    pipeline_created_this_submit: bool,
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    skin_pipeline: vk::Pipeline,
    sdf_pipeline: vk::Pipeline,
}

pub(super) struct VulkanXrF32MeshSdfProbeSourceMeshBuffers {
    generation: u64,
    vertex_byte_len: vk::DeviceSize,
    triangle_byte_len: vk::DeviceSize,
    vertices: VulkanBuffer,
    triangles: VulkanBuffer,
}

struct VulkanXrF32MeshSdfProbeSourceMeshBufferUse {
    generation: u64,
    resident: bool,
    reused: bool,
    vertex_byte_len: vk::DeviceSize,
    triangle_byte_len: vk::DeviceSize,
    vertices: VulkanBuffer,
    triangles: VulkanBuffer,
    owned_vertices: Option<VulkanBuffer>,
    owned_triangles: Option<VulkanBuffer>,
}

pub(super) struct VulkanXrF32MeshSdfProbeDerivedBuffers {
    generation: u64,
    skinned_position_byte_len: vk::DeviceSize,
    sdf_distance_byte_len: vk::DeviceSize,
    skinned_positions: VulkanBuffer,
    sdf_distances: VulkanBuffer,
}

struct VulkanXrF32MeshSdfProbeDerivedBufferUse {
    generation: u64,
    resident: bool,
    reused: bool,
    skinned_position_byte_len: vk::DeviceSize,
    sdf_distance_byte_len: vk::DeviceSize,
    skinned_positions: VulkanBuffer,
    sdf_distances: VulkanBuffer,
    owned_skinned_positions: Option<VulkanBuffer>,
    owned_sdf_distances: Option<VulkanBuffer>,
}

fn xr_f32_mesh_sdf_capacity_byte_len(required: vk::DeviceSize) -> vk::DeviceSize {
    if required == 0 {
        0
    } else {
        required.checked_next_power_of_two().unwrap_or(required)
    }
}

pub(super) struct VulkanXrF32MeshSdfProbeResources {
    request_id: u64,
    started: Instant,
    vertex_count: usize,
    triangle_count: usize,
    voxel_count: usize,
    sample_count: usize,
    sample_indices: [u32; XR_GPU_F32_MESH_SDF_PROBE_SAMPLES],
    expected_distances: [f32; XR_GPU_F32_MESH_SDF_PROBE_SAMPLES],
    tolerance: f32,
    queue_submit_serial: u64,
    resource_generation: u64,
    program_generation: u64,
    program_reused: bool,
    shader_compiled_this_submit: bool,
    pipeline_created_this_submit: bool,
    source_mesh_buffer_generation: u64,
    source_mesh_buffers_resident: bool,
    source_mesh_buffers_reused: bool,
    source_vertex_buffer_bytes: vk::DeviceSize,
    source_triangle_buffer_bytes: vk::DeviceSize,
    derived_buffer_generation: u64,
    derived_buffers_resident: bool,
    derived_buffers_reused: bool,
    skinned_position_buffer_bytes: vk::DeviceSize,
    sdf_distance_buffer_bytes: vk::DeviceSize,
    completed: bool,
    owned_vertices: Option<VulkanBuffer>,
    owned_triangles: Option<VulkanBuffer>,
    owned_skinned_positions: Option<VulkanBuffer>,
    owned_sdf_distances: Option<VulkanBuffer>,
    sdf_distances: VulkanBuffer,
    params: VulkanBuffer,
    grid: VulkanBuffer,
    descriptor_pool: vk::DescriptorPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
}

impl CxVulkan {
    fn ensure_xr_f32_mesh_sdf_probe_program(
        &mut self,
    ) -> Result<VulkanXrF32MeshSdfProbeProgramUse, String> {
        if let Some(program) = self.xr_f32_mesh_sdf_probe_program.as_ref() {
            return Ok(VulkanXrF32MeshSdfProbeProgramUse {
                generation: program.generation,
                program_reused: true,
                shader_compiled_this_submit: false,
                pipeline_created_this_submit: false,
                descriptor_set_layout: program.descriptor_set_layout,
                pipeline_layout: program.pipeline_layout,
                skin_pipeline: program.skin_pipeline,
                sdf_pipeline: program.sdf_pipeline,
            });
        }

        let skin_shader_spv = compile_compute_wgsl_to_spirv(
            XR_GPU_F32_MESH_SDF_SKINNING_WGSL,
            XR_GPU_F32_MESH_SDF_SKINNING_ENTRY,
        )?;
        let sdf_shader_spv = compile_compute_wgsl_to_spirv(
            XR_GPU_F32_MESH_SDF_BUILD_WGSL,
            XR_GPU_F32_MESH_SDF_BUILD_ENTRY,
        )?;

        let skin_shader_module = unsafe {
            self.device
                .create_shader_module(
                    &vk::ShaderModuleCreateInfo::default().code(&skin_shader_spv),
                    None,
                )
                .map_err(|err| {
                    format!("create_shader_module(f32 mesh SDF skinning program) failed: {err:?}")
                })?
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
                return Err(format!(
                    "create_shader_module(f32 mesh SDF build program) failed: {err:?}"
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
                return Err(format!(
                    "create_descriptor_set_layout(f32 mesh SDF program) failed: {err:?}"
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
                return Err(format!(
                    "create_pipeline_layout(f32 mesh SDF program) failed: {err:?}"
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
                return Err(err);
            }
        };

        let generation = 1;
        self.xr_f32_mesh_sdf_probe_program = Some(VulkanXrF32MeshSdfProbeProgram {
            generation,
            skin_shader_module,
            sdf_shader_module,
            descriptor_set_layout,
            pipeline_layout,
            skin_pipeline,
            sdf_pipeline,
        });

        Ok(VulkanXrF32MeshSdfProbeProgramUse {
            generation,
            program_reused: false,
            shader_compiled_this_submit: true,
            pipeline_created_this_submit: true,
            descriptor_set_layout,
            pipeline_layout,
            skin_pipeline,
            sdf_pipeline,
        })
    }

    fn prepare_xr_f32_mesh_sdf_source_mesh_buffers(
        &mut self,
        vertices: &[XrGpuF32SkinningMeshVertex],
        triangles: &[XrGpuSkinningMeshTriangle],
        vertex_byte_len: vk::DeviceSize,
        triangle_byte_len: vk::DeviceSize,
    ) -> Result<VulkanXrF32MeshSdfProbeSourceMeshBufferUse, String> {
        let has_pending_reader = self
            .xr_f32_mesh_sdf_probe_resources
            .iter()
            .any(|resource| !resource.completed);
        if has_pending_reader {
            return self.create_owned_xr_f32_mesh_sdf_source_mesh_buffers(
                vertices,
                triangles,
                vertex_byte_len,
                triangle_byte_len,
            );
        }

        let mut generation = self
            .xr_f32_mesh_sdf_probe_source_mesh_buffers
            .as_ref()
            .map_or(1, |buffers| buffers.generation);
        let reused = self
            .xr_f32_mesh_sdf_probe_source_mesh_buffers
            .as_ref()
            .is_some_and(|buffers| {
                buffers.vertex_byte_len == vertex_byte_len
                    && buffers.triangle_byte_len == triangle_byte_len
            });
        if !reused {
            if let Some(old_buffers) = self.xr_f32_mesh_sdf_probe_source_mesh_buffers.take() {
                generation = old_buffers.generation.saturating_add(1);
                self.destroy_buffer(old_buffers.triangles);
                self.destroy_buffer(old_buffers.vertices);
            }
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
            self.xr_f32_mesh_sdf_probe_source_mesh_buffers =
                Some(VulkanXrF32MeshSdfProbeSourceMeshBuffers {
                    generation,
                    vertex_byte_len,
                    triangle_byte_len,
                    vertices: vertex_buffer,
                    triangles: triangle_buffer,
                });
        }

        let buffers = self
            .xr_f32_mesh_sdf_probe_source_mesh_buffers
            .as_ref()
            .ok_or_else(|| "f32 mesh SDF resident source mesh buffers missing".to_string())?;
        let vertices_buffer = buffers.vertices;
        let triangles_buffer = buffers.triangles;
        let generation = buffers.generation;
        self.write_xr_f32_mesh_sdf_host_buffer(
            vertices_buffer,
            vertices,
            vertex_byte_len,
            "source vertices",
        )?;
        self.write_xr_f32_mesh_sdf_host_buffer(
            triangles_buffer,
            triangles,
            triangle_byte_len,
            "source triangles",
        )?;

        Ok(VulkanXrF32MeshSdfProbeSourceMeshBufferUse {
            generation,
            resident: true,
            reused,
            vertex_byte_len,
            triangle_byte_len,
            vertices: vertices_buffer,
            triangles: triangles_buffer,
            owned_vertices: None,
            owned_triangles: None,
        })
    }

    fn create_owned_xr_f32_mesh_sdf_source_mesh_buffers(
        &mut self,
        vertices: &[XrGpuF32SkinningMeshVertex],
        triangles: &[XrGpuSkinningMeshTriangle],
        vertex_byte_len: vk::DeviceSize,
        triangle_byte_len: vk::DeviceSize,
    ) -> Result<VulkanXrF32MeshSdfProbeSourceMeshBufferUse, String> {
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
        Ok(VulkanXrF32MeshSdfProbeSourceMeshBufferUse {
            generation: 0,
            resident: false,
            reused: false,
            vertex_byte_len,
            triangle_byte_len,
            vertices: vertex_buffer,
            triangles: triangle_buffer,
            owned_vertices: Some(vertex_buffer),
            owned_triangles: Some(triangle_buffer),
        })
    }

    fn prepare_xr_f32_mesh_sdf_derived_buffers(
        &mut self,
        skinned_position_byte_len: vk::DeviceSize,
        sdf_distance_byte_len: vk::DeviceSize,
    ) -> Result<VulkanXrF32MeshSdfProbeDerivedBufferUse, String> {
        let has_pending_reader = self
            .xr_f32_mesh_sdf_probe_resources
            .iter()
            .any(|resource| !resource.completed);
        if has_pending_reader {
            return self.create_owned_xr_f32_mesh_sdf_derived_buffers(
                skinned_position_byte_len,
                sdf_distance_byte_len,
            );
        }

        let mut generation = self
            .xr_f32_mesh_sdf_probe_derived_buffers
            .as_ref()
            .map_or(1, |buffers| buffers.generation);
        let skinned_position_capacity_byte_len =
            xr_f32_mesh_sdf_capacity_byte_len(skinned_position_byte_len);
        let sdf_distance_capacity_byte_len =
            xr_f32_mesh_sdf_capacity_byte_len(sdf_distance_byte_len);
        let reused = self
            .xr_f32_mesh_sdf_probe_derived_buffers
            .as_ref()
            .is_some_and(|buffers| {
                buffers.skinned_position_byte_len >= skinned_position_byte_len
                    && buffers.sdf_distance_byte_len >= sdf_distance_byte_len
            });
        if !reused {
            if let Some(old_buffers) = self.xr_f32_mesh_sdf_probe_derived_buffers.take() {
                generation = old_buffers.generation.saturating_add(1);
                self.destroy_buffer(old_buffers.sdf_distances);
                self.destroy_buffer(old_buffers.skinned_positions);
            }
            let skinned_positions = self.create_host_buffer(
                vk::BufferUsageFlags::STORAGE_BUFFER,
                skinned_position_capacity_byte_len,
            )?;
            let sdf_distances = match self.create_host_buffer(
                vk::BufferUsageFlags::STORAGE_BUFFER,
                sdf_distance_capacity_byte_len,
            ) {
                Ok(buffer) => buffer,
                Err(err) => {
                    self.destroy_buffer(skinned_positions);
                    return Err(err);
                }
            };
            self.xr_f32_mesh_sdf_probe_derived_buffers =
                Some(VulkanXrF32MeshSdfProbeDerivedBuffers {
                    generation,
                    skinned_position_byte_len: skinned_position_capacity_byte_len,
                    sdf_distance_byte_len: sdf_distance_capacity_byte_len,
                    skinned_positions,
                    sdf_distances,
                });
        }

        let buffers = self
            .xr_f32_mesh_sdf_probe_derived_buffers
            .as_ref()
            .ok_or_else(|| "f32 mesh SDF resident derived buffers missing".to_string())?;
        Ok(VulkanXrF32MeshSdfProbeDerivedBufferUse {
            generation: buffers.generation,
            resident: true,
            reused,
            skinned_position_byte_len: buffers.skinned_position_byte_len,
            sdf_distance_byte_len: buffers.sdf_distance_byte_len,
            skinned_positions: buffers.skinned_positions,
            sdf_distances: buffers.sdf_distances,
            owned_skinned_positions: None,
            owned_sdf_distances: None,
        })
    }

    fn create_owned_xr_f32_mesh_sdf_derived_buffers(
        &mut self,
        skinned_position_byte_len: vk::DeviceSize,
        sdf_distance_byte_len: vk::DeviceSize,
    ) -> Result<VulkanXrF32MeshSdfProbeDerivedBufferUse, String> {
        let skinned_positions = self.create_host_buffer(
            vk::BufferUsageFlags::STORAGE_BUFFER,
            skinned_position_byte_len,
        )?;
        let sdf_distances = match self
            .create_host_buffer(vk::BufferUsageFlags::STORAGE_BUFFER, sdf_distance_byte_len)
        {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_buffer(skinned_positions);
                return Err(err);
            }
        };
        Ok(VulkanXrF32MeshSdfProbeDerivedBufferUse {
            generation: 0,
            resident: false,
            reused: false,
            skinned_position_byte_len,
            sdf_distance_byte_len,
            skinned_positions,
            sdf_distances,
            owned_skinned_positions: Some(skinned_positions),
            owned_sdf_distances: Some(sdf_distances),
        })
    }

    fn write_xr_f32_mesh_sdf_host_buffer<T: Copy>(
        &self,
        buffer: VulkanBuffer,
        data: &[T],
        byte_len: vk::DeviceSize,
        label: &str,
    ) -> Result<(), String> {
        if byte_len > buffer.size {
            return Err(format!(
                "f32 mesh SDF {label} data exceeds resident buffer size"
            ));
        }
        if data.is_empty() {
            return Ok(());
        }
        unsafe {
            let mapped = self
                .device
                .map_memory(buffer.memory, 0, buffer.size, vk::MemoryMapFlags::empty())
                .map_err(|err| format!("map_memory(f32 mesh SDF {label}) failed: {err:?}"))?;
            std::ptr::copy_nonoverlapping(
                data.as_ptr() as *const u8,
                mapped as *mut u8,
                byte_len as usize,
            );
            self.device.unmap_memory(buffer.memory);
        }
        Ok(())
    }

    fn destroy_xr_f32_mesh_sdf_owned_source_mesh_buffers(
        &self,
        buffers: VulkanXrF32MeshSdfProbeSourceMeshBufferUse,
    ) {
        if let Some(buffer) = buffers.owned_triangles {
            self.destroy_buffer(buffer);
        }
        if let Some(buffer) = buffers.owned_vertices {
            self.destroy_buffer(buffer);
        }
    }

    fn destroy_xr_f32_mesh_sdf_owned_derived_buffers(
        &self,
        buffers: VulkanXrF32MeshSdfProbeDerivedBufferUse,
    ) {
        if let Some(buffer) = buffers.owned_sdf_distances {
            self.destroy_buffer(buffer);
        }
        if let Some(buffer) = buffers.owned_skinned_positions {
            self.destroy_buffer(buffer);
        }
    }

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
        let ticket = self.submit_xr_f32_mesh_sdf_probe_async(
            vertices,
            triangles,
            grid,
            sample_linear_indices,
            expected_distances,
            sample_count,
            tolerance,
        )?;
        self.wait_xr_f32_mesh_sdf_probe(ticket.request_id)
    }

    pub(crate) fn submit_xr_f32_mesh_sdf_probe_async(
        &mut self,
        vertices: &[XrGpuF32SkinningMeshVertex],
        triangles: &[XrGpuSkinningMeshTriangle],
        grid: XrGpuF32MeshSdfProbeGrid,
        sample_linear_indices: [u32; XR_GPU_F32_MESH_SDF_PROBE_SAMPLES],
        expected_distances: [f32; XR_GPU_F32_MESH_SDF_PROBE_SAMPLES],
        sample_count: usize,
        tolerance: f32,
    ) -> Result<XrGpuF32MeshSdfProbeTicket, String> {
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

        let program = self.ensure_xr_f32_mesh_sdf_probe_program()?;
        let descriptor_set_layout = program.descriptor_set_layout;
        let pipeline_layout = program.pipeline_layout;
        let skin_pipeline = program.skin_pipeline;
        let sdf_pipeline = program.sdf_pipeline;

        let source_mesh_buffers = self.prepare_xr_f32_mesh_sdf_source_mesh_buffers(
            vertices,
            triangles,
            vertex_byte_len,
            triangle_byte_len,
        )?;
        let vertex_buffer = source_mesh_buffers.vertices;
        let triangle_buffer = source_mesh_buffers.triangles;
        let derived_buffers = match self.prepare_xr_f32_mesh_sdf_derived_buffers(
            skinned_position_byte_len,
            sdf_distance_byte_len,
        ) {
            Ok(buffers) => buffers,
            Err(err) => {
                self.destroy_xr_f32_mesh_sdf_owned_source_mesh_buffers(source_mesh_buffers);
                return Err(err);
            }
        };
        let skinned_positions = derived_buffers.skinned_positions;
        let sdf_distances = derived_buffers.sdf_distances;
        let params_buffer = match self
            .create_host_buffer_with_data(vk::BufferUsageFlags::STORAGE_BUFFER, &params)
        {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_xr_f32_mesh_sdf_owned_derived_buffers(derived_buffers);
                self.destroy_xr_f32_mesh_sdf_owned_source_mesh_buffers(source_mesh_buffers);
                return Err(err);
            }
        };
        let grid_buffer = match self
            .create_host_buffer_with_data(vk::BufferUsageFlags::STORAGE_BUFFER, &grid_params)
        {
            Ok(buffer) => buffer,
            Err(err) => {
                self.destroy_buffer(params_buffer);
                self.destroy_xr_f32_mesh_sdf_owned_derived_buffers(derived_buffers);
                self.destroy_xr_f32_mesh_sdf_owned_source_mesh_buffers(source_mesh_buffers);
                return Err(err);
            }
        };

        let set_layouts = [descriptor_set_layout];

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
                self.destroy_buffer(grid_buffer);
                self.destroy_buffer(params_buffer);
                self.destroy_xr_f32_mesh_sdf_owned_derived_buffers(derived_buffers);
                self.destroy_xr_f32_mesh_sdf_owned_source_mesh_buffers(source_mesh_buffers);
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
                    }
                    self.destroy_buffer(grid_buffer);
                    self.destroy_buffer(params_buffer);
                    self.destroy_xr_f32_mesh_sdf_owned_derived_buffers(derived_buffers);
                    self.destroy_xr_f32_mesh_sdf_owned_source_mesh_buffers(source_mesh_buffers);
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
                    }
                    self.destroy_buffer(grid_buffer);
                    self.destroy_buffer(params_buffer);
                    self.destroy_xr_f32_mesh_sdf_owned_derived_buffers(derived_buffers);
                    self.destroy_xr_f32_mesh_sdf_owned_source_mesh_buffers(source_mesh_buffers);
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
                    }
                    self.destroy_buffer(grid_buffer);
                    self.destroy_buffer(params_buffer);
                    self.destroy_xr_f32_mesh_sdf_owned_derived_buffers(derived_buffers);
                    self.destroy_xr_f32_mesh_sdf_owned_source_mesh_buffers(source_mesh_buffers);
                    return Err(format!("create_fence(f32 mesh SDF probe) failed: {err:?}"));
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
            }
            self.destroy_buffer(grid_buffer);
            self.destroy_buffer(params_buffer);
            self.destroy_xr_f32_mesh_sdf_owned_derived_buffers(derived_buffers);
            self.destroy_xr_f32_mesh_sdf_owned_source_mesh_buffers(source_mesh_buffers);
            return Err(err);
        }

        let resource_generation = self.xr_f32_mesh_sdf_probe_resources.len() as u64 + 1;
        let request_id = queue_submit_serial;
        self.xr_f32_mesh_sdf_probe_resources
            .push(VulkanXrF32MeshSdfProbeResources {
                request_id,
                started,
                vertex_count,
                triangle_count,
                voxel_count,
                sample_count,
                sample_indices,
                expected_distances,
                tolerance,
                queue_submit_serial,
                resource_generation,
                program_generation: program.generation,
                program_reused: program.program_reused,
                shader_compiled_this_submit: program.shader_compiled_this_submit,
                pipeline_created_this_submit: program.pipeline_created_this_submit,
                source_mesh_buffer_generation: source_mesh_buffers.generation,
                source_mesh_buffers_resident: source_mesh_buffers.resident,
                source_mesh_buffers_reused: source_mesh_buffers.reused,
                source_vertex_buffer_bytes: source_mesh_buffers.vertex_byte_len,
                source_triangle_buffer_bytes: source_mesh_buffers.triangle_byte_len,
                derived_buffer_generation: derived_buffers.generation,
                derived_buffers_resident: derived_buffers.resident,
                derived_buffers_reused: derived_buffers.reused,
                skinned_position_buffer_bytes: derived_buffers.skinned_position_byte_len,
                sdf_distance_buffer_bytes: derived_buffers.sdf_distance_byte_len,
                completed: false,
                owned_vertices: source_mesh_buffers.owned_vertices,
                owned_triangles: source_mesh_buffers.owned_triangles,
                owned_skinned_positions: derived_buffers.owned_skinned_positions,
                owned_sdf_distances: derived_buffers.owned_sdf_distances,
                sdf_distances,
                params: params_buffer,
                grid: grid_buffer,
                descriptor_pool,
                command_buffer,
                fence,
            });
        let retained_resource_count = self.xr_f32_mesh_sdf_probe_resources.len();
        let pending_retire_count = retained_resource_count;

        Ok(XrGpuF32MeshSdfProbeTicket {
            request_id,
            queue_submit_serial,
            resource_generation,
            pending_retire_count,
            retained_resource_count,
        })
    }

    pub(crate) fn poll_xr_f32_mesh_sdf_probe(
        &mut self,
        request_id: u64,
    ) -> Result<Option<XrGpuF32MeshSdfProbeResult>, String> {
        let Some(resource_index) = self
            .xr_f32_mesh_sdf_probe_resources
            .iter()
            .position(|resource| resource.request_id == request_id)
        else {
            return Ok(None);
        };
        if self.xr_f32_mesh_sdf_probe_resources[resource_index].completed {
            return Ok(None);
        }

        let fence = self.xr_f32_mesh_sdf_probe_resources[resource_index].fence;
        let queue_submit_serial =
            self.xr_f32_mesh_sdf_probe_resources[resource_index].queue_submit_serial;
        match unsafe { self.device.get_fence_status(fence) } {
            Ok(true) => {
                self.gpu_completed_submit_serial =
                    self.gpu_completed_submit_serial.max(queue_submit_serial);
                self.collect_retired_texture_resources();
                self.complete_xr_f32_mesh_sdf_probe(resource_index, false)
                    .map(Some)
            }
            Ok(false) => Ok(None),
            Err(err) => Err(format!(
                "get_fence_status(f32 mesh SDF probe {request_id}) failed: {err:?}"
            )),
        }
    }

    fn wait_xr_f32_mesh_sdf_probe(
        &mut self,
        request_id: u64,
    ) -> Result<XrGpuF32MeshSdfProbeResult, String> {
        let resource_index = self
            .xr_f32_mesh_sdf_probe_resources
            .iter()
            .position(|resource| resource.request_id == request_id)
            .ok_or_else(|| format!("f32 mesh SDF probe {request_id} was not found"))?;
        if self.xr_f32_mesh_sdf_probe_resources[resource_index].completed {
            return Err(format!(
                "f32 mesh SDF probe {request_id} was already completed"
            ));
        }

        let fence = self.xr_f32_mesh_sdf_probe_resources[resource_index].fence;
        let queue_submit_serial =
            self.xr_f32_mesh_sdf_probe_resources[resource_index].queue_submit_serial;
        unsafe {
            self.device
                .wait_for_fences(&[fence], true, u64::MAX)
                .map_err(|e| format!("wait_for_fences(f32 mesh SDF probe) failed: {e:?}"))?;
            self.device
                .queue_wait_idle(self.queue)
                .map_err(|e| format!("queue_wait_idle(f32 mesh SDF probe) failed: {e:?}"))?;
        }
        self.gpu_completed_submit_serial =
            self.gpu_completed_submit_serial.max(queue_submit_serial);
        self.collect_retired_texture_resources();
        self.complete_xr_f32_mesh_sdf_probe(resource_index, true)
    }

    fn complete_xr_f32_mesh_sdf_probe(
        &mut self,
        resource_index: usize,
        queue_wait_idle_performed: bool,
    ) -> Result<XrGpuF32MeshSdfProbeResult, String> {
        let (
            started,
            vertex_count,
            triangle_count,
            voxel_count,
            sample_count,
            sample_indices,
            expected_distances,
            tolerance,
            queue_submit_serial,
            resource_generation,
            program_generation,
            program_reused,
            shader_compiled_this_submit,
            pipeline_created_this_submit,
            source_mesh_buffer_generation,
            source_mesh_buffers_resident,
            source_mesh_buffers_reused,
            source_vertex_buffer_bytes,
            source_triangle_buffer_bytes,
            derived_buffer_generation,
            derived_buffers_resident,
            derived_buffers_reused,
            skinned_position_buffer_bytes,
            sdf_distance_buffer_bytes,
            sdf_distances,
        ) = {
            let resource = self
                .xr_f32_mesh_sdf_probe_resources
                .get(resource_index)
                .ok_or_else(|| "f32 mesh SDF probe resource index is stale".to_string())?;
            if resource.completed {
                return Err(format!(
                    "f32 mesh SDF probe {} was already completed",
                    resource.request_id
                ));
            }
            (
                resource.started,
                resource.vertex_count,
                resource.triangle_count,
                resource.voxel_count,
                resource.sample_count,
                resource.sample_indices,
                resource.expected_distances,
                resource.tolerance,
                resource.queue_submit_serial,
                resource.resource_generation,
                resource.program_generation,
                resource.program_reused,
                resource.shader_compiled_this_submit,
                resource.pipeline_created_this_submit,
                resource.source_mesh_buffer_generation,
                resource.source_mesh_buffers_resident,
                resource.source_mesh_buffers_reused,
                resource.source_vertex_buffer_bytes,
                resource.source_triangle_buffer_bytes,
                resource.derived_buffer_generation,
                resource.derived_buffers_resident,
                resource.derived_buffers_reused,
                resource.skinned_position_buffer_bytes,
                resource.sdf_distance_buffer_bytes,
                resource.sdf_distances,
            )
        };
        let sdf_distance_byte_len = (std::mem::size_of::<f32>() * voxel_count) as vk::DeviceSize;
        let dense_distances = unsafe {
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
            output_distances
        };

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

        if let Some(resource) = self.xr_f32_mesh_sdf_probe_resources.get_mut(resource_index) {
            resource.completed = true;
        }
        let retained_resource_count = self.xr_f32_mesh_sdf_probe_resources.len();
        let pending_retire_count = self
            .xr_f32_mesh_sdf_probe_resources
            .iter()
            .filter(|resource| !resource.completed)
            .count();

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
            fence_serial: queue_submit_serial,
            resource_generation,
            program_generation,
            program_reused,
            shader_compiled_this_submit,
            pipeline_created_this_submit,
            source_mesh_buffer_generation,
            source_mesh_buffers_resident,
            source_mesh_buffers_reused,
            source_vertex_buffer_bytes: source_vertex_buffer_bytes as u64,
            source_triangle_buffer_bytes: source_triangle_buffer_bytes as u64,
            derived_buffer_generation,
            derived_buffers_resident,
            derived_buffers_reused,
            skinned_position_buffer_bytes: skinned_position_buffer_bytes as u64,
            sdf_distance_buffer_bytes: sdf_distance_buffer_bytes as u64,
            pending_retire_count,
            retained_resource_count,
            retired_after_fence_count: 0,
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
            }
            self.destroy_buffer(resource.grid);
            self.destroy_buffer(resource.params);
            if let Some(buffer) = resource.owned_sdf_distances {
                self.destroy_buffer(buffer);
            }
            if let Some(buffer) = resource.owned_skinned_positions {
                self.destroy_buffer(buffer);
            }
            if let Some(buffer) = resource.owned_triangles {
                self.destroy_buffer(buffer);
            }
            if let Some(buffer) = resource.owned_vertices {
                self.destroy_buffer(buffer);
            }
        }
    }

    pub(super) fn destroy_xr_f32_mesh_sdf_probe_derived_buffers(&mut self) {
        if let Some(buffers) = self.xr_f32_mesh_sdf_probe_derived_buffers.take() {
            self.destroy_buffer(buffers.sdf_distances);
            self.destroy_buffer(buffers.skinned_positions);
        }
    }

    pub(super) fn destroy_xr_f32_mesh_sdf_probe_source_mesh_buffers(&mut self) {
        if let Some(buffers) = self.xr_f32_mesh_sdf_probe_source_mesh_buffers.take() {
            self.destroy_buffer(buffers.triangles);
            self.destroy_buffer(buffers.vertices);
        }
    }

    pub(super) fn destroy_xr_f32_mesh_sdf_probe_program(&mut self) {
        if let Some(program) = self.xr_f32_mesh_sdf_probe_program.take() {
            unsafe {
                if program.sdf_pipeline != vk::Pipeline::null() {
                    self.device.destroy_pipeline(program.sdf_pipeline, None);
                }
                if program.skin_pipeline != vk::Pipeline::null() {
                    self.device.destroy_pipeline(program.skin_pipeline, None);
                }
                if program.pipeline_layout != vk::PipelineLayout::null() {
                    self.device
                        .destroy_pipeline_layout(program.pipeline_layout, None);
                }
                if program.descriptor_set_layout != vk::DescriptorSetLayout::null() {
                    self.device
                        .destroy_descriptor_set_layout(program.descriptor_set_layout, None);
                }
                if program.sdf_shader_module != vk::ShaderModule::null() {
                    self.device
                        .destroy_shader_module(program.sdf_shader_module, None);
                }
                if program.skin_shader_module != vk::ShaderModule::null() {
                    self.device
                        .destroy_shader_module(program.skin_shader_module, None);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_sdf_derived_capacity_rounds_small_growth_to_same_buffer_size() {
        assert_eq!(xr_f32_mesh_sdf_capacity_byte_len(5_808), 8_192);
        assert_eq!(xr_f32_mesh_sdf_capacity_byte_len(6_292), 8_192);
    }

    #[test]
    fn mesh_sdf_derived_capacity_handles_exact_powers_and_overflow() {
        assert_eq!(xr_f32_mesh_sdf_capacity_byte_len(8_192), 8_192);
        assert_eq!(xr_f32_mesh_sdf_capacity_byte_len(8_193), 16_384);
        assert_eq!(xr_f32_mesh_sdf_capacity_byte_len(u64::MAX), u64::MAX);
    }
}
