use crate::{
    cx::Cx,
    draw_list::DrawListId,
    draw_pass::{DrawPassClearColor, DrawPassClearDepth, DrawPassId},
};
use ash::vk;
use std::{ffi::CStr, time::Instant};

use super::{CxVulkan, FrameResources, VulkanBuffer, VulkanDrawStats, VulkanTextureResource};

const XR_FRAGMENT_DENSITY_MAP_FORMAT: vk::Format = vk::Format::R8G8_UNORM;
pub(super) const XR_MAX_FRAMES_IN_FLIGHT: u32 = 3;
pub(super) const XR_MAX_FRAMES_IN_FLIGHT_LIMIT: u32 = 8;
pub(super) struct VulkanXrInFlightFrame {
    frame_resources: FrameResources,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    timestamp_query_pool: vk::QueryPool,
    pending_timestamp_query: bool,
    submit_serial: u64,
}

struct CxVulkanOpenXrMultiviewTarget {
    framebuffer: vk::Framebuffer,
    color_view: vk::ImageView,
    depth_target: VulkanTextureResource,
    fragment_density_view: vk::ImageView,
}

struct CxVulkanOpenXrSwapchainImage {
    image: vk::Image,
    target: CxVulkanOpenXrMultiviewTarget,
}

#[derive(Clone, Copy)]
pub(crate) struct CxVulkanOpenXrFoveationImageInfo {
    pub image: vk::Image,
}

struct CxVulkanOpenXrDepthImage {
    image: vk::Image,
    views: [vk::ImageView; 2],
    multiview_view: vk::ImageView,
}

pub(crate) struct CxVulkanOpenXrSessionData {
    width: u32,
    height: u32,
    color_format: vk::Format,
    pub(crate) depth_width: u32,
    pub(crate) depth_height: u32,
    color_images: Vec<CxVulkanOpenXrSwapchainImage>,
    depth_images: Vec<CxVulkanOpenXrDepthImage>,
    color_readback_buffer: Option<VulkanBuffer>,
    depth_readback_buffer: Option<VulkanBuffer>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct OpenXrVulkanRepaintStats {
    pub wait_inflight_ms: f64,
    pub prepare_textures_ms: f64,
    pub record_draw_ms: f64,
    pub submit_ms: f64,
    pub gpu_ms: Option<f64>,
    pub texture_upload_count: u32,
    pub texture_upload_bytes: u64,
    pub packet_buffer_count: u32,
    pub packet_buffer_bytes: u64,
    pub geometry_upload_bytes: u64,
    pub descriptor_set_count: u32,
    pub draw_items: u64,
    pub draw_calls: u64,
    pub packets: u64,
    pub instances: u64,
    pub indices: u64,
}

impl CxVulkan {
    fn create_xr_timestamp_query_pool(&self) -> vk::QueryPool {
        if !self.xr_gpu_timestamps_supported {
            return vk::QueryPool::null();
        }
        let create_info = vk::QueryPoolCreateInfo::default()
            .query_type(vk::QueryType::TIMESTAMP)
            .query_count(2);
        match unsafe { self.device.create_query_pool(&create_info, None) } {
            Ok(pool) => pool,
            Err(err) => {
                crate::warning!("OpenXR Vulkan GPU timing query pool creation failed: {err:?}");
                vk::QueryPool::null()
            }
        }
    }

    pub(super) fn create_xr_in_flight_frames(
        &self,
        frame_count: u32,
    ) -> Result<Vec<VulkanXrInFlightFrame>, String> {
        if frame_count == 0 {
            return Ok(Vec::new());
        }
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(frame_count);
        let command_buffers = unsafe { self.device.allocate_command_buffers(&alloc_info) }
            .map_err(|e| format!("allocate_command_buffers(openxr inflight) failed: {e:?}"))?;

        let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        let mut frames: Vec<VulkanXrInFlightFrame> = Vec::with_capacity(command_buffers.len());
        for &command_buffer in &command_buffers {
            let fence = match unsafe { self.device.create_fence(&fence_info, None) } {
                Ok(fence) => fence,
                Err(err) => {
                    unsafe {
                        for frame in &frames {
                            if frame.timestamp_query_pool != vk::QueryPool::null() {
                                self.device
                                    .destroy_query_pool(frame.timestamp_query_pool, None);
                            }
                            self.device.destroy_fence(frame.fence, None);
                        }
                        self.device
                            .free_command_buffers(self.command_pool, &command_buffers);
                    }
                    return Err(format!("create_fence(openxr inflight) failed: {err:?}"));
                }
            };
            frames.push(VulkanXrInFlightFrame {
                frame_resources: FrameResources::default(),
                command_buffer,
                fence,
                timestamp_query_pool: self.create_xr_timestamp_query_pool(),
                pending_timestamp_query: false,
                submit_serial: 0,
            });
        }
        Ok(frames)
    }

    fn xr_in_flight_frame_is_ready(&self, frame: &VulkanXrInFlightFrame) -> Result<bool, String> {
        if frame.fence == vk::Fence::null() {
            return Ok(true);
        }
        match unsafe { self.device.get_fence_status(frame.fence) } {
            Ok(ready) => Ok(ready),
            Err(vk::Result::NOT_READY) => Ok(false),
            Err(err) => Err(format!("get_fence_status(openxr inflight) failed: {err:?}")),
        }
    }

    fn append_xr_in_flight_frames(&mut self, frame_count: u32) -> Result<(), String> {
        if frame_count == 0 {
            return Ok(());
        }
        let mut frames = self.create_xr_in_flight_frames(frame_count)?;
        self.xr_in_flight_frames.append(&mut frames);
        Ok(())
    }

    fn recycle_xr_in_flight_frame(
        &mut self,
        frame: &mut VulkanXrInFlightFrame,
    ) -> Result<(), String> {
        unsafe {
            self.device
                .wait_for_fences(&[frame.fence], true, u64::MAX)
                .map_err(|e| format!("wait_for_fences(openxr inflight) failed: {e:?}"))?;
        }
        self.gpu_completed_submit_serial =
            self.gpu_completed_submit_serial.max(frame.submit_serial);
        frame.submit_serial = 0;
        self.collect_retired_texture_resources();

        if frame.pending_timestamp_query && frame.timestamp_query_pool != vk::QueryPool::null() {
            let mut timestamps = [0u64; 2];
            let query_result = unsafe {
                self.device.get_query_pool_results(
                    frame.timestamp_query_pool,
                    0,
                    &mut timestamps,
                    vk::QueryResultFlags::TYPE_64,
                )
            };
            self.xr_last_gpu_frame_time_ms = match query_result {
                Ok(()) if timestamps[1] >= timestamps[0] => {
                    let ticks = timestamps[1] - timestamps[0];
                    Some((ticks as f64 * self.xr_timestamp_period_ns) / 1_000_000.0)
                }
                Ok(()) => None,
                Err(err) => {
                    crate::warning!("OpenXR Vulkan GPU timing readback failed: {err:?}");
                    None
                }
            };
            frame.pending_timestamp_query = false;
        }

        if frame.timestamp_query_pool != vk::QueryPool::null() {
            unsafe {
                self.device
                    .destroy_query_pool(frame.timestamp_query_pool, None);
            }
            frame.timestamp_query_pool = self.create_xr_timestamp_query_pool();
        }

        self.recycle_owned_frame_resources(&mut frame.frame_resources)?;

        unsafe {
            self.device
                .reset_fences(&[frame.fence])
                .map_err(|e| format!("reset_fences(openxr inflight) failed: {e:?}"))?;
            self.device
                .reset_command_buffer(frame.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| format!("reset_command_buffer(openxr inflight) failed: {e:?}"))?;
        }

        Ok(())
    }

    pub(crate) fn wait_for_openxr_idle(&mut self) -> Result<(), String> {
        for index in 0..self.xr_in_flight_frames.len() {
            let mut frame = std::mem::replace(
                &mut self.xr_in_flight_frames[index],
                VulkanXrInFlightFrame {
                    frame_resources: FrameResources::default(),
                    command_buffer: vk::CommandBuffer::null(),
                    fence: vk::Fence::null(),
                    timestamp_query_pool: vk::QueryPool::null(),
                    pending_timestamp_query: false,
                    submit_serial: 0,
                },
            );
            let result = if frame.fence != vk::Fence::null() {
                self.recycle_xr_in_flight_frame(&mut frame)
            } else {
                Ok(())
            };
            self.xr_in_flight_frames[index] = frame;
            result?;
        }
        Ok(())
    }

    pub(super) fn destroy_xr_in_flight_frames(&mut self) {
        for mut frame in self.xr_in_flight_frames.drain(..) {
            Self::destroy_owned_frame_resources(&self.device, &mut frame.frame_resources);
            unsafe {
                if frame.timestamp_query_pool != vk::QueryPool::null() {
                    self.device
                        .destroy_query_pool(frame.timestamp_query_pool, None);
                }
                if frame.fence != vk::Fence::null() {
                    self.device.destroy_fence(frame.fence, None);
                }
            }
        }
        self.xr_in_flight_index = 0;
    }

    pub(super) fn query_multiview_support(
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
    ) -> bool {
        let mut multiview_features = vk::PhysicalDeviceMultiviewFeatures::default();
        let mut features2 =
            vk::PhysicalDeviceFeatures2::default().push_next(&mut multiview_features);
        unsafe {
            instance.get_physical_device_features2(physical_device, &mut features2);
        }

        let mut multiview_props = vk::PhysicalDeviceMultiviewProperties::default();
        let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut multiview_props);
        unsafe {
            instance.get_physical_device_properties2(physical_device, &mut props2);
        }

        multiview_features.multiview == vk::TRUE && multiview_props.max_multiview_view_count >= 2
    }

    pub(super) fn query_fragment_density_map_support(
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
    ) -> bool {
        let available_exts = match unsafe {
            instance.enumerate_device_extension_properties(physical_device)
        } {
            Ok(exts) => exts,
            Err(err) => {
                crate::warning!(
                    "Android Vulkan XR: failed to enumerate device extensions for fragment density map support: {err:?}"
                );
                return false;
            }
        };
        let has_fragment_density_map_ext = available_exts.iter().any(|ext| {
            let name = unsafe { CStr::from_ptr(ext.extension_name.as_ptr()) };
            name.to_bytes() == vk::EXT_FRAGMENT_DENSITY_MAP_NAME.to_bytes()
        });
        if !has_fragment_density_map_ext {
            return false;
        }

        let mut fragment_density_features =
            vk::PhysicalDeviceFragmentDensityMapFeaturesEXT::default();
        let mut features2 =
            vk::PhysicalDeviceFeatures2::default().push_next(&mut fragment_density_features);
        unsafe {
            instance.get_physical_device_features2(physical_device, &mut features2);
        }
        if fragment_density_features.fragment_density_map != vk::TRUE {
            return false;
        }

        let format_props = unsafe {
            instance.get_physical_device_format_properties(
                physical_device,
                XR_FRAGMENT_DENSITY_MAP_FORMAT,
            )
        };
        format_props
            .optimal_tiling_features
            .contains(vk::FormatFeatureFlags::FRAGMENT_DENSITY_MAP_EXT)
    }

    pub(crate) fn supports_openxr_fixed_foveation(&self) -> bool {
        self.xr_fragment_density_map_enabled
    }

    pub(crate) fn last_openxr_gpu_frame_time_ms(&self) -> Option<f64> {
        self.xr_last_gpu_frame_time_ms
    }

    fn ensure_xr_render_pass_for_format(
        &mut self,
        color_format: vk::Format,
        use_fragment_density_map: bool,
    ) -> Result<(), String> {
        if self.depth_format == vk::Format::UNDEFINED {
            self.depth_format = self.pick_depth_format()?;
        }
        if self.swapchain_format == color_format
            && self.xr_render_pass != vk::RenderPass::null()
            && self.xr_render_pass_uses_fragment_density_map == use_fragment_density_map
        {
            return Ok(());
        }

        if self.xr_render_pass != vk::RenderPass::null() {
            unsafe {
                self.device.destroy_render_pass(self.xr_render_pass, None);
            }
            self.xr_render_pass = vk::RenderPass::null();
        }

        self.swapchain_format = color_format;
        self.xr_render_pass_uses_fragment_density_map = use_fragment_density_map;

        let color_attachment = vk::AttachmentDescription::default()
            .format(color_format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let depth_attachment = vk::AttachmentDescription::default()
            .format(self.depth_format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::DONT_CARE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL);
        let fragment_density_attachment = vk::AttachmentDescription::default()
            .format(XR_FRAGMENT_DENSITY_MAP_FORMAT)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::DONT_CARE)
            .store_op(vk::AttachmentStoreOp::DONT_CARE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::FRAGMENT_DENSITY_MAP_OPTIMAL_EXT)
            .final_layout(vk::ImageLayout::FRAGMENT_DENSITY_MAP_OPTIMAL_EXT);
        let color_ref = vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let depth_ref = vk::AttachmentReference::default()
            .attachment(1)
            .layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL);
        let fragment_density_ref = vk::AttachmentReference::default()
            .attachment(2)
            .layout(vk::ImageLayout::FRAGMENT_DENSITY_MAP_OPTIMAL_EXT);
        let color_refs = [color_ref];
        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_refs)
            .depth_stencil_attachment(&depth_ref);
        let dependencies = [vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
                    | if use_fragment_density_map {
                        vk::PipelineStageFlags::FRAGMENT_DENSITY_PROCESS_EXT
                    } else {
                        vk::PipelineStageFlags::empty()
                    },
            )
            .dst_stage_mask(
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
                    | if use_fragment_density_map {
                        vk::PipelineStageFlags::FRAGMENT_DENSITY_PROCESS_EXT
                    } else {
                        vk::PipelineStageFlags::empty()
                    },
            )
            .dst_access_mask(
                vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
                    | if use_fragment_density_map {
                        vk::AccessFlags::FRAGMENT_DENSITY_MAP_READ_EXT
                    } else {
                        vk::AccessFlags::empty()
                    },
            )];
        let attachments = if use_fragment_density_map {
            vec![
                color_attachment,
                depth_attachment,
                fragment_density_attachment,
            ]
        } else {
            vec![color_attachment, depth_attachment]
        };
        let subpasses = [subpass];
        let mut xr_render_pass_info = vk::RenderPassCreateInfo::default()
            .attachments(&attachments)
            .subpasses(&subpasses)
            .dependencies(&dependencies);
        let mut fragment_density_info = vk::RenderPassFragmentDensityMapCreateInfoEXT::default()
            .fragment_density_map_attachment(fragment_density_ref);
        if use_fragment_density_map {
            xr_render_pass_info = xr_render_pass_info.push_next(&mut fragment_density_info);
        }
        let view_masks = [0b11u32];
        let correlation_masks = [0b11u32];
        let mut multiview_info = vk::RenderPassMultiviewCreateInfo::default()
            .view_masks(&view_masks)
            .correlation_masks(&correlation_masks);
        if self.xr_multiview_enabled {
            xr_render_pass_info = xr_render_pass_info.push_next(&mut multiview_info);
        }
        self.xr_render_pass = unsafe { self.device.create_render_pass(&xr_render_pass_info, None) }
            .map_err(|e| format!("create_render_pass(openxr) failed: {e:?}"))?;
        Ok(())
    }

    pub(crate) fn create_openxr_session_data(
        &mut self,
        color_images: &[vk::Image],
        depth_images: &[vk::Image],
        color_format: vk::Format,
        width: u32,
        height: u32,
        depth_width: u32,
        depth_height: u32,
        foveation_images: Option<&[CxVulkanOpenXrFoveationImageInfo]>,
    ) -> Result<CxVulkanOpenXrSessionData, String> {
        let use_fragment_density_map = self.xr_fragment_density_map_enabled
            && foveation_images.is_some_and(|images| images.len() == color_images.len());
        self.ensure_xr_render_pass_for_format(color_format, use_fragment_density_map)?;
        self.ensure_xr_depth_dummy_multiview()?;

        let depth_readback_buffer = if depth_width > 0 && depth_height > 0 {
            let byte_len = depth_width as vk::DeviceSize
                * depth_height as vk::DeviceSize
                * std::mem::size_of::<u16>() as vk::DeviceSize;
            Some(self.create_host_buffer(vk::BufferUsageFlags::TRANSFER_DST, byte_len)?)
        } else {
            None
        };
        let color_readback_buffer = if width > 0 && height > 0 {
            let byte_len = width as vk::DeviceSize * height as vk::DeviceSize * 4;
            Some(self.create_host_buffer(vk::BufferUsageFlags::TRANSFER_DST, byte_len)?)
        } else {
            None
        };

        let mut xr_color_images = Vec::with_capacity(color_images.len());
        for (index, &image) in color_images.iter().enumerate() {
            let target = self.create_openxr_multiview_target(
                image,
                color_format,
                width,
                height,
                if use_fragment_density_map {
                    foveation_images.and_then(|images| images.get(index))
                } else {
                    None
                },
            )?;
            xr_color_images.push(CxVulkanOpenXrSwapchainImage { image, target });
        }

        let mut xr_depth_images = Vec::with_capacity(depth_images.len());
        let mut depth_view_error: Option<String> = None;
        for &image in depth_images {
            let views = match (
                self.create_openxr_depth_view(image, 0, vk::Format::D16_UNORM),
                self.create_openxr_depth_view(image, 1, vk::Format::D16_UNORM),
            ) {
                (Ok(left), Ok(right)) => [left, right],
                (left, right) => {
                    if let Ok(view) = left {
                        unsafe {
                            self.device.destroy_image_view(view, None);
                        }
                    }
                    if let Ok(view) = right {
                        unsafe {
                            self.device.destroy_image_view(view, None);
                        }
                    }
                    depth_view_error = Some(match (left.err(), right.err()) {
                        (Some(left_err), Some(right_err)) => {
                            format!("{left_err}; {right_err}")
                        }
                        (Some(err), None) | (None, Some(err)) => err,
                        (None, None) => "unknown depth-view creation failure".to_string(),
                    });
                    break;
                }
            };
            let multiview_view =
                match self.create_openxr_depth_array_view(image, vk::Format::D16_UNORM) {
                    Ok(view) => view,
                    Err(err) => {
                        for view in views {
                            unsafe {
                                self.device.destroy_image_view(view, None);
                            }
                        }
                        depth_view_error = Some(err);
                        break;
                    }
                };
            xr_depth_images.push(CxVulkanOpenXrDepthImage {
                image,
                views,
                multiview_view,
            });
        }
        if let Some(err) = depth_view_error {
            crate::warning!(
                "OpenXR Vulkan: environment depth image views unavailable, disabling XR depth sampling: {}",
                err
            );
        }

        Ok(CxVulkanOpenXrSessionData {
            width: width.max(1),
            height: height.max(1),
            color_format,
            depth_width,
            depth_height,
            color_images: xr_color_images,
            depth_images: xr_depth_images,
            color_readback_buffer,
            depth_readback_buffer,
        })
    }

    fn create_openxr_multiview_target(
        &self,
        image: vk::Image,
        color_format: vk::Format,
        width: u32,
        height: u32,
        foveation_image: Option<&CxVulkanOpenXrFoveationImageInfo>,
    ) -> Result<CxVulkanOpenXrMultiviewTarget, String> {
        let color_view = unsafe {
            self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D_ARRAY)
                    .format(color_format)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .base_mip_level(0)
                            .level_count(1)
                            .base_array_layer(0)
                            .layer_count(2),
                    ),
                None,
            )
        }
        .map_err(|e| format!("create_image_view(openxr color multiview) failed: {e:?}"))?;

        let depth_target =
            match self.create_depth_target_layers(width, height, self.depth_format, 2) {
                Ok(depth_target) => depth_target,
                Err(err) => {
                    unsafe {
                        self.device.destroy_image_view(color_view, None);
                    }
                    return Err(format!(
                        "create_depth_target(openxr multiview) failed: {err}"
                    ));
                }
            };

        let fragment_density_view = if let Some(foveation_image) = foveation_image {
            match self.create_openxr_fragment_density_view(foveation_image.image) {
                Ok(view) => view,
                Err(err) => {
                    unsafe {
                        self.device.destroy_image_view(color_view, None);
                    }
                    self.destroy_texture_resource(depth_target);
                    return Err(err);
                }
            }
        } else {
            vk::ImageView::null()
        };

        let attachments = if fragment_density_view != vk::ImageView::null() {
            vec![color_view, depth_target.view, fragment_density_view]
        } else {
            vec![color_view, depth_target.view]
        };
        let framebuffer = match unsafe {
            self.device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(self.xr_render_pass)
                    .width(width.max(1))
                    .height(height.max(1))
                    .layers(1)
                    .attachments(&attachments),
                None,
            )
        } {
            Ok(framebuffer) => framebuffer,
            Err(e) => {
                unsafe {
                    if fragment_density_view != vk::ImageView::null() {
                        self.device.destroy_image_view(fragment_density_view, None);
                    }
                    self.device.destroy_image_view(color_view, None);
                }
                self.destroy_texture_resource(depth_target);
                return Err(format!(
                    "create_framebuffer(openxr multiview) failed: {e:?}"
                ));
            }
        };

        Ok(CxVulkanOpenXrMultiviewTarget {
            framebuffer,
            color_view,
            depth_target,
            fragment_density_view,
        })
    }

    fn create_openxr_fragment_density_view(
        &self,
        image: vk::Image,
    ) -> Result<vk::ImageView, String> {
        unsafe {
            self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(XR_FRAGMENT_DENSITY_MAP_FORMAT)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .base_mip_level(0)
                            .level_count(1)
                            .base_array_layer(0)
                            .layer_count(1),
                    ),
                None,
            )
        }
        .map_err(|e| format!("create_image_view(openxr fragment density map) failed: {e:?}"))
    }

    fn create_openxr_depth_view(
        &self,
        image: vk::Image,
        eye: usize,
        format: vk::Format,
    ) -> Result<vk::ImageView, String> {
        unsafe {
            self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::DEPTH)
                            .base_mip_level(0)
                            .level_count(1)
                            .base_array_layer(eye as u32)
                            .layer_count(1),
                    ),
                None,
            )
        }
        .map_err(|e| format!("create_image_view(openxr depth eye {eye}) failed: {e:?}"))
    }

    fn create_openxr_depth_array_view(
        &self,
        image: vk::Image,
        format: vk::Format,
    ) -> Result<vk::ImageView, String> {
        unsafe {
            self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D_ARRAY)
                    .format(format)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::DEPTH)
                            .base_mip_level(0)
                            .level_count(1)
                            .base_array_layer(0)
                            .layer_count(2),
                    ),
                None,
            )
        }
        .map_err(|e| format!("create_image_view(openxr depth multiview) failed: {e:?}"))
    }

    pub(crate) fn destroy_openxr_session_data(&mut self, session: CxVulkanOpenXrSessionData) {
        if let Err(err) = self.wait_for_openxr_idle() {
            crate::warning!(
                "OpenXR Vulkan: failed to drain in-flight frames before destroy: {err}"
            );
        }
        for image in session.color_images {
            unsafe {
                if image.target.framebuffer != vk::Framebuffer::null() {
                    self.device
                        .destroy_framebuffer(image.target.framebuffer, None);
                }
                if image.target.fragment_density_view != vk::ImageView::null() {
                    self.device
                        .destroy_image_view(image.target.fragment_density_view, None);
                }
                if image.target.color_view != vk::ImageView::null() {
                    self.device
                        .destroy_image_view(image.target.color_view, None);
                }
            }
            self.destroy_texture_resource(image.target.depth_target);
        }
        for image in session.depth_images {
            for view in image.views {
                unsafe {
                    if view != vk::ImageView::null() {
                        self.device.destroy_image_view(view, None);
                    }
                }
            }
            unsafe {
                if image.multiview_view != vk::ImageView::null() {
                    self.device.destroy_image_view(image.multiview_view, None);
                }
            }
        }
        if let Some(buffer) = session.depth_readback_buffer {
            unsafe {
                if buffer.buffer != vk::Buffer::null() {
                    self.device.destroy_buffer(buffer.buffer, None);
                }
                if buffer.memory != vk::DeviceMemory::null() {
                    self.device.free_memory(buffer.memory, None);
                }
            }
        }
        if let Some(buffer) = session.color_readback_buffer {
            unsafe {
                if buffer.buffer != vk::Buffer::null() {
                    self.device.destroy_buffer(buffer.buffer, None);
                }
                if buffer.memory != vk::DeviceMemory::null() {
                    self.device.free_memory(buffer.memory, None);
                }
            }
        }
    }

    pub(crate) fn read_openxr_depth_image(
        &mut self,
        session: &CxVulkanOpenXrSessionData,
        depth_image_index: usize,
        eye_index: usize,
    ) -> Result<Vec<u16>, String> {
        let depth_image = session
            .depth_images
            .get(depth_image_index)
            .ok_or_else(|| format!("invalid OpenXR depth image index {depth_image_index}"))?;
        let staging = session
            .depth_readback_buffer
            .ok_or_else(|| "OpenXR depth readback buffer unavailable".to_string())?;
        if session.depth_width == 0 || session.depth_height == 0 {
            return Err("OpenXR depth swapchain has invalid dimensions".to_string());
        }

        let pixel_count = session.depth_width as usize * session.depth_height as usize;
        let byte_len = pixel_count as vk::DeviceSize * std::mem::size_of::<u16>() as vk::DeviceSize;

        unsafe {
            self.device
                .wait_for_fences(&[self.in_flight_fence], true, u64::MAX)
                .map_err(|e| format!("wait_for_fences(depth readback) failed: {e:?}"))?;
            self.device
                .reset_fences(&[self.in_flight_fence])
                .map_err(|e| format!("reset_fences(depth readback) failed: {e:?}"))?;
        }

        self.destroy_frame_resources();

        unsafe {
            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| format!("reset_command_buffer(depth readback) failed: {e:?}"))?;
            self.device
                .begin_command_buffer(
                    self.command_buffer,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(|e| format!("begin_command_buffer(depth readback) failed: {e:?}"))?;
        }

        let to_transfer = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::SHADER_READ)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .old_layout(vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .image(depth_image.image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::DEPTH)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(eye_index as u32)
                    .layer_count(1),
            );
        let copy_region = vk::BufferImageCopy::default()
            .buffer_offset(0)
            .buffer_row_length(0)
            .buffer_image_height(0)
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::DEPTH)
                    .mip_level(0)
                    .base_array_layer(eye_index as u32)
                    .layer_count(1),
            )
            .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
            .image_extent(vk::Extent3D {
                width: session.depth_width,
                height: session.depth_height,
                depth: 1,
            });
        let to_read_only = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_READ)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)
            .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .new_layout(vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL)
            .image(depth_image.image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::DEPTH)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(eye_index as u32)
                    .layer_count(1),
            );
        let buffer_ready = vk::BufferMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::HOST_READ)
            .buffer(staging.buffer)
            .offset(0)
            .size(byte_len);

        unsafe {
            self.device.cmd_pipeline_barrier(
                self.command_buffer,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_transfer],
            );
            self.device.cmd_copy_image_to_buffer(
                self.command_buffer,
                depth_image.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                staging.buffer,
                &[copy_region],
            );
            self.device.cmd_pipeline_barrier(
                self.command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER | vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[],
                &[buffer_ready],
                &[to_read_only],
            );
            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| format!("end_command_buffer(depth readback) failed: {e:?}"))?;
            self.device
                .queue_submit(
                    self.queue,
                    &[vk::SubmitInfo::default().command_buffers(&[self.command_buffer])],
                    self.in_flight_fence,
                )
                .map_err(|e| format!("queue_submit(depth readback) failed: {e:?}"))?;
            self.device
                .wait_for_fences(&[self.in_flight_fence], true, u64::MAX)
                .map_err(|e| format!("wait_for_fences(depth readback submit) failed: {e:?}"))?;
        }

        let depth = unsafe {
            let mapped = self
                .device
                .map_memory(staging.memory, 0, byte_len, vk::MemoryMapFlags::empty())
                .map_err(|e| format!("map_memory(depth readback) failed: {e:?}"))?;
            let data = std::slice::from_raw_parts(mapped as *const u16, pixel_count).to_vec();
            self.device.unmap_memory(staging.memory);
            data
        };

        Ok(depth)
    }

    pub(crate) fn read_openxr_color_image_rgba(
        &mut self,
        session: &CxVulkanOpenXrSessionData,
        color_image_index: usize,
        eye_index: usize,
    ) -> Result<Vec<u8>, String> {
        let color_image = session
            .color_images
            .get(color_image_index)
            .ok_or_else(|| format!("invalid OpenXR color image index {color_image_index}"))?;
        let staging = session
            .color_readback_buffer
            .ok_or_else(|| "OpenXR color readback buffer unavailable".to_string())?;
        if session.width == 0 || session.height == 0 {
            return Err("OpenXR color readback dimensions are zero".to_string());
        }

        let pixel_count = session.width as usize * session.height as usize;
        let byte_len = pixel_count as vk::DeviceSize * 4;

        unsafe {
            self.device
                .wait_for_fences(&[self.in_flight_fence], true, u64::MAX)
                .map_err(|e| format!("wait_for_fences(color readback) failed: {e:?}"))?;
            self.device
                .reset_fences(&[self.in_flight_fence])
                .map_err(|e| format!("reset_fences(color readback) failed: {e:?}"))?;
            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| format!("reset_command_buffer(color readback) failed: {e:?}"))?;
            self.device
                .begin_command_buffer(
                    self.command_buffer,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(|e| format!("begin_command_buffer(color readback) failed: {e:?}"))?;
        }

        let to_transfer = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .image(color_image.image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(eye_index as u32)
                    .layer_count(1),
            );
        let copy_region = vk::BufferImageCopy::default()
            .buffer_offset(0)
            .buffer_row_length(0)
            .buffer_image_height(0)
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(0)
                    .base_array_layer(eye_index as u32)
                    .layer_count(1),
            )
            .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
            .image_extent(vk::Extent3D {
                width: session.width,
                height: session.height,
                depth: 1,
            });
        let to_color_attachment = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_READ)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .image(color_image.image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(eye_index as u32)
                    .layer_count(1),
            );
        let buffer_ready = vk::BufferMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::HOST_READ)
            .buffer(staging.buffer)
            .offset(0)
            .size(byte_len);

        unsafe {
            self.device.cmd_pipeline_barrier(
                self.command_buffer,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_transfer],
            );
            self.device.cmd_copy_image_to_buffer(
                self.command_buffer,
                color_image.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                staging.buffer,
                &[copy_region],
            );
            self.device.cmd_pipeline_barrier(
                self.command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT | vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[],
                &[buffer_ready],
                &[to_color_attachment],
            );
            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| format!("end_command_buffer(color readback) failed: {e:?}"))?;
            self.device
                .queue_submit(
                    self.queue,
                    &[vk::SubmitInfo::default().command_buffers(&[self.command_buffer])],
                    self.in_flight_fence,
                )
                .map_err(|e| format!("queue_submit(color readback) failed: {e:?}"))?;
            self.device
                .wait_for_fences(&[self.in_flight_fence], true, u64::MAX)
                .map_err(|e| format!("wait_for_fences(color readback submit) failed: {e:?}"))?;
        }

        let mut rgba = unsafe {
            let mapped = self
                .device
                .map_memory(staging.memory, 0, byte_len, vk::MemoryMapFlags::empty())
                .map_err(|e| format!("map_memory(color readback) failed: {e:?}"))?;
            let bytes = std::slice::from_raw_parts(mapped as *const u8, byte_len as usize).to_vec();
            self.device.unmap_memory(staging.memory);
            bytes
        };

        match session.color_format {
            vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB => {
                for px in rgba.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
            }
            vk::Format::R8G8B8A8_UNORM | vk::Format::R8G8B8A8_SRGB => {}
            other => {
                return Err(format!(
                    "OpenXR color readback does not support format {:?}",
                    other
                ));
            }
        }

        Ok(rgba)
    }

    pub(crate) fn draw_openxr_view(
        &mut self,
        cx: &mut Cx,
        draw_pass_id: DrawPassId,
        draw_list_id: DrawListId,
        session: &CxVulkanOpenXrSessionData,
        color_image_index: usize,
        depth_image_index: Option<usize>,
    ) -> Result<OpenXrVulkanRepaintStats, String> {
        let mut stats = OpenXrVulkanRepaintStats::default();
        let color_target = session
            .color_images
            .get(color_image_index)
            .ok_or_else(|| format!("invalid OpenXR color image index {color_image_index}"))?;
        let xr_depth_view = depth_image_index
            .and_then(|index| session.depth_images.get(index))
            .map(|image| image.multiview_view)
            .unwrap_or(self.ensure_xr_depth_dummy_multiview()?);
        let framebuffer = color_target.target.framebuffer;
        let mut xr_frame = if self.xr_in_flight_frames.is_empty() {
            None
        } else {
            let preferred_index = self.xr_in_flight_index % self.xr_in_flight_frames.len();
            let mut selected_index = None;
            for offset in 0..self.xr_in_flight_frames.len() {
                let frame_index = (preferred_index + offset) % self.xr_in_flight_frames.len();
                if self.xr_in_flight_frame_is_ready(&self.xr_in_flight_frames[frame_index])? {
                    selected_index = Some(frame_index);
                    break;
                }
            }
            let frame_index = if let Some(frame_index) = selected_index {
                frame_index
            } else if self.xr_in_flight_frames.len() < XR_MAX_FRAMES_IN_FLIGHT_LIMIT as usize {
                self.append_xr_in_flight_frames(1)?;
                self.xr_in_flight_frames.len() - 1
            } else {
                preferred_index
            };
            let frame = std::mem::replace(
                &mut self.xr_in_flight_frames[frame_index],
                VulkanXrInFlightFrame {
                    frame_resources: FrameResources::default(),
                    command_buffer: vk::CommandBuffer::null(),
                    fence: vk::Fence::null(),
                    timestamp_query_pool: vk::QueryPool::null(),
                    pending_timestamp_query: false,
                    submit_serial: 0,
                },
            );
            Some((frame_index, frame))
        };

        if let Some((frame_index, mut frame)) = xr_frame.take() {
            let recycle_started = Instant::now();
            let recycle_result = self.recycle_xr_in_flight_frame(&mut frame);
            stats.wait_inflight_ms = recycle_started.elapsed().as_secs_f64() * 1000.0;
            if let Err(err) = recycle_result {
                self.xr_in_flight_frames[frame_index] = frame;
                return Err(err);
            }
            std::mem::swap(&mut self.frame_resources, &mut frame.frame_resources);
            std::mem::swap(&mut self.command_buffer, &mut frame.command_buffer);
            std::mem::swap(&mut self.in_flight_fence, &mut frame.fence);
            xr_frame = Some((frame_index, frame));
        }

        let timestamp_query_pool = xr_frame.as_ref().and_then(|(_, frame)| {
            (frame.timestamp_query_pool != vk::QueryPool::null())
                .then_some(frame.timestamp_query_pool)
        });
        let mut draw_stats = VulkanDrawStats::default();

        let result = (|| -> Result<(), String> {
            unsafe {
                self.device
                    .begin_command_buffer(
                        self.command_buffer,
                        &vk::CommandBufferBeginInfo::default()
                            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                    )
                    .map_err(|e| format!("begin_command_buffer(openxr) failed: {e:?}"))?;
            }

            self.texture_upload_count_this_frame = 0;
            self.texture_upload_bytes_this_frame = 0;
            self.xr_packet_buffer_count_this_frame = 0;
            self.xr_packet_buffer_bytes_this_frame = 0;
            self.xr_geometry_upload_bytes_this_frame = 0;
            self.xr_descriptor_set_count_this_frame = 0;
            self.prune_stale_geometry_resources(cx);
            let prepare_textures_started = Instant::now();
            self.prepare_draw_list_textures(cx, draw_list_id)?;
            stats.prepare_textures_ms = prepare_textures_started.elapsed().as_secs_f64() * 1000.0;

            let clear_color = if cx.passes[draw_pass_id].color_textures.is_empty() {
                cx.passes[draw_pass_id].clear_color
            } else {
                match cx.passes[draw_pass_id].color_textures[0].clear_color {
                    DrawPassClearColor::InitWith(color) => color,
                    DrawPassClearColor::ClearWith(color) => color,
                }
            };
            let clear_depth = match cx.passes[draw_pass_id].clear_depth {
                DrawPassClearDepth::InitWith(depth) | DrawPassClearDepth::ClearWith(depth) => depth,
            };
            let clear_values = [
                vk::ClearValue {
                    color: vk::ClearColorValue {
                        float32: [clear_color.x, clear_color.y, clear_color.z, clear_color.w],
                    },
                },
                vk::ClearValue {
                    depth_stencil: vk::ClearDepthStencilValue {
                        depth: clear_depth,
                        stencil: 0,
                    },
                },
                vk::ClearValue {
                    color: vk::ClearColorValue {
                        uint32: [0, 0, 0, 0],
                    },
                },
            ];

            unsafe {
                if let Some(query_pool) = timestamp_query_pool {
                    self.device.cmd_write_timestamp(
                        self.command_buffer,
                        vk::PipelineStageFlags::TOP_OF_PIPE,
                        query_pool,
                        0,
                    );
                }
                self.device.cmd_begin_render_pass(
                    self.command_buffer,
                    &vk::RenderPassBeginInfo::default()
                        .render_pass(self.xr_render_pass)
                        .framebuffer(framebuffer)
                        .render_area(vk::Rect2D {
                            offset: vk::Offset2D { x: 0, y: 0 },
                            extent: vk::Extent2D {
                                width: session.width,
                                height: session.height,
                            },
                        })
                        .clear_values(if self.xr_render_pass_uses_fragment_density_map {
                            &clear_values
                        } else {
                            &clear_values[..2]
                        }),
                    vk::SubpassContents::INLINE,
                );
                self.device.cmd_set_viewport(
                    self.command_buffer,
                    0,
                    &[vk::Viewport {
                        x: 0.0,
                        y: session.height as f32,
                        width: session.width as f32,
                        height: -(session.height as f32),
                        min_depth: 0.0,
                        max_depth: 1.0,
                    }],
                );
                self.device.cmd_set_scissor(
                    self.command_buffer,
                    0,
                    &[vk::Rect2D {
                        offset: vk::Offset2D { x: 0, y: 0 },
                        extent: vk::Extent2D {
                            width: session.width,
                            height: session.height,
                        },
                    }],
                );
            }

            let render_pass_key = self.main_render_pass_key();
            let mut zbias = 0.0f32;
            let zbias_step = cx.passes[draw_pass_id].zbias_step;
            let record_draw_started = Instant::now();
            self.record_draw_list(
                cx,
                draw_pass_id,
                draw_list_id,
                &render_pass_key,
                &mut zbias,
                zbias_step,
                &mut draw_stats,
                xr_depth_view,
            )?;
            stats.record_draw_ms = record_draw_started.elapsed().as_secs_f64() * 1000.0;

            let submit_started = Instant::now();
            unsafe {
                self.device.cmd_end_render_pass(self.command_buffer);
                if let Some(query_pool) = timestamp_query_pool {
                    self.device.cmd_write_timestamp(
                        self.command_buffer,
                        vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                        query_pool,
                        1,
                    );
                }
                self.device
                    .end_command_buffer(self.command_buffer)
                    .map_err(|e| format!("end_command_buffer(openxr) failed: {e:?}"))?;
                self.device
                    .queue_submit(
                        self.queue,
                        &[vk::SubmitInfo::default().command_buffers(&[self.command_buffer])],
                        self.in_flight_fence,
                    )
                    .map_err(|e| format!("queue_submit(openxr) failed: {e:?}"))?;
            }
            stats.submit_ms = submit_started.elapsed().as_secs_f64() * 1000.0;
            Ok(())
        })();

        if let Some((frame_index, mut frame)) = xr_frame {
            std::mem::swap(&mut self.frame_resources, &mut frame.frame_resources);
            std::mem::swap(&mut self.command_buffer, &mut frame.command_buffer);
            std::mem::swap(&mut self.in_flight_fence, &mut frame.fence);
            if result.is_ok() {
                self.gpu_submit_serial = self.gpu_submit_serial.saturating_add(1);
                frame.submit_serial = self.gpu_submit_serial;
                frame.pending_timestamp_query = timestamp_query_pool.is_some();
                self.xr_in_flight_index = (frame_index + 1) % self.xr_in_flight_frames.len();
            }
            self.xr_in_flight_frames[frame_index] = frame;
        }

        if result.is_ok() {
            stats.gpu_ms = self.xr_last_gpu_frame_time_ms;
            stats.texture_upload_count = self.texture_upload_count_this_frame;
            stats.texture_upload_bytes = self.texture_upload_bytes_this_frame;
            stats.packet_buffer_count = self.xr_packet_buffer_count_this_frame;
            stats.packet_buffer_bytes = self.xr_packet_buffer_bytes_this_frame;
            stats.geometry_upload_bytes = self.xr_geometry_upload_bytes_this_frame;
            stats.descriptor_set_count = self.xr_descriptor_set_count_this_frame;
            stats.draw_items = draw_stats.draw_items as u64;
            stats.draw_calls = draw_stats.draw_calls as u64;
            stats.packets = draw_stats.packets_recorded as u64;
            stats.instances = draw_stats.instances;
            stats.indices = draw_stats.indices;
        }

        result.map(|()| stats)
    }
}
