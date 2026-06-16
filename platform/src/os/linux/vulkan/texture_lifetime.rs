use crate::os::linux::{android::ndk_sys, module_loader::ModuleLoader};
use ash::vk;
use std::sync::OnceLock;

use super::{
    CxVulkan, RetiredTextureResource, VideoHardwareBufferTextureCacheEntry, VulkanTextureKey,
    VulkanTextureResource,
};

const VIDEO_HARDWARE_BUFFER_TEXTURE_CACHE_LIMIT: usize = 16;

type AHardwareBufferGetIdFn =
    unsafe extern "C" fn(*const ndk_sys::AHardwareBuffer, *mut u64) -> i32;

struct AHardwareBufferGetIdSymbols {
    _lib: ModuleLoader,
    get_id: AHardwareBufferGetIdFn,
}

unsafe impl Send for AHardwareBufferGetIdSymbols {}
unsafe impl Sync for AHardwareBufferGetIdSymbols {}

impl CxVulkan {
    fn ahardware_buffer_get_id_symbols() -> Option<&'static AHardwareBufferGetIdSymbols> {
        static GET_ID_SYMBOLS: OnceLock<Option<AHardwareBufferGetIdSymbols>> = OnceLock::new();
        GET_ID_SYMBOLS
            .get_or_init(|| {
                let lib = ModuleLoader::load("libandroid.so").ok()?;
                let get_id = lib.get_symbol("AHardwareBuffer_getId").ok()?;
                Some(AHardwareBufferGetIdSymbols { _lib: lib, get_id })
            })
            .as_ref()
    }

    pub(super) fn hardware_buffer_cache_key(
        hardware_buffer: *mut ndk_sys::AHardwareBuffer,
    ) -> Option<u64> {
        if hardware_buffer.is_null() {
            return None;
        }
        if let Some(symbols) = Self::ahardware_buffer_get_id_symbols() {
            let mut native_id = 0u64;
            let id_result = unsafe {
                (symbols.get_id)(
                    hardware_buffer as *const ndk_sys::AHardwareBuffer,
                    &mut native_id,
                )
            };
            if id_result == 0 && native_id != 0 {
                return Some(native_id);
            }
        }
        Some(hardware_buffer as usize as u64)
    }

    pub(super) fn texture_resource_hardware_buffer_cache_key(
        resource: &VulkanTextureResource,
    ) -> Option<u64> {
        resource
            .hardware_buffer
            .and_then(Self::hardware_buffer_cache_key)
    }

    fn should_log_video_hardware_buffer_cache_count(count: u64) -> bool {
        count <= 8 || count % 100 == 0
    }

    pub(super) fn take_cached_video_hardware_buffer_texture_resource(
        &mut self,
        texture_key: VulkanTextureKey,
        hardware_buffer_key: u64,
    ) -> Option<VulkanTextureResource> {
        let position = self
            .video_hardware_buffer_texture_cache
            .iter()
            .position(|entry| {
                entry.texture_key == texture_key && entry.hardware_buffer_key == hardware_buffer_key
            });
        if let Some(position) = position {
            let entry = self.video_hardware_buffer_texture_cache.remove(position);
            self.video_hardware_buffer_texture_cache_hit_count = self
                .video_hardware_buffer_texture_cache_hit_count
                .saturating_add(1);
            if Self::should_log_video_hardware_buffer_cache_count(
                self.video_hardware_buffer_texture_cache_hit_count,
            ) {
                crate::log!(
                    "RUSTY_XR_MAKEPAD_VULKAN_VIDEO_HARDWARE_BUFFER_CACHE schema=rusty.xr.makepad-vulkan-video-hardware-buffer-cache.v1 phase=lookup status=hit textureKey={} hardwareBufferKey={} hitCount={} missCount={} evictCount={} cacheSize={} cacheLimit={}",
                    texture_key,
                    hardware_buffer_key,
                    self.video_hardware_buffer_texture_cache_hit_count,
                    self.video_hardware_buffer_texture_cache_miss_count,
                    self.video_hardware_buffer_texture_cache_evict_count,
                    self.video_hardware_buffer_texture_cache.len(),
                    VIDEO_HARDWARE_BUFFER_TEXTURE_CACHE_LIMIT,
                );
            }
            Some(entry.resource)
        } else {
            self.video_hardware_buffer_texture_cache_miss_count = self
                .video_hardware_buffer_texture_cache_miss_count
                .saturating_add(1);
            if Self::should_log_video_hardware_buffer_cache_count(
                self.video_hardware_buffer_texture_cache_miss_count,
            ) {
                crate::log!(
                    "RUSTY_XR_MAKEPAD_VULKAN_VIDEO_HARDWARE_BUFFER_CACHE schema=rusty.xr.makepad-vulkan-video-hardware-buffer-cache.v1 phase=lookup status=miss textureKey={} hardwareBufferKey={} hitCount={} missCount={} evictCount={} cacheSize={} cacheLimit={}",
                    texture_key,
                    hardware_buffer_key,
                    self.video_hardware_buffer_texture_cache_hit_count,
                    self.video_hardware_buffer_texture_cache_miss_count,
                    self.video_hardware_buffer_texture_cache_evict_count,
                    self.video_hardware_buffer_texture_cache.len(),
                    VIDEO_HARDWARE_BUFFER_TEXTURE_CACHE_LIMIT,
                );
            }
            None
        }
    }

    pub(super) fn cache_video_hardware_buffer_texture_resource(
        &mut self,
        texture_key: VulkanTextureKey,
        resource: VulkanTextureResource,
    ) {
        let Some(hardware_buffer_key) = Self::texture_resource_hardware_buffer_cache_key(&resource)
        else {
            self.retire_texture_resource(resource);
            return;
        };
        if let Some(position) = self
            .video_hardware_buffer_texture_cache
            .iter()
            .position(|entry| {
                entry.texture_key == texture_key && entry.hardware_buffer_key == hardware_buffer_key
            })
        {
            let old = self.video_hardware_buffer_texture_cache.remove(position);
            self.retire_texture_resource(old.resource);
        }
        self.video_hardware_buffer_texture_cache
            .push(VideoHardwareBufferTextureCacheEntry {
                texture_key,
                hardware_buffer_key,
                resource,
                last_used_submit_serial: self.gpu_submit_serial,
            });
        self.trim_video_hardware_buffer_texture_cache();
    }

    fn trim_video_hardware_buffer_texture_cache(&mut self) {
        while self.video_hardware_buffer_texture_cache.len()
            > VIDEO_HARDWARE_BUFFER_TEXTURE_CACHE_LIMIT
        {
            let evict_position = self
                .video_hardware_buffer_texture_cache
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last_used_submit_serial)
                .map(|(index, _)| index)
                .unwrap_or(0);
            let entry = self
                .video_hardware_buffer_texture_cache
                .remove(evict_position);
            self.video_hardware_buffer_texture_cache_evict_count = self
                .video_hardware_buffer_texture_cache_evict_count
                .saturating_add(1);
            crate::log!(
                "RUSTY_XR_MAKEPAD_VULKAN_VIDEO_HARDWARE_BUFFER_CACHE schema=rusty.xr.makepad-vulkan-video-hardware-buffer-cache.v1 phase=evict status=retire textureKey={} hardwareBufferKey={} hitCount={} missCount={} evictCount={} cacheSize={} cacheLimit={}",
                entry.texture_key,
                entry.hardware_buffer_key,
                self.video_hardware_buffer_texture_cache_hit_count,
                self.video_hardware_buffer_texture_cache_miss_count,
                self.video_hardware_buffer_texture_cache_evict_count,
                self.video_hardware_buffer_texture_cache.len(),
                VIDEO_HARDWARE_BUFFER_TEXTURE_CACHE_LIMIT,
            );
            self.retire_texture_resource(entry.resource);
        }
    }

    pub(super) fn destroy_texture_resource(&self, resource: VulkanTextureResource) {
        unsafe {
            if resource.owns_sampler_ycbcr_conversion {
                if let Some(sampler) = resource.sampler {
                    self.device.destroy_sampler(sampler, None);
                }
                if let Some(conversion) = resource.ycbcr_conversion {
                    self.device
                        .destroy_sampler_ycbcr_conversion(conversion, None);
                }
            }
            for face_view in resource.face_views {
                if face_view != vk::ImageView::null() {
                    self.device.destroy_image_view(face_view, None);
                }
            }
            if resource.view != vk::ImageView::null() {
                self.device.destroy_image_view(resource.view, None);
            }
            if resource.owns_image && resource.image != vk::Image::null() {
                self.device.destroy_image(resource.image, None);
            }
            if resource.owns_image && resource.memory != vk::DeviceMemory::null() {
                self.device.free_memory(resource.memory, None);
            }
            if resource.owns_image {
                if let Some(hardware_buffer) = resource.hardware_buffer {
                    if !hardware_buffer.is_null() {
                        ndk_sys::AHardwareBuffer_release(hardware_buffer);
                    }
                }
            }
        }
    }

    pub(super) fn retire_texture_resource(&mut self, resource: VulkanTextureResource) {
        let retire_after_submit_serial = self.gpu_submit_serial;
        let hardware_buffer_resource = resource.hardware_buffer.is_some();
        if self.gpu_completed_submit_serial >= retire_after_submit_serial {
            if hardware_buffer_resource {
                crate::log!(
                    "RUSTY_XR_MAKEPAD_VULKAN_RESOURCE_RETIRE schema=rusty.xr.makepad-vulkan-resource-retire.v1 phase=retire status=destroy-now resourceKind=hardware-buffer retireAfterSubmitSerial={} completedSubmitSerial={} pendingRetiredTextureCount={}",
                    retire_after_submit_serial,
                    self.gpu_completed_submit_serial,
                    self.retired_texture_resources.len(),
                );
            }
            self.destroy_texture_resource(resource);
            return;
        }
        let pending_before = self.retired_texture_resources.len();
        if hardware_buffer_resource {
            crate::log!(
                "RUSTY_XR_MAKEPAD_VULKAN_RESOURCE_RETIRE schema=rusty.xr.makepad-vulkan-resource-retire.v1 phase=retire status=deferred resourceKind=hardware-buffer retireAfterSubmitSerial={} completedSubmitSerial={} pendingBefore={} pendingAfter={}",
                retire_after_submit_serial,
                self.gpu_completed_submit_serial,
                pending_before,
                pending_before + 1,
            );
        }
        self.retired_texture_resources.push(RetiredTextureResource {
            retire_after_submit_serial,
            resource,
        });
    }

    pub(super) fn collect_retired_texture_resources(&mut self) {
        if self.retired_texture_resources.is_empty() {
            return;
        }
        let completed = self.gpu_completed_submit_serial;
        let mut pending = Vec::new();
        let mut ready = Vec::new();
        for retired in self.retired_texture_resources.drain(..) {
            if retired.retire_after_submit_serial <= completed {
                ready.push(retired.resource);
            } else {
                pending.push(retired);
            }
        }
        let ready_count = ready.len();
        let ready_hardware_buffer_count = ready
            .iter()
            .filter(|resource| resource.hardware_buffer.is_some())
            .count();
        let pending_count = pending.len();
        let pending_hardware_buffer_count = pending
            .iter()
            .filter(|retired| retired.resource.hardware_buffer.is_some())
            .count();
        self.retired_texture_resources = pending;
        if ready_hardware_buffer_count > 0 {
            crate::log!(
                "RUSTY_XR_MAKEPAD_VULKAN_RESOURCE_RETIRE schema=rusty.xr.makepad-vulkan-resource-retire.v1 phase=collect status=destroy-ready readyCount={} readyHardwareBufferCount={} pendingCount={} pendingHardwareBufferCount={} completedSubmitSerial={}",
                ready_count,
                ready_hardware_buffer_count,
                pending_count,
                pending_hardware_buffer_count,
                completed,
            );
        }
        for resource in ready {
            self.destroy_texture_resource(resource);
        }
    }
}
