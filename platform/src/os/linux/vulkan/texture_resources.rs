use crate::{
    cx::Cx,
    draw_list::DrawListId,
    makepad_script::shader::TextureType,
    texture::{TextureFormat, TextureId, TexturePixel, TextureUpdated},
};
use ash::vk;
use std::{borrow::Cow, collections::HashSet};

use super::{CxVulkan, VulkanTextureKey, VulkanTextureResource, VulkanTextureUpload};

impl CxVulkan {
    pub(super) fn prepare_draw_list_textures(
        &mut self,
        cx: &mut Cx,
        draw_list_id: DrawListId,
    ) -> Result<(), String> {
        let mut seen = HashSet::<VulkanTextureKey>::new();
        self.prepare_draw_list_textures_inner(cx, draw_list_id, &mut seen)
    }

    fn prepare_draw_list_textures_inner(
        &mut self,
        cx: &mut Cx,
        draw_list_id: DrawListId,
        seen: &mut HashSet<VulkanTextureKey>,
    ) -> Result<(), String> {
        let draw_order_len = cx.draw_lists[draw_list_id].draw_item_order_len();
        for order_index in 0..draw_order_len {
            let Some(draw_item_id) =
                cx.draw_lists[draw_list_id].draw_item_id_at_order_index(order_index)
            else {
                continue;
            };
            let (sub_list_id, texture_ids) = {
                let draw_list = &cx.draw_lists[draw_list_id];
                let draw_item = &draw_list.draw_items[draw_item_id];
                if let Some(sub_list_id) = draw_item.kind.sub_list() {
                    (Some(sub_list_id), Vec::new())
                } else if let Some(draw_call) = draw_item.kind.draw_call() {
                    let sh = &cx.draw_shaders.shaders[draw_call.draw_shader_id.index];
                    let null_texture_id = cx.null_texture.texture_id();
                    let null_cube_texture_id = cx.null_cube_texture.texture_id();
                    let texture_ids = (0..sh.mapping.textures.len())
                        .map(|i| {
                            draw_call.texture_slots[i]
                                .as_ref()
                                .map(|texture| texture.texture_id())
                                .unwrap_or_else(|| {
                                    if matches!(
                                        sh.mapping.textures[i].tex_type,
                                        TextureType::TextureCube | TextureType::TextureCubeArray
                                    ) {
                                        null_cube_texture_id
                                    } else {
                                        null_texture_id
                                    }
                                })
                        })
                        .collect();
                    (None, texture_ids)
                } else {
                    (None, Vec::new())
                }
            };

            if let Some(sub_list_id) = sub_list_id {
                self.prepare_draw_list_textures_inner(cx, sub_list_id, seen)?;
                continue;
            }

            for texture_id in texture_ids {
                if seen.insert(Self::texture_key(texture_id)) {
                    self.ensure_texture_uploaded(cx, texture_id)?;
                }
            }
        }
        Ok(())
    }

    fn vec_texture_meta(format: &TextureFormat) -> Option<(u32, u32, u32, bool, vk::Format)> {
        match format {
            TextureFormat::VecBGRAu8_32 { width, height, .. } => Some((
                *width as u32,
                *height as u32,
                1,
                false,
                vk::Format::B8G8R8A8_UNORM,
            )),
            TextureFormat::VecCubeBGRAu8_32 { width, height, .. } => Some((
                *width as u32,
                *height as u32,
                6,
                true,
                vk::Format::B8G8R8A8_UNORM,
            )),
            TextureFormat::VecMipBGRAu8_32 { width, height, .. } => Some((
                *width as u32,
                *height as u32,
                1,
                false,
                vk::Format::B8G8R8A8_UNORM,
            )),
            TextureFormat::VecRGBAf32 { width, height, .. } => Some((
                *width as u32,
                *height as u32,
                1,
                false,
                vk::Format::R32G32B32A32_SFLOAT,
            )),
            TextureFormat::VecRu8 { width, height, .. } => Some((
                *width as u32,
                *height as u32,
                1,
                false,
                vk::Format::R8_UNORM,
            )),
            TextureFormat::VecRGu8 { width, height, .. } => Some((
                *width as u32,
                *height as u32,
                1,
                false,
                vk::Format::R8G8_UNORM,
            )),
            TextureFormat::VecRf32 { width, height, .. } => Some((
                *width as u32,
                *height as u32,
                1,
                false,
                vk::Format::R32_SFLOAT,
            )),
            _ => None,
        }
    }

    fn texture_upload_rect(
        width: usize,
        height: usize,
        updated: TextureUpdated,
        force_full: bool,
    ) -> Option<(usize, usize, usize, usize)> {
        if width == 0 || height == 0 {
            return None;
        }
        if force_full {
            return Some((0, 0, width, height));
        }
        match updated {
            TextureUpdated::Empty => None,
            TextureUpdated::Full => Some((0, 0, width, height)),
            TextureUpdated::Partial(rect) => {
                let x0 = rect.origin.x.min(width);
                let y0 = rect.origin.y.min(height);
                let x1 = rect.origin.x.saturating_add(rect.size.width).min(width);
                let y1 = rect.origin.y.saturating_add(rect.size.height).min(height);
                if x1 <= x0 || y1 <= y0 {
                    None
                } else {
                    Some((x0, y0, x1 - x0, y1 - y0))
                }
            }
        }
    }

    fn texture_region_bytes<'a>(
        src: &'a [u8],
        src_row_pixels: usize,
        bytes_per_pixel: usize,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) -> Cow<'a, [u8]> {
        let src_row_bytes = src_row_pixels.saturating_mul(bytes_per_pixel);
        let row_bytes = width.saturating_mul(bytes_per_pixel);
        let byte_offset = y
            .saturating_mul(src_row_pixels)
            .saturating_add(x)
            .saturating_mul(bytes_per_pixel);
        let byte_len = row_bytes.saturating_mul(height);
        if x == 0 && row_bytes == src_row_bytes && byte_offset.saturating_add(byte_len) <= src.len()
        {
            return Cow::Borrowed(&src[byte_offset..byte_offset + byte_len]);
        }
        let mut out = vec![0u8; row_bytes.saturating_mul(height)];
        for row in 0..height {
            let src_offset = (y + row)
                .saturating_mul(src_row_pixels)
                .saturating_add(x)
                .saturating_mul(bytes_per_pixel);
            let dst_offset = row.saturating_mul(row_bytes);
            let src_end = src_offset.saturating_add(row_bytes);
            if src_end <= src.len() && dst_offset + row_bytes <= out.len() {
                out[dst_offset..dst_offset + row_bytes].copy_from_slice(&src[src_offset..src_end]);
            }
        }
        Cow::Owned(out)
    }

    fn vec_texture_upload<'a>(
        format: &'a TextureFormat,
        updated: TextureUpdated,
        force_full: bool,
    ) -> Option<VulkanTextureUpload<'a>> {
        match format {
            TextureFormat::VecBGRAu8_32 {
                width,
                height,
                data,
                ..
            }
            | TextureFormat::VecMipBGRAu8_32 {
                width,
                height,
                data,
                ..
            } => {
                let (x, y, w, h) = Self::texture_upload_rect(*width, *height, updated, force_full)?;
                let out = if let Some(data) = data.as_ref() {
                    let src = unsafe {
                        std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4)
                    };
                    Self::texture_region_bytes(src, *width, 4, x, y, w, h)
                } else {
                    Cow::Owned(vec![0u8; w.saturating_mul(h).saturating_mul(4)])
                };
                Some(VulkanTextureUpload {
                    data: out,
                    offset_x: x as u32,
                    offset_y: y as u32,
                    width: w as u32,
                    height: h as u32,
                    layers: 1,
                })
            }
            TextureFormat::VecCubeBGRAu8_32 {
                width,
                height,
                data,
                ..
            } => {
                let w = *width;
                let h = *height;
                if w == 0 || h == 0 {
                    return None;
                }
                let out = if let Some(data) = data.as_ref() {
                    let src = unsafe {
                        std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4)
                    };
                    let expected = w.saturating_mul(h).saturating_mul(4).saturating_mul(6);
                    if src.len() >= expected {
                        Cow::Borrowed(&src[..expected])
                    } else {
                        Cow::Owned(vec![0u8; expected])
                    }
                } else {
                    Cow::Owned(vec![
                        0u8;
                        w.saturating_mul(h).saturating_mul(4).saturating_mul(6)
                    ])
                };
                Some(VulkanTextureUpload {
                    data: out,
                    offset_x: 0,
                    offset_y: 0,
                    width: w as u32,
                    height: h as u32,
                    layers: 6,
                })
            }
            TextureFormat::VecRGBAf32 {
                width,
                height,
                data,
                ..
            } => {
                let (x, y, w, h) = Self::texture_upload_rect(*width, *height, updated, force_full)?;
                let out = if let Some(data) = data.as_ref() {
                    let src = unsafe {
                        std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4)
                    };
                    Self::texture_region_bytes(src, *width, 16, x, y, w, h)
                } else {
                    Cow::Owned(vec![0u8; w.saturating_mul(h).saturating_mul(16)])
                };
                Some(VulkanTextureUpload {
                    data: out,
                    offset_x: x as u32,
                    offset_y: y as u32,
                    width: w as u32,
                    height: h as u32,
                    layers: 1,
                })
            }
            TextureFormat::VecRf32 {
                width,
                height,
                data,
                ..
            } => {
                let (x, y, w, h) = Self::texture_upload_rect(*width, *height, updated, force_full)?;
                let out = if let Some(data) = data.as_ref() {
                    let src = unsafe {
                        std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4)
                    };
                    Self::texture_region_bytes(src, *width, 4, x, y, w, h)
                } else {
                    Cow::Owned(vec![0u8; w.saturating_mul(h).saturating_mul(4)])
                };
                Some(VulkanTextureUpload {
                    data: out,
                    offset_x: x as u32,
                    offset_y: y as u32,
                    width: w as u32,
                    height: h as u32,
                    layers: 1,
                })
            }
            TextureFormat::VecRu8 {
                width,
                height,
                data,
                unpack_row_length,
                ..
            } => {
                let (x, y, w, h) = Self::texture_upload_rect(*width, *height, updated, force_full)?;
                let row_len = unpack_row_length.unwrap_or(*width);
                let out = if let Some(data) = data.as_ref() {
                    Self::texture_region_bytes(data, row_len, 1, x, y, w, h)
                } else {
                    Cow::Owned(vec![0u8; w.saturating_mul(h)])
                };
                Some(VulkanTextureUpload {
                    data: out,
                    offset_x: x as u32,
                    offset_y: y as u32,
                    width: w as u32,
                    height: h as u32,
                    layers: 1,
                })
            }
            TextureFormat::VecRGu8 {
                width,
                height,
                data,
                unpack_row_length,
                ..
            } => {
                let (x, y, w, h) = Self::texture_upload_rect(*width, *height, updated, force_full)?;
                let row_len = unpack_row_length.unwrap_or(*width);
                let out = if let Some(data) = data.as_ref() {
                    Self::texture_region_bytes(data, row_len, 2, x, y, w, h)
                } else {
                    Cow::Owned(vec![0u8; w.saturating_mul(h).saturating_mul(2)])
                };
                Some(VulkanTextureUpload {
                    data: out,
                    offset_x: x as u32,
                    offset_y: y as u32,
                    width: w as u32,
                    height: h as u32,
                    layers: 1,
                })
            }
            _ => None,
        }
    }

    fn create_texture_resource(
        &self,
        width: u32,
        height: u32,
        layers: u32,
        is_cube: bool,
        format: vk::Format,
    ) -> Result<VulkanTextureResource, String> {
        let image_flags = if is_cube {
            vk::ImageCreateFlags::CUBE_COMPATIBLE
        } else {
            vk::ImageCreateFlags::empty()
        };
        let image_info = vk::ImageCreateInfo::default()
            .flags(image_flags)
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: width.max(1),
                height: height.max(1),
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(layers.max(1))
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { self.device.create_image(&image_info, None) }
            .map_err(|e| format!("create_image failed: {e:?}"))?;
        let memory_req = unsafe { self.device.get_image_memory_requirements(image) };
        let memory_type_index = self
            .find_memory_type(
                memory_req.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .or_else(|_| {
                self.find_memory_type(
                    memory_req.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
            })?;
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(memory_req.size)
            .memory_type_index(memory_type_index);
        let memory = match unsafe { self.device.allocate_memory(&alloc_info, None) } {
            Ok(memory) => memory,
            Err(e) => {
                unsafe {
                    self.device.destroy_image(image, None);
                }
                return Err(format!("allocate_memory(image) failed: {e:?}"));
            }
        };
        unsafe {
            if let Err(e) = self.device.bind_image_memory(image, memory, 0) {
                self.device.free_memory(memory, None);
                self.device.destroy_image(image, None);
                return Err(format!("bind_image_memory failed: {e:?}"));
            }
        }
        let view_type = if is_cube {
            vk::ImageViewType::CUBE
        } else {
            vk::ImageViewType::TYPE_2D
        };
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(view_type)
            .format(format)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(layers.max(1)),
            );
        let view = match unsafe { self.device.create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(e) => {
                unsafe {
                    self.device.free_memory(memory, None);
                    self.device.destroy_image(image, None);
                }
                return Err(format!("create_image_view(texture) failed: {e:?}"));
            }
        };
        let mut face_views = [vk::ImageView::null(); 6];
        if is_cube {
            for face in 0..6u32 {
                let face_view_info = vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .base_mip_level(0)
                            .level_count(1)
                            .base_array_layer(face)
                            .layer_count(1),
                    );
                face_views[face as usize] =
                    unsafe { self.device.create_image_view(&face_view_info, None) }.map_err(
                        |e| format!("create_image_view(texture face {face}) failed: {e:?}"),
                    )?;
            }
        }

        Ok(VulkanTextureResource {
            image,
            memory,
            view,
            face_views,
            width: width.max(1),
            height: height.max(1),
            layers: layers.max(1),
            is_cube,
            format,
            layout: vk::ImageLayout::UNDEFINED,
            hardware_buffer: None,
            sampler: None,
            ycbcr_conversion: None,
            ycbcr_conversion_metadata: None,
            owns_sampler_ycbcr_conversion: false,
            owns_image: true,
        })
    }

    pub(super) fn vk_color_format_from_texture_pixel(pixel: TexturePixel) -> Option<vk::Format> {
        match pixel {
            TexturePixel::BGRAu8 => Some(vk::Format::B8G8R8A8_UNORM),
            TexturePixel::RGBAf16 => Some(vk::Format::R16G16B16A16_SFLOAT),
            TexturePixel::RGBAf32 => Some(vk::Format::R32G32B32A32_SFLOAT),
            _ => None,
        }
    }

    pub(super) fn create_color_target_resource(
        &self,
        width: u32,
        height: u32,
        format: vk::Format,
        is_cube: bool,
    ) -> Result<VulkanTextureResource, String> {
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: width.max(1),
                height: height.max(1),
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(if is_cube { 6 } else { 1 })
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED)
            .flags(if is_cube {
                vk::ImageCreateFlags::CUBE_COMPATIBLE
            } else {
                vk::ImageCreateFlags::empty()
            })
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { self.device.create_image(&image_info, None) }
            .map_err(|e| format!("create_image(render_target) failed: {e:?}"))?;
        let memory_req = unsafe { self.device.get_image_memory_requirements(image) };
        let memory_type_index = self
            .find_memory_type(
                memory_req.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .or_else(|_| {
                self.find_memory_type(
                    memory_req.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
            })?;
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(memory_req.size)
            .memory_type_index(memory_type_index);
        let memory = match unsafe { self.device.allocate_memory(&alloc_info, None) } {
            Ok(memory) => memory,
            Err(e) => {
                unsafe {
                    self.device.destroy_image(image, None);
                }
                return Err(format!("allocate_memory(render_target) failed: {e:?}"));
            }
        };
        unsafe {
            if let Err(e) = self.device.bind_image_memory(image, memory, 0) {
                self.device.free_memory(memory, None);
                self.device.destroy_image(image, None);
                return Err(format!("bind_image_memory(render_target) failed: {e:?}"));
            }
        }
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(if is_cube {
                vk::ImageViewType::CUBE
            } else {
                vk::ImageViewType::TYPE_2D
            })
            .format(format)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(if is_cube { 6 } else { 1 }),
            );
        let view = match unsafe { self.device.create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(e) => {
                unsafe {
                    self.device.free_memory(memory, None);
                    self.device.destroy_image(image, None);
                }
                return Err(format!("create_image_view(render_target) failed: {e:?}"));
            }
        };
        let mut face_views = [vk::ImageView::null(); 6];
        if is_cube {
            for face in 0..6u32 {
                let face_view_info = vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .base_mip_level(0)
                            .level_count(1)
                            .base_array_layer(face)
                            .layer_count(1),
                    );
                face_views[face as usize] =
                    unsafe { self.device.create_image_view(&face_view_info, None) }.map_err(
                        |e| format!("create_image_view(render_target face {face}) failed: {e:?}"),
                    )?;
            }
        }

        Ok(VulkanTextureResource {
            image,
            memory,
            view,
            face_views,
            width: width.max(1),
            height: height.max(1),
            layers: if is_cube { 6 } else { 1 },
            is_cube,
            format,
            layout: vk::ImageLayout::UNDEFINED,
            hardware_buffer: None,
            sampler: None,
            ycbcr_conversion: None,
            ycbcr_conversion_metadata: None,
            owns_sampler_ycbcr_conversion: false,
            owns_image: true,
        })
    }

    pub(super) fn transition_image_layout(
        &self,
        image: vk::Image,
        aspect_mask: vk::ImageAspectFlags,
        layer_count: u32,
        old_layout: vk::ImageLayout,
        new_layout: vk::ImageLayout,
    ) {
        let (src_stage, src_access) = Self::layout_stage_access(old_layout);
        let (dst_stage, dst_access) = Self::layout_stage_access(new_layout);
        let barrier = vk::ImageMemoryBarrier::default()
            .old_layout(old_layout)
            .new_layout(new_layout)
            .src_access_mask(src_access)
            .dst_access_mask(dst_access)
            .image(image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(aspect_mask)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(layer_count.max(1)),
            );
        unsafe {
            self.device.cmd_pipeline_barrier(
                self.command_buffer,
                src_stage,
                dst_stage,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
        }
    }

    fn has_stencil_component(format: vk::Format) -> bool {
        matches!(
            format,
            vk::Format::D24_UNORM_S8_UINT | vk::Format::D32_SFLOAT_S8_UINT
        )
    }

    pub(super) fn pick_depth_format(&self) -> Result<vk::Format, String> {
        let candidates = [
            vk::Format::D32_SFLOAT,
            vk::Format::D24_UNORM_S8_UINT,
            vk::Format::D16_UNORM,
        ];
        for format in candidates {
            let props = unsafe {
                self.instance
                    .get_physical_device_format_properties(self.physical_device, format)
            };
            if props
                .optimal_tiling_features
                .contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT)
            {
                return Ok(format);
            }
        }
        Err("No supported Vulkan depth format found".to_string())
    }

    pub(super) fn create_depth_target(
        &self,
        width: u32,
        height: u32,
        format: vk::Format,
    ) -> Result<VulkanTextureResource, String> {
        self.create_depth_target_layers(width, height, format, 1)
    }

    pub(super) fn create_depth_target_layers(
        &self,
        width: u32,
        height: u32,
        format: vk::Format,
        layers: u32,
    ) -> Result<VulkanTextureResource, String> {
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: width.max(1),
                height: height.max(1),
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(layers.max(1))
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let image = unsafe { self.device.create_image(&image_info, None) }
            .map_err(|e| format!("create_image(depth) failed: {e:?}"))?;
        let memory_req = unsafe { self.device.get_image_memory_requirements(image) };
        let memory_type_index = self
            .find_memory_type(
                memory_req.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .or_else(|_| {
                self.find_memory_type(
                    memory_req.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
            })?;
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(memory_req.size)
            .memory_type_index(memory_type_index);
        let memory = match unsafe { self.device.allocate_memory(&alloc_info, None) } {
            Ok(memory) => memory,
            Err(e) => {
                unsafe {
                    self.device.destroy_image(image, None);
                }
                return Err(format!("allocate_memory(depth) failed: {e:?}"));
            }
        };
        unsafe {
            if let Err(e) = self.device.bind_image_memory(image, memory, 0) {
                self.device.free_memory(memory, None);
                self.device.destroy_image(image, None);
                return Err(format!("bind_image_memory(depth) failed: {e:?}"));
            }
        }

        let mut aspect = vk::ImageAspectFlags::DEPTH;
        if Self::has_stencil_component(format) {
            aspect |= vk::ImageAspectFlags::STENCIL;
        }
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(if layers > 1 {
                vk::ImageViewType::TYPE_2D_ARRAY
            } else {
                vk::ImageViewType::TYPE_2D
            })
            .format(format)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(aspect)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(layers.max(1)),
            );
        let view = match unsafe { self.device.create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(e) => {
                unsafe {
                    self.device.free_memory(memory, None);
                    self.device.destroy_image(image, None);
                }
                return Err(format!("create_image_view(depth) failed: {e:?}"));
            }
        };

        Ok(VulkanTextureResource {
            image,
            memory,
            view,
            face_views: [vk::ImageView::null(); 6],
            width: width.max(1),
            height: height.max(1),
            layers: layers.max(1),
            is_cube: false,
            format,
            layout: vk::ImageLayout::UNDEFINED,
            hardware_buffer: None,
            sampler: None,
            ycbcr_conversion: None,
            ycbcr_conversion_metadata: None,
            owns_sampler_ycbcr_conversion: false,
            owns_image: true,
        })
    }

    pub(super) fn create_sampled_depth_resource(
        &self,
        width: u32,
        height: u32,
        format: vk::Format,
    ) -> Result<VulkanTextureResource, String> {
        self.create_sampled_depth_resource_layers(width, height, format, 1)
    }

    pub(super) fn create_sampled_depth_resource_layers(
        &self,
        width: u32,
        height: u32,
        format: vk::Format,
        layers: u32,
    ) -> Result<VulkanTextureResource, String> {
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: width.max(1),
                height: height.max(1),
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(layers.max(1))
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let image = unsafe { self.device.create_image(&image_info, None) }
            .map_err(|e| format!("create_image(sampled_depth) failed: {e:?}"))?;
        let memory_req = unsafe { self.device.get_image_memory_requirements(image) };
        let memory_type_index = self
            .find_memory_type(
                memory_req.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .or_else(|_| {
                self.find_memory_type(
                    memory_req.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
            })?;
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(memory_req.size)
            .memory_type_index(memory_type_index);
        let memory = match unsafe { self.device.allocate_memory(&alloc_info, None) } {
            Ok(memory) => memory,
            Err(e) => {
                unsafe {
                    self.device.destroy_image(image, None);
                }
                return Err(format!("allocate_memory(sampled_depth) failed: {e:?}"));
            }
        };
        unsafe {
            if let Err(e) = self.device.bind_image_memory(image, memory, 0) {
                self.device.free_memory(memory, None);
                self.device.destroy_image(image, None);
                return Err(format!("bind_image_memory(sampled_depth) failed: {e:?}"));
            }
        }

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(if layers > 1 {
                vk::ImageViewType::TYPE_2D_ARRAY
            } else {
                vk::ImageViewType::TYPE_2D
            })
            .format(format)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::DEPTH)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(layers.max(1)),
            );
        let view = match unsafe { self.device.create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(e) => {
                unsafe {
                    self.device.free_memory(memory, None);
                    self.device.destroy_image(image, None);
                }
                return Err(format!("create_image_view(sampled_depth) failed: {e:?}"));
            }
        };

        Ok(VulkanTextureResource {
            image,
            memory,
            view,
            face_views: [vk::ImageView::null(); 6],
            width: width.max(1),
            height: height.max(1),
            layers: layers.max(1),
            is_cube: false,
            format,
            layout: vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL,
            hardware_buffer: None,
            sampler: None,
            ycbcr_conversion: None,
            ycbcr_conversion_metadata: None,
            owns_sampler_ycbcr_conversion: false,
            owns_image: true,
        })
    }

    pub(super) fn ensure_xr_depth_dummy(&mut self) -> Result<vk::ImageView, String> {
        if self.xr_depth_dummy.is_none() {
            self.xr_depth_dummy =
                Some(self.create_sampled_depth_resource(1, 1, vk::Format::D16_UNORM)?);
        }
        Ok(self.xr_depth_dummy.as_ref().unwrap().view)
    }

    pub(super) fn ensure_xr_depth_dummy_multiview(&mut self) -> Result<vk::ImageView, String> {
        if self.xr_depth_dummy_multiview.is_none() {
            self.xr_depth_dummy_multiview =
                Some(self.create_sampled_depth_resource_layers(1, 1, vk::Format::D16_UNORM, 2)?);
        }
        Ok(self.xr_depth_dummy_multiview.as_ref().unwrap().view)
    }

    fn layout_stage_access(layout: vk::ImageLayout) -> (vk::PipelineStageFlags, vk::AccessFlags) {
        match layout {
            vk::ImageLayout::UNDEFINED => (
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::AccessFlags::empty(),
            ),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL => (
                vk::PipelineStageFlags::TRANSFER,
                vk::AccessFlags::TRANSFER_WRITE,
            ),
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL => (
                vk::PipelineStageFlags::FRAGMENT_SHADER | vk::PipelineStageFlags::VERTEX_SHADER,
                vk::AccessFlags::SHADER_READ,
            ),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL => (
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            ),
            vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL => (
                vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
                    | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS,
                vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            ),
            vk::ImageLayout::GENERAL => (
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
            ),
            _ => (
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
            ),
        }
    }

    pub(super) fn texture_key(texture_id: TextureId) -> VulkanTextureKey {
        texture_id.0
    }

    fn ensure_texture_uploaded(
        &mut self,
        cx: &mut Cx,
        texture_id: TextureId,
    ) -> Result<(), String> {
        let texture_key = Self::texture_key(texture_id);
        let (alloc_changed, updated, width, height, layers, is_cube, format) = {
            let cxtexture = &mut cx.textures[texture_id];
            if !cxtexture.format.is_vec() {
                self.ensure_imported_texture_shader_read(texture_id);
                return Ok(());
            }
            let alloc_changed = cxtexture.alloc_vec();
            let updated = cxtexture.take_updated();
            let (width, height, layers, is_cube, format) =
                Self::vec_texture_meta(&cxtexture.format).ok_or_else(|| {
                    format!("unsupported Vulkan texture format: {:?}", cxtexture.format)
                })?;
            (
                alloc_changed,
                updated,
                width,
                height,
                layers,
                is_cube,
                format,
            )
        };

        let needs_recreate = match self.textures.get(&texture_key) {
            Some(resource) => {
                alloc_changed
                    || resource.width != width.max(1)
                    || resource.height != height.max(1)
                    || resource.layers != layers.max(1)
                    || resource.is_cube != is_cube
                    || resource.format != format
            }
            None => true,
        };

        if needs_recreate {
            if let Some(old_resource) = self.textures.remove(&texture_key) {
                self.retire_texture_resource(old_resource);
            }
            let resource = self.create_texture_resource(width, height, layers, is_cube, format)?;
            self.textures.insert(texture_key, resource);
        }

        if matches!(updated, TextureUpdated::Empty) && !needs_recreate {
            return Ok(());
        }

        let force_full_upload = needs_recreate;
        let upload = {
            let cxtexture = &cx.textures[texture_id];
            Self::vec_texture_upload(&cxtexture.format, updated, force_full_upload)
                .ok_or_else(|| format!("texture {} has unsupported upload format", texture_key))?
        };
        if upload.data.is_empty() || upload.width == 0 || upload.height == 0 {
            return Ok(());
        }
        let layer_count = upload.layers.max(1);
        self.texture_upload_count_this_frame += 1;
        self.texture_upload_bytes_this_frame += upload.data.len() as u64;

        let upload_data = upload.data.as_ref();
        let upload_size = upload_data.len() as vk::DeviceSize;
        let (staging, staging_offset) = self.alloc_frame_texture_upload_slice(upload_size)?;
        unsafe {
            let mapped = self
                .device
                .map_memory(
                    staging.memory,
                    staging_offset,
                    upload_size,
                    vk::MemoryMapFlags::empty(),
                )
                .map_err(|e| format!("map_memory(texture_upload_buffer) failed: {e:?}"))?;
            std::ptr::copy_nonoverlapping(
                upload_data.as_ptr(),
                mapped as *mut u8,
                upload_data.len(),
            );
            self.device.unmap_memory(staging.memory);
        }

        let (image, old_layout) = {
            let texture = self
                .textures
                .get(&texture_key)
                .ok_or_else(|| format!("missing Vulkan texture resource for {}", texture_key))?;
            (texture.image, texture.layout)
        };
        let (src_stage, src_access) = Self::layout_stage_access(old_layout);

        let to_transfer = vk::ImageMemoryBarrier::default()
            .src_access_mask(src_access)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .old_layout(old_layout)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .image(image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(layer_count),
            );
        let copy_region = vk::BufferImageCopy::default()
            .buffer_offset(staging_offset)
            .buffer_row_length(0)
            .buffer_image_height(0)
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(0)
                    .base_array_layer(0)
                    .layer_count(layer_count),
            )
            .image_offset(vk::Offset3D {
                x: upload.offset_x as i32,
                y: upload.offset_y as i32,
                z: 0,
            })
            .image_extent(vk::Extent3D {
                width: upload.width,
                height: upload.height,
                depth: 1,
            });
        let to_shader = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .image(image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(layer_count),
            );
        unsafe {
            self.device.cmd_pipeline_barrier(
                self.command_buffer,
                src_stage,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_transfer],
            );
            self.device.cmd_copy_buffer_to_image(
                self.command_buffer,
                staging.buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[copy_region],
            );
            self.device.cmd_pipeline_barrier(
                self.command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER | vk::PipelineStageFlags::VERTEX_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_shader],
            );
        }
        if let Some(texture) = self.textures.get_mut(&texture_key) {
            texture.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        }

        Ok(())
    }

    fn ensure_imported_texture_shader_read(&mut self, texture_id: TextureId) {
        let texture_key = Self::texture_key(texture_id);
        let Some((image, layers, layout, imported_hardware_buffer)) =
            self.textures.get(&texture_key).map(|resource| {
                (
                    resource.image,
                    resource.layers,
                    resource.layout,
                    resource.hardware_buffer.is_some(),
                )
            })
        else {
            return;
        };
        if !imported_hardware_buffer || layout != vk::ImageLayout::UNDEFINED {
            return;
        }

        self.transition_image_layout(
            image,
            vk::ImageAspectFlags::COLOR,
            layers,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        );
        if let Some(resource) = self.textures.get_mut(&texture_key) {
            resource.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        }
        crate::log!(
            "RUSTY_XR_MAKEPAD_VULKAN_VIDEO_IMPORT schema=rusty.xr.makepad-vulkan-video-import.v1 phase=layout-transition status=ok textureKey={} importImageLayout=shader-read-transition oldLayout=undefined newLayout=shader-read-only-optimal",
            texture_key,
        );
    }
}
