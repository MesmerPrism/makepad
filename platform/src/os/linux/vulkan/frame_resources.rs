use ash::vk;

use super::{CxVulkan, FrameResources, VulkanBuffer};

impl CxVulkan {
    pub(super) fn destroy_owned_frame_resources(
        device: &ash::Device,
        frame_resources: &mut FrameResources,
    ) {
        unsafe {
            for pool in frame_resources.descriptor_pools.drain(..) {
                device.destroy_descriptor_pool(pool, None);
            }
            for buffer in frame_resources.buffers.drain(..) {
                device.destroy_buffer(buffer.buffer, None);
                device.free_memory(buffer.memory, None);
            }
            if let Some(buffer) = frame_resources.packet_buffer.take() {
                device.destroy_buffer(buffer.buffer, None);
                device.free_memory(buffer.memory, None);
            }
            if let Some(buffer) = frame_resources.texture_upload_buffer.take() {
                device.destroy_buffer(buffer.buffer, None);
                device.free_memory(buffer.memory, None);
            }
        }
        frame_resources.packet_buffer_used = 0;
        frame_resources.texture_upload_buffer_used = 0;
    }

    pub(super) fn recycle_owned_frame_resources(
        &self,
        frame_resources: &mut FrameResources,
    ) -> Result<(), String> {
        unsafe {
            for &pool in &frame_resources.descriptor_pools {
                self.device
                    .reset_descriptor_pool(pool, vk::DescriptorPoolResetFlags::empty())
                    .map_err(|e| format!("reset_descriptor_pool(openxr inflight) failed: {e:?}"))?;
            }
            for buffer in frame_resources.buffers.drain(..) {
                self.device.destroy_buffer(buffer.buffer, None);
                self.device.free_memory(buffer.memory, None);
            }
        }
        frame_resources.packet_buffer_used = 0;
        frame_resources.texture_upload_buffer_used = 0;
        Ok(())
    }

    fn create_frame_descriptor_pool(&self) -> Result<vk::DescriptorPool, String> {
        let pool_sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: 8192,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLED_IMAGE,
                descriptor_count: 4096,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: 1024,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLER,
                descriptor_count: 4096,
            },
        ];
        let info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(2048)
            .pool_sizes(&pool_sizes);
        unsafe { self.device.create_descriptor_pool(&info, None) }
            .map_err(|e| format!("create_descriptor_pool failed: {e:?}"))
    }

    pub(super) fn alloc_frame_descriptor_set(
        &mut self,
        descriptor_set_layout: vk::DescriptorSetLayout,
    ) -> Result<vk::DescriptorSet, String> {
        if self.frame_resources.descriptor_pools.is_empty() {
            let pool = self.create_frame_descriptor_pool()?;
            self.frame_resources.descriptor_pools.push(pool);
        }
        let try_alloc = |device: &ash::Device, pool: vk::DescriptorPool| {
            let set_layouts = [descriptor_set_layout];
            let alloc_info = vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pool)
                .set_layouts(&set_layouts);
            unsafe { device.allocate_descriptor_sets(&alloc_info) }.map(|sets| sets[0])
        };

        let pool = *self.frame_resources.descriptor_pools.last().unwrap();
        match try_alloc(&self.device, pool) {
            Ok(set) => {
                self.xr_descriptor_set_count_this_frame += 1;
                Ok(set)
            }
            Err(vk::Result::ERROR_OUT_OF_POOL_MEMORY) | Err(vk::Result::ERROR_FRAGMENTED_POOL) => {
                let pool = self.create_frame_descriptor_pool()?;
                self.frame_resources.descriptor_pools.push(pool);
                let set = try_alloc(&self.device, pool)
                    .map_err(|e| format!("allocate_descriptor_sets failed: {e:?}"))?;
                self.xr_descriptor_set_count_this_frame += 1;
                Ok(set)
            }
            Err(e) => Err(format!("allocate_descriptor_sets failed: {e:?}")),
        }
    }

    pub(super) fn alloc_frame_packet_slice(
        &mut self,
        usage: vk::BufferUsageFlags,
        size: vk::DeviceSize,
        alignment: vk::DeviceSize,
    ) -> Result<(VulkanBuffer, vk::DeviceSize), String> {
        let size = size.max(4);
        let alignment = alignment.max(4);
        let mut offset =
            Self::align_device_size(self.frame_resources.packet_buffer_used, alignment);
        let required_size = offset + size;
        let needs_grow = self
            .frame_resources
            .packet_buffer
            .map(|buffer| buffer.size < required_size)
            .unwrap_or(true);
        if needs_grow {
            if let Some(old_buffer) = self.frame_resources.packet_buffer.take() {
                self.destroy_buffer(old_buffer);
            }
            let new_size = required_size.next_power_of_two().max(64 * 1024);
            let buffer = self.create_host_buffer(usage, new_size)?;
            self.frame_resources.packet_buffer = Some(buffer);
            self.frame_resources.packet_buffer_used = 0;
            offset = 0;
        }
        self.frame_resources.packet_buffer_used = offset + size;
        let buffer = self
            .frame_resources
            .packet_buffer
            .ok_or_else(|| "missing frame packet buffer".to_string())?;
        Ok((buffer, offset))
    }

    pub(super) fn alloc_frame_texture_upload_slice(
        &mut self,
        size: vk::DeviceSize,
    ) -> Result<(VulkanBuffer, vk::DeviceSize), String> {
        let size = size.max(4);
        let alignment = 4;
        let mut offset =
            Self::align_device_size(self.frame_resources.texture_upload_buffer_used, alignment);
        let required_size = offset + size;
        let needs_grow = self
            .frame_resources
            .texture_upload_buffer
            .map(|buffer| buffer.size < required_size)
            .unwrap_or(true);
        if needs_grow {
            if let Some(old_buffer) = self.frame_resources.texture_upload_buffer.take() {
                self.frame_resources.buffers.push(old_buffer);
            }
            let new_size = required_size.next_power_of_two().max(8 * 1024 * 1024);
            let buffer = self.create_host_buffer(vk::BufferUsageFlags::TRANSFER_SRC, new_size)?;
            self.frame_resources.texture_upload_buffer = Some(buffer);
            self.frame_resources.texture_upload_buffer_used = 0;
            offset = 0;
        }
        self.frame_resources.texture_upload_buffer_used = offset + size;
        let buffer = self
            .frame_resources
            .texture_upload_buffer
            .ok_or_else(|| "missing frame texture upload buffer".to_string())?;
        Ok((buffer, offset))
    }
}
