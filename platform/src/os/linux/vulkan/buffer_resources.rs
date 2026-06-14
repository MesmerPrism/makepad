use crate::{
    cx::Cx,
    geometry::{CxGeometry, GeometryId},
};
use ash::vk;

use super::CxVulkan;

#[derive(Clone, Copy)]
pub(super) struct VulkanBuffer {
    pub(super) buffer: vk::Buffer,
    pub(super) memory: vk::DeviceMemory,
    pub(super) size: vk::DeviceSize,
}

#[derive(Clone, Copy)]
pub(super) struct VulkanGeometryResource {
    pub(super) vertex_buffer: VulkanBuffer,
    pub(super) index_buffer: VulkanBuffer,
}

impl CxVulkan {
    fn geometry_id_is_live(cx: &Cx, geometry_id: GeometryId) -> bool {
        let slot_index = geometry_id.slot_index();
        cx.geometries
            .0
            .pool
            .get(slot_index)
            .map(|item| item.generation == geometry_id.generation())
            .unwrap_or(false)
    }

    pub(super) fn prune_stale_geometry_resources(&mut self, cx: &Cx) {
        let stale_keys = self
            .geometries
            .keys()
            .copied()
            .filter(|geometry_id| !Self::geometry_id_is_live(cx, *geometry_id))
            .collect::<Vec<_>>();
        for geometry_id in stale_keys {
            if let Some(resource) = self.geometries.remove(&geometry_id) {
                self.destroy_geometry_resource(resource);
            }
        }
    }

    pub(super) fn align_device_size(
        value: vk::DeviceSize,
        alignment: vk::DeviceSize,
    ) -> vk::DeviceSize {
        if alignment <= 1 {
            value
        } else {
            value.div_ceil(alignment) * alignment
        }
    }

    pub(super) fn create_host_buffer(
        &self,
        usage: vk::BufferUsageFlags,
        byte_len: vk::DeviceSize,
    ) -> Result<VulkanBuffer, String> {
        let buffer_info = vk::BufferCreateInfo::default()
            .size(byte_len.max(4))
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let buffer = unsafe { self.device.create_buffer(&buffer_info, None) }
            .map_err(|e| format!("create_buffer failed: {e:?}"))?;
        let mem_req = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let memory_type_index = match self.find_memory_type(
            mem_req.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ) {
            Ok(memory_type_index) => memory_type_index,
            Err(err) => {
                unsafe {
                    self.device.destroy_buffer(buffer, None);
                }
                return Err(err);
            }
        };
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_req.size)
            .memory_type_index(memory_type_index);
        let memory = match unsafe { self.device.allocate_memory(&alloc_info, None) } {
            Ok(memory) => memory,
            Err(e) => {
                unsafe {
                    self.device.destroy_buffer(buffer, None);
                }
                return Err(format!("allocate_memory failed: {e:?}"));
            }
        };
        unsafe {
            if let Err(e) = self.device.bind_buffer_memory(buffer, memory, 0) {
                self.device.free_memory(memory, None);
                self.device.destroy_buffer(buffer, None);
                return Err(format!("bind_buffer_memory failed: {e:?}"));
            }
        }

        Ok(VulkanBuffer {
            buffer,
            memory,
            size: byte_len.max(4),
        })
    }

    pub(super) fn destroy_buffer(&self, buffer: VulkanBuffer) {
        unsafe {
            if buffer.buffer != vk::Buffer::null() {
                self.device.destroy_buffer(buffer.buffer, None);
            }
            if buffer.memory != vk::DeviceMemory::null() {
                self.device.free_memory(buffer.memory, None);
            }
        }
    }

    fn destroy_geometry_resource(&self, resource: VulkanGeometryResource) {
        self.destroy_buffer(resource.vertex_buffer);
        self.destroy_buffer(resource.index_buffer);
    }

    pub(super) fn ensure_geometry_resource(
        &mut self,
        geometry_id: GeometryId,
        geometry: &mut CxGeometry,
    ) -> Result<(), String> {
        if geometry.vertices.is_empty() || geometry.indices.is_empty() {
            if let Some(old) = self.geometries.remove(&geometry_id) {
                self.destroy_geometry_resource(old);
            }
            geometry.dirty_vertices = false;
            geometry.dirty_indices = false;
            geometry.dirty = false;
            return Ok(());
        }

        let existing = self.geometries.remove(&geometry_id);
        let vertex_needs_upload = existing.is_none() || geometry.dirty_vertices;
        let index_needs_upload = existing.is_none() || geometry.dirty_indices;

        let new_vertex_buffer = if vertex_needs_upload {
            let buffer = self.create_host_buffer_with_data(
                vk::BufferUsageFlags::VERTEX_BUFFER,
                &geometry.vertices,
            )?;
            self.xr_geometry_upload_bytes_this_frame +=
                std::mem::size_of_val(geometry.vertices.as_slice()) as u64;
            Some(buffer)
        } else {
            None
        };

        let new_index_buffer = if index_needs_upload {
            match self
                .create_host_buffer_with_data(vk::BufferUsageFlags::INDEX_BUFFER, &geometry.indices)
            {
                Ok(buffer) => {
                    self.xr_geometry_upload_bytes_this_frame +=
                        std::mem::size_of_val(geometry.indices.as_slice()) as u64;
                    Some(buffer)
                }
                Err(err) => {
                    if let Some(buffer) = new_vertex_buffer {
                        self.destroy_buffer(buffer);
                    }
                    if let Some(existing) = existing {
                        self.geometries.insert(geometry_id, existing);
                    }
                    return Err(err);
                }
            }
        } else {
            None
        };

        let resource = match existing {
            Some(existing) => {
                if vertex_needs_upload {
                    self.destroy_buffer(existing.vertex_buffer);
                }
                if index_needs_upload {
                    self.destroy_buffer(existing.index_buffer);
                }
                VulkanGeometryResource {
                    vertex_buffer: new_vertex_buffer.unwrap_or(existing.vertex_buffer),
                    index_buffer: new_index_buffer.unwrap_or(existing.index_buffer),
                }
            }
            None => VulkanGeometryResource {
                vertex_buffer: new_vertex_buffer
                    .ok_or_else(|| "missing Vulkan vertex buffer upload".to_string())?,
                index_buffer: new_index_buffer
                    .ok_or_else(|| "missing Vulkan index buffer upload".to_string())?,
            },
        };

        self.geometries.insert(geometry_id, resource);
        geometry.dirty_vertices = false;
        geometry.dirty_indices = false;
        geometry.dirty = false;
        Ok(())
    }

    pub(super) fn create_host_buffer_with_data<T: Copy>(
        &self,
        usage: vk::BufferUsageFlags,
        data: &[T],
    ) -> Result<VulkanBuffer, String> {
        let byte_len = std::mem::size_of_val(data) as vk::DeviceSize;
        let buffer = self.create_host_buffer(usage, byte_len)?;

        if !data.is_empty() {
            unsafe {
                let mapped = self
                    .device
                    .map_memory(buffer.memory, 0, buffer.size, vk::MemoryMapFlags::empty())
                    .map_err(|e| format!("map_memory failed: {e:?}"))?;
                std::ptr::copy_nonoverlapping(
                    data.as_ptr() as *const u8,
                    mapped as *mut u8,
                    std::mem::size_of_val(data),
                );
                self.device.unmap_memory(buffer.memory);
            }
        }

        Ok(buffer)
    }

    pub(super) fn find_memory_type(
        &self,
        type_filter: u32,
        properties: vk::MemoryPropertyFlags,
    ) -> Result<u32, String> {
        let memory_props = unsafe {
            self.instance
                .get_physical_device_memory_properties(self.physical_device)
        };
        for i in 0..memory_props.memory_type_count {
            let bit = 1u32 << i;
            if (type_filter & bit) == 0 {
                continue;
            }
            let flags = memory_props.memory_types[i as usize].property_flags;
            if flags.contains(properties) {
                return Ok(i);
            }
        }
        Err(format!(
            "failed to find memory type matching {:?} for filter 0x{:X}",
            properties, type_filter
        ))
    }

    pub(super) fn destroy_geometry_resources(&mut self) {
        let resources: Vec<VulkanGeometryResource> = self
            .geometries
            .drain()
            .map(|(_, resource)| resource)
            .collect();
        for resource in resources {
            self.destroy_geometry_resource(resource);
        }
    }
}
