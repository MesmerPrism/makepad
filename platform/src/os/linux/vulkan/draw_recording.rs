use crate::{
    cx::Cx, draw_list::DrawListId, draw_pass::DrawPassId, geometry::GeometryId, makepad_live_id::*,
    makepad_script::shader::TextureType, texture::TextureId,
};
use ash::vk;
use std::collections::HashSet;

use super::{
    reflected_shader_descriptor_kind_name, reflected_vulkan_descriptor_type,
    vulkan_descriptor_type_name, CxVulkan, VulkanGeometryResource, VulkanPipelineKey,
    VulkanRenderPassKey,
};

pub(super) struct VulkanDrawPacket {
    pub(super) shader_index: usize,
    pub(super) shader_variant: usize,
    pub(super) geometry_id: GeometryId,
    pub(super) depth_write: bool,
    pub(super) alpha_blend: bool,
    pub(super) backface_culling: bool,
    pub(super) instances: Vec<f32>,
    pub(super) draw_call_uniforms: Vec<f32>,
    pub(super) dyn_uniforms: Vec<f32>,
    pub(super) scope_uniforms: Vec<f32>,
    pub(super) uniform_bindings: Vec<(LiveId, usize)>,
    pub(super) dyn_uniform_binding: u32,
    pub(super) scope_uniform_binding: Option<usize>,
    pub(super) texture_ids: Vec<TextureId>,
    pub(super) texture_types: Vec<TextureType>,
}

#[derive(Default)]
pub(super) struct VulkanDrawStats {
    pub(super) draw_items: usize,
    pub(super) draw_calls: usize,
    pub(super) packets_recorded: usize,
    pub(super) instances: u64,
    pub(super) indices: u64,
    pub(super) skipped_non_draw_call: usize,
    pub(super) skipped_no_os_shader: usize,
    pub(super) skipped_no_vulkan_shader: usize,
    pub(super) skipped_missing_spirv: usize,
    pub(super) skipped_no_instance_slots: usize,
    pub(super) skipped_no_instances_buffer: usize,
    pub(super) skipped_instances_too_short: usize,
    pub(super) skipped_zero_instances: usize,
    pub(super) skipped_no_geometry_id: usize,
    pub(super) skipped_empty_geometry: usize,
}

impl CxVulkan {
    pub(super) fn record_draw_list(
        &mut self,
        cx: &mut Cx,
        draw_pass_id: DrawPassId,
        draw_list_id: DrawListId,
        render_pass_key: &VulkanRenderPassKey,
        zbias: &mut f32,
        zbias_step: f32,
        draw_stats: &mut VulkanDrawStats,
        xr_depth_view: vk::ImageView,
    ) -> Result<(), String> {
        let draw_order_len = cx.draw_lists[draw_list_id].draw_item_order_len();
        for order_index in 0..draw_order_len {
            let Some(draw_item_id) =
                cx.draw_lists[draw_list_id].draw_item_id_at_order_index(order_index)
            else {
                continue;
            };
            let null_texture_id = cx.null_texture.texture_id();
            let null_cube_texture_id = cx.null_cube_texture.texture_id();
            draw_stats.draw_items += 1;
            if let Some(sub_list_id) = cx.draw_lists[draw_list_id].draw_items[draw_item_id]
                .kind
                .sub_list()
            {
                let child_resets_zbias = cx.draw_lists[sub_list_id].reset_zbias;
                let mut child_zbias = 0.0f32;
                self.record_draw_list(
                    cx,
                    draw_pass_id,
                    sub_list_id,
                    render_pass_key,
                    if child_resets_zbias {
                        &mut child_zbias
                    } else {
                        zbias
                    },
                    zbias_step,
                    draw_stats,
                    xr_depth_view,
                )?;
                continue;
            }

            let shader_variant = cx.passes[draw_pass_id].os.shader_variant;
            let packet = {
                let draw_list = &mut cx.draw_lists[draw_list_id];
                let draw_item = &mut draw_list.draw_items[draw_item_id];
                let draw_call = if let Some(draw_call) = draw_item.kind.draw_call_mut() {
                    draw_stats.draw_calls += 1;
                    draw_call
                } else {
                    draw_stats.skipped_non_draw_call += 1;
                    continue;
                };

                let sh = &cx.draw_shaders.shaders[draw_call.draw_shader_id.index];
                let os_shader_id = if let Some(id) = sh.os_shader_id {
                    id
                } else {
                    draw_stats.skipped_no_os_shader += 1;
                    continue;
                };
                let os_shader = &cx.draw_shaders.os_shaders[os_shader_id];
                let vk_shader = if let Some(vk) = &os_shader.vulkan_shader[shader_variant] {
                    vk
                } else {
                    draw_stats.skipped_no_vulkan_shader += 1;
                    continue;
                };
                if vk_shader.vertex_spirv.is_none() || vk_shader.fragment_spirv.is_none() {
                    draw_stats.skipped_missing_spirv += 1;
                    continue;
                }
                if sh.mapping.instances.total_slots == 0 {
                    draw_stats.skipped_no_instance_slots += 1;
                    continue;
                }
                let instances = if let Some(instances) = draw_item.instances.as_ref() {
                    instances.clone()
                } else {
                    draw_stats.skipped_no_instances_buffer += 1;
                    continue;
                };
                if instances.len() < sh.mapping.instances.total_slots {
                    draw_stats.skipped_instances_too_short += 1;
                    continue;
                }
                let instance_count = instances.len() / sh.mapping.instances.total_slots;
                if instance_count == 0 {
                    draw_stats.skipped_zero_instances += 1;
                    continue;
                }
                draw_stats.instances += instance_count as u64;
                let geometry_id = if let Some(geometry_id) = draw_call.geometry_id {
                    geometry_id
                } else {
                    draw_stats.skipped_no_geometry_id += 1;
                    continue;
                };

                if sh.mapping.uses_time {
                    cx.demo_time_repaint = true;
                }

                draw_call.draw_call_uniforms.set_zbias(*zbias);
                *zbias += zbias_step;
                draw_call.instance_dirty = false;
                draw_call.uniforms_dirty = false;
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
                let texture_types = sh.mapping.textures.iter().map(|t| t.tex_type).collect();

                VulkanDrawPacket {
                    shader_index: draw_call.draw_shader_id.index,
                    shader_variant,
                    geometry_id,
                    depth_write: draw_call.options.depth_write,
                    alpha_blend: draw_call.options.alpha_blend,
                    backface_culling: draw_call.options.backface_culling,
                    instances,
                    draw_call_uniforms: draw_call.draw_call_uniforms.as_slice().to_vec(),
                    dyn_uniforms: draw_call.dyn_uniforms[..sh
                        .mapping
                        .dyn_uniforms
                        .total_slots
                        .min(draw_call.dyn_uniforms.len())]
                        .to_vec(),
                    scope_uniforms: sh.mapping.scope_uniforms_buf.clone(),
                    uniform_bindings: sh.mapping.uniform_buffer_bindings.bindings.clone(),
                    dyn_uniform_binding: vk_shader.dyn_uniform_binding,
                    scope_uniform_binding: sh
                        .mapping
                        .uniform_buffer_bindings
                        .scope_uniform_buffer_index,
                    texture_ids,
                    texture_types,
                }
            };

            let geometry = &mut cx.geometries[packet.geometry_id];
            if geometry.indices.is_empty() || geometry.vertices.is_empty() {
                draw_stats.skipped_empty_geometry += 1;
                continue;
            }
            self.ensure_geometry_resource(packet.geometry_id, geometry)?;
            let geometry_resource = self
                .geometries
                .get(&packet.geometry_id)
                .copied()
                .ok_or_else(|| {
                    format!(
                        "missing Vulkan geometry resource for {:?}",
                        packet.geometry_id
                    )
                })?;
            let index_count = geometry.indices.len() as u32;
            draw_stats.indices += index_count as u64;
            let pass_uniforms = cx.passes[draw_pass_id].pass_uniforms.as_slice().to_vec();
            let draw_list_uniforms = cx.draw_lists[draw_list_id]
                .draw_list_uniforms
                .as_slice()
                .to_vec();

            self.record_draw_packet(
                cx,
                &packet,
                render_pass_key,
                geometry_resource,
                index_count,
                &pass_uniforms,
                &draw_list_uniforms,
                xr_depth_view,
            )?;
            draw_stats.packets_recorded += 1;
        }
        Ok(())
    }

    pub(super) fn record_draw_packet(
        &mut self,
        cx: &Cx,
        packet: &VulkanDrawPacket,
        render_pass_key: &VulkanRenderPassKey,
        geometry_resource: VulkanGeometryResource,
        index_count: u32,
        pass_uniforms: &[f32],
        draw_list_uniforms: &[f32],
        xr_depth_view: vk::ImageView,
    ) -> Result<(), String> {
        let video_combined_immutable_samplers =
            self.collect_packet_video_combined_immutable_samplers(cx, packet)?;
        self.ensure_pipeline(
            cx,
            packet.shader_index,
            packet.shader_variant,
            render_pass_key,
            packet.alpha_blend,
            packet.backface_culling,
            &video_combined_immutable_samplers,
        )?;
        let (
            pipeline_handle,
            pipeline_layout,
            descriptor_set_layout,
            pipeline_has_descriptors,
            pipeline_samplers,
        ) = {
            let pipeline_key = VulkanPipelineKey {
                shader_index: packet.shader_index,
                shader_variant: packet.shader_variant,
                render_pass: render_pass_key.clone(),
                alpha_blend: packet.alpha_blend,
                backface_culling: packet.backface_culling,
                video_combined_immutable_samplers: Self::pipeline_video_sampler_key(
                    &video_combined_immutable_samplers,
                ),
            };
            let pipeline = self.pipelines.get(&pipeline_key).ok_or_else(|| {
                format!("missing Vulkan pipeline for shader {}", packet.shader_index)
            })?;
            (
                if packet.depth_write {
                    pipeline.pipeline_write
                } else {
                    pipeline.pipeline_no_write
                },
                pipeline.layout,
                pipeline.descriptor_set_layout,
                pipeline.has_descriptors,
                pipeline.sampler_handles.clone(),
            )
        };

        let sh = &cx.draw_shaders.shaders[packet.shader_index];
        let os_shader_id = sh
            .os_shader_id
            .ok_or_else(|| format!("shader {} missing os_shader_id", packet.shader_index))?;
        let os_shader = &cx.draw_shaders.os_shaders[os_shader_id];
        let vk_shader = os_shader.vulkan_shader[packet.shader_variant]
            .as_ref()
            .ok_or_else(|| format!("shader {} missing Vulkan binary", packet.shader_index))?;
        let geometry_stride =
            (sh.mapping.geometries.total_slots * std::mem::size_of::<f32>()) as u64;
        let instance_stride =
            (sh.mapping.instances.total_slots * std::mem::size_of::<f32>()) as u64;
        if geometry_stride == 0 || instance_stride == 0 {
            return Ok(());
        }
        let instance_count = (packet.instances.len() as u64
            / (instance_stride / std::mem::size_of::<f32>() as u64))
            as u32;
        if instance_count == 0 || index_count == 0 {
            return Ok(());
        }

        struct UniformUpload<'a> {
            binding: u32,
            src: &'a [f32],
            offset: vk::DeviceSize,
            size: vk::DeviceSize,
        }

        let mut uniform_uploads: Vec<UniformUpload<'_>> = Vec::new();
        for (type_name, binding_idx) in &packet.uniform_bindings {
            let src: &[f32] = if *type_name == id!(DrawPassUniforms) {
                pass_uniforms
            } else if *type_name == id!(DrawListUniforms) {
                draw_list_uniforms
            } else if *type_name == id!(DrawCallUniforms) {
                packet.draw_call_uniforms.as_slice()
            } else {
                &[]
            };
            if src.is_empty() {
                continue;
            }
            uniform_uploads.push(UniformUpload {
                binding: *binding_idx as u32,
                src,
                offset: 0,
                size: 0,
            });
        }
        if !packet.dyn_uniforms.is_empty() {
            uniform_uploads.push(UniformUpload {
                binding: packet.dyn_uniform_binding,
                src: packet.dyn_uniforms.as_slice(),
                offset: 0,
                size: 0,
            });
        }
        if let Some(scope_binding) = packet.scope_uniform_binding {
            if !packet.scope_uniforms.is_empty() {
                uniform_uploads.push(UniformUpload {
                    binding: scope_binding as u32,
                    src: packet.scope_uniforms.as_slice(),
                    offset: 0,
                    size: 0,
                });
            }
        }
        uniform_uploads.sort_by_key(|uniform| uniform.binding);
        uniform_uploads.dedup_by_key(|uniform| uniform.binding);

        let mut cursor: vk::DeviceSize = 0;
        let instances_offset = Self::align_device_size(cursor, 4);
        let instances_bytes = std::mem::size_of_val(packet.instances.as_slice()) as vk::DeviceSize;
        cursor = instances_offset + instances_bytes;

        let uniform_alignment = self.min_uniform_buffer_offset_alignment.max(4);
        for uniform in &mut uniform_uploads {
            let size = std::mem::size_of_val(uniform.src) as vk::DeviceSize;
            if size == 0 {
                continue;
            }
            let offset = Self::align_device_size(cursor, uniform_alignment);
            cursor = offset + size;
            uniform.offset = offset;
            uniform.size = size;
        }
        uniform_uploads.retain(|uniform| uniform.size != 0);

        let packet_buffer_usage =
            vk::BufferUsageFlags::VERTEX_BUFFER | vk::BufferUsageFlags::UNIFORM_BUFFER;
        let packet_span_size = cursor.max(4);
        let packet_base_alignment = self.min_uniform_buffer_offset_alignment.max(4);
        let (packet_buffer, packet_base_offset) = self.alloc_frame_packet_slice(
            packet_buffer_usage,
            packet_span_size,
            packet_base_alignment,
        )?;
        unsafe {
            let mapped = self
                .device
                .map_memory(
                    packet_buffer.memory,
                    packet_base_offset,
                    packet_span_size,
                    vk::MemoryMapFlags::empty(),
                )
                .map_err(|e| format!("map_memory(packet_buffer) failed: {e:?}"))?;
            let mapped_ptr = mapped as *mut u8;
            if instances_bytes != 0 {
                std::ptr::copy_nonoverlapping(
                    packet.instances.as_ptr() as *const u8,
                    mapped_ptr.add(instances_offset as usize),
                    instances_bytes as usize,
                );
            }
            for uniform in &uniform_uploads {
                std::ptr::copy_nonoverlapping(
                    uniform.src.as_ptr() as *const u8,
                    mapped_ptr.add(uniform.offset as usize),
                    uniform.size as usize,
                );
            }
            self.device.unmap_memory(packet_buffer.memory);
        }
        self.xr_packet_buffer_count_this_frame += 1;
        self.xr_packet_buffer_bytes_this_frame += packet_span_size as u64;

        let mut texture_bindings = Vec::new();
        let mut texture_descriptor_types = Vec::new();
        let mut texture_infos = Vec::new();
        let mut combined_immutable_sampler_bindings = HashSet::new();
        let mut video_sampler_overrides = std::collections::HashMap::<usize, vk::Sampler>::new();
        let null_texture_key = Self::texture_key(cx.null_texture.texture_id());
        let null_cube_texture_key = Self::texture_key(cx.null_cube_texture.texture_id());
        let null_texture_resource = self.textures.get(&null_texture_key);
        let null_cube_texture_resource = self.textures.get(&null_cube_texture_key);
        for (slot, texture_id) in packet.texture_ids.iter().enumerate() {
            let expected_cube = packet
                .texture_types
                .get(slot)
                .map(|tex_type| {
                    matches!(
                        tex_type,
                        TextureType::TextureCube | TextureType::TextureCubeArray
                    )
                })
                .unwrap_or(false);
            let fallback = if expected_cube {
                null_cube_texture_resource
            } else {
                null_texture_resource
            };
            let texture_key = Self::texture_key(*texture_id);
            let resource = self.textures.get(&texture_key).or(fallback);
            let Some(resource) = resource else {
                return Ok(());
            };
            let sampler_index = sh
                .mapping
                .texture_sampler_indices
                .get(slot)
                .copied()
                .unwrap_or(0);
            let texture_binding = vk_shader.texture_binding_base + slot as u32;
            texture_bindings.push(texture_binding);
            let combined_remap = vk_shader
                .video_combined_image_sampler_remaps
                .iter()
                .find(|remap| remap.texture_binding == texture_binding);
            let combined_immutable_video = combined_remap.is_some()
                && resource.sampler.is_some()
                && resource.ycbcr_conversion.is_some();
            let descriptor_type = if combined_immutable_video {
                vk::DescriptorType::COMBINED_IMAGE_SAMPLER
            } else {
                reflected_vulkan_descriptor_type(
                    vk_shader,
                    texture_binding,
                    vk::DescriptorType::SAMPLED_IMAGE,
                )
            };
            texture_descriptor_types.push(descriptor_type);

            let mut image_info = vk::DescriptorImageInfo::default()
                .image_view(resource.view)
                .image_layout(resource.layout);
            if combined_immutable_video {
                image_info = image_info.sampler(resource.sampler.unwrap());
                combined_immutable_sampler_bindings.insert(combined_remap.unwrap().sampler_binding);
            }
            if resource.sampler.is_some() && resource.ycbcr_conversion.is_some() {
                let report_key = (packet.shader_index, slot, texture_key, sampler_index);
                if self.reported_video_descriptor_shapes.insert(report_key) {
                    let sampler_binding = vk_shader.sampler_binding_base + sampler_index as u32;
                    let remapped_sampler_binding = combined_remap
                        .map(|remap| remap.sampler_binding)
                        .unwrap_or(sampler_binding);
                    let sampler_descriptor_type = if combined_immutable_video {
                        vk::DescriptorType::COMBINED_IMAGE_SAMPLER
                    } else {
                        reflected_vulkan_descriptor_type(
                            vk_shader,
                            sampler_binding,
                            vk::DescriptorType::SAMPLER,
                        )
                    };
                    crate::log!(
                        "RUSTY_XR_MAKEPAD_VULKAN_VIDEO_DESCRIPTOR_SHAPE schema=rusty.xr.makepad-vulkan-video-descriptor-shape.v1 shaderIndex={} slot={} textureKey={} textureType={:?} textureBinding={} shaderTextureResourceKind={} textureDescriptorType={} samplerIndex={} samplerBinding={} remappedSamplerBinding={} shaderSamplerResourceKind={} samplerDescriptorType={} resourceSamplerOverride={} samplerYcbcrConversion=true combinedImageSampler={} immutableSampler={} samplerBindingMode={} samplerBindingCompliance={} effectiveYcbcrModel={} effectiveYcbcrRange={} conversionMode={} shaderSampleLowering={} colorFixAttempt=hwb-external-combined-immutable-v4-default-sampler-remap",
                        packet.shader_index,
                        slot,
                        texture_key,
                        packet.texture_types.get(slot).copied(),
                        texture_binding,
                        reflected_shader_descriptor_kind_name(vk_shader, texture_binding),
                        vulkan_descriptor_type_name(descriptor_type),
                        sampler_index,
                        sampler_binding,
                        remapped_sampler_binding,
                        if combined_immutable_video {
                            "remapped-sampler-same-binding"
                        } else {
                            reflected_shader_descriptor_kind_name(vk_shader, sampler_binding)
                        },
                        vulkan_descriptor_type_name(sampler_descriptor_type),
                        !combined_immutable_video,
                        combined_immutable_video,
                        combined_immutable_video,
                        if combined_immutable_video {
                            "combined-immutable-sampler"
                        } else {
                            "separate-sampled-image-and-mutable-sampler"
                        },
                        if combined_immutable_video {
                            "pure-hwb-reference-combined-immutable"
                        } else {
                            "diagnostic-not-combined-immutable"
                        },
                        resource
                            .ycbcr_conversion_metadata
                            .as_ref()
                            .map(|metadata| metadata.effective_model.as_str())
                            .unwrap_or("unspecified"),
                        resource
                            .ycbcr_conversion_metadata
                            .as_ref()
                            .map(|metadata| metadata.effective_range.as_str())
                            .unwrap_or("unspecified"),
                        resource
                            .ycbcr_conversion_metadata
                            .as_ref()
                            .map(|metadata| metadata.conversion_mode.as_str())
                            .unwrap_or("unspecified"),
                        if combined_immutable_video {
                            "textureSampleLevel_combined_image_sampler_same_binding"
                        } else {
                            "textureSampleLevel_separate_texture_sampler"
                        },
                    );
                }
            }
            if let Some(video_sampler) = resource.sampler {
                video_sampler_overrides.insert(sampler_index, video_sampler);
            }
            texture_infos.push(image_info);
        }

        let mut sampler_bindings = Vec::new();
        let mut sampler_infos = Vec::new();
        for (sampler_index, sampler) in pipeline_samplers.iter().enumerate() {
            let sampler_binding = vk_shader.sampler_binding_base + sampler_index as u32;
            if combined_immutable_sampler_bindings.contains(&sampler_binding) {
                continue;
            }
            sampler_bindings.push(sampler_binding);
            let sampler = video_sampler_overrides
                .get(&sampler_index)
                .copied()
                .unwrap_or(*sampler);
            sampler_infos.push(vk::DescriptorImageInfo::default().sampler(sampler));
        }

        let xr_depth_info = vk::DescriptorImageInfo::default()
            .image_view(xr_depth_view)
            .image_layout(vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL);

        let descriptor_set = if pipeline_has_descriptors {
            if uniform_uploads.is_empty() && texture_infos.is_empty() && sampler_infos.is_empty() {
                return Err(format!(
                    "shader {} expects descriptors but no descriptor payloads were built",
                    packet.shader_index
                ));
            }

            let descriptor_set = self.alloc_frame_descriptor_set(descriptor_set_layout)?;

            let mut buffer_infos = Vec::with_capacity(uniform_uploads.len());
            for uniform in &uniform_uploads {
                buffer_infos.push(
                    vk::DescriptorBufferInfo::default()
                        .buffer(packet_buffer.buffer)
                        .offset(packet_base_offset + uniform.offset)
                        .range(uniform.size),
                );
            }

            let mut writes = Vec::with_capacity(
                uniform_uploads.len() + texture_infos.len() + sampler_infos.len(),
            );
            for (index, uniform) in uniform_uploads.iter().enumerate() {
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(descriptor_set)
                        .dst_binding(uniform.binding)
                        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                        .buffer_info(std::slice::from_ref(&buffer_infos[index])),
                );
            }
            for (index, binding) in texture_bindings.iter().enumerate() {
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(descriptor_set)
                        .dst_binding(*binding)
                        .descriptor_type(texture_descriptor_types[index])
                        .image_info(std::slice::from_ref(&texture_infos[index])),
                );
            }
            for (index, binding) in sampler_bindings.iter().enumerate() {
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(descriptor_set)
                        .dst_binding(*binding)
                        .descriptor_type(vk::DescriptorType::SAMPLER)
                        .image_info(std::slice::from_ref(&sampler_infos[index])),
                );
            }
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(descriptor_set)
                    .dst_binding(vk_shader.xr_depth_binding)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(std::slice::from_ref(&xr_depth_info)),
            );
            unsafe {
                self.device.update_descriptor_sets(&writes, &[]);
            }
            Some(descriptor_set)
        } else {
            None
        };
        let vertex_buffers = [geometry_resource.vertex_buffer.buffer, packet_buffer.buffer];
        let vertex_offsets = [0, packet_base_offset + instances_offset];

        unsafe {
            self.device.cmd_bind_pipeline(
                self.command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                pipeline_handle,
            );
            if let Some(set) = descriptor_set {
                self.device.cmd_bind_descriptor_sets(
                    self.command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    pipeline_layout,
                    0,
                    &[set],
                    &[],
                );
            }
            self.device.cmd_bind_vertex_buffers(
                self.command_buffer,
                0,
                &vertex_buffers,
                &vertex_offsets,
            );
            self.device.cmd_bind_index_buffer(
                self.command_buffer,
                geometry_resource.index_buffer.buffer,
                0,
                vk::IndexType::UINT32,
            );
            self.device
                .cmd_draw_indexed(self.command_buffer, index_count, instance_count, 0, 0, 0);
        }

        Ok(())
    }
}
