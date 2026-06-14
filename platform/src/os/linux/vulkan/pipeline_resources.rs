use crate::{cx::Cx, draw_shader::DrawShaderAttrFormat, makepad_script::shader::TextureType};
use ash::vk::{self, Handle};
use std::collections::{HashMap, HashSet};

use super::shader_descriptors::{
    reflected_shader_descriptor_kind_name, reflected_vulkan_descriptor_type,
    vulkan_descriptor_type_name,
};
use super::{CxVulkan, VulkanDrawPacket, VulkanRenderPassKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct VulkanVideoCombinedImmutableSamplerKey {
    pub(super) texture_binding: u32,
    pub(super) sampler: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct VulkanPipelineKey {
    pub(super) shader_index: usize,
    pub(super) shader_variant: usize,
    pub(super) render_pass: VulkanRenderPassKey,
    pub(super) alpha_blend: bool,
    pub(super) backface_culling: bool,
    pub(super) video_combined_immutable_samplers: Vec<VulkanVideoCombinedImmutableSamplerKey>,
}

pub(super) struct VulkanPipeline {
    pub(super) pipeline_write: vk::Pipeline,
    pub(super) pipeline_no_write: vk::Pipeline,
    pub(super) layout: vk::PipelineLayout,
    pub(super) descriptor_set_layout: vk::DescriptorSetLayout,
    pub(super) has_descriptors: bool,
    pub(super) sampler_handles: Vec<vk::Sampler>,
}

impl CxVulkan {
    pub(super) fn pipeline_video_sampler_key(
        samplers: &[(u32, vk::Sampler)],
    ) -> Vec<VulkanVideoCombinedImmutableSamplerKey> {
        let mut key = samplers
            .iter()
            .map(
                |(texture_binding, sampler)| VulkanVideoCombinedImmutableSamplerKey {
                    texture_binding: *texture_binding,
                    sampler: sampler.as_raw(),
                },
            )
            .collect::<Vec<_>>();
        key.sort_by_key(|entry| (entry.texture_binding, entry.sampler));
        key.dedup();
        key
    }

    pub(super) fn collect_packet_video_combined_immutable_samplers(
        &self,
        cx: &Cx,
        packet: &VulkanDrawPacket,
    ) -> Result<Vec<(u32, vk::Sampler)>, String> {
        let sh = &cx.draw_shaders.shaders[packet.shader_index];
        let os_shader_id = sh
            .os_shader_id
            .ok_or_else(|| format!("shader {} missing os_shader_id", packet.shader_index))?;
        let os_shader = &cx.draw_shaders.os_shaders[os_shader_id];
        let vk_shader = os_shader.vulkan_shader[packet.shader_variant]
            .as_ref()
            .ok_or_else(|| format!("shader {} missing Vulkan binary", packet.shader_index))?;

        let null_texture_key = Self::texture_key(cx.null_texture.texture_id());
        let null_cube_texture_key = Self::texture_key(cx.null_cube_texture.texture_id());
        let null_texture_resource = self.textures.get(&null_texture_key);
        let null_cube_texture_resource = self.textures.get(&null_cube_texture_key);
        let mut samplers = Vec::new();
        for (slot, texture_id) in packet.texture_ids.iter().enumerate() {
            let texture_binding = vk_shader.texture_binding_base + slot as u32;
            if !vk_shader
                .video_combined_image_sampler_remaps
                .iter()
                .any(|remap| remap.texture_binding == texture_binding)
            {
                continue;
            }
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
            let Some(resource) = self.textures.get(&texture_key).or(fallback) else {
                continue;
            };
            if resource.ycbcr_conversion.is_none() {
                continue;
            }
            if let Some(sampler) = resource.sampler {
                samplers.push((texture_binding, sampler));
            }
        }
        Ok(Self::pipeline_video_sampler_key(&samplers)
            .into_iter()
            .map(|entry| (entry.texture_binding, vk::Sampler::from_raw(entry.sampler)))
            .collect())
    }

    pub(super) fn ensure_pipeline(
        &mut self,
        cx: &Cx,
        shader_index: usize,
        shader_variant: usize,
        render_pass_key: &VulkanRenderPassKey,
        alpha_blend: bool,
        backface_culling: bool,
        video_combined_immutable_samplers: &[(u32, vk::Sampler)],
    ) -> Result<(), String> {
        let pipeline_key = VulkanPipelineKey {
            shader_index,
            shader_variant,
            render_pass: render_pass_key.clone(),
            alpha_blend,
            backface_culling,
            video_combined_immutable_samplers: Self::pipeline_video_sampler_key(
                video_combined_immutable_samplers,
            ),
        };
        if self.pipelines.contains_key(&pipeline_key) {
            return Ok(());
        }
        let video_immutable_sampler_by_binding = video_combined_immutable_samplers
            .iter()
            .copied()
            .collect::<HashMap<u32, vk::Sampler>>();

        let sh = &cx.draw_shaders.shaders[shader_index];
        let os_shader_id = sh
            .os_shader_id
            .ok_or_else(|| format!("shader {} missing os_shader_id", shader_index))?;
        let os_shader = &cx.draw_shaders.os_shaders[os_shader_id];
        let vk_shader = os_shader.vulkan_shader[shader_variant]
            .as_ref()
            .ok_or_else(|| format!("shader {} missing Vulkan binary", shader_index))?;
        let vs_spv = vk_shader
            .vertex_spirv
            .as_ref()
            .ok_or_else(|| format!("shader {} missing vertex SPIR-V", shader_index))?;
        let fs_spv = vk_shader
            .fragment_spirv
            .as_ref()
            .ok_or_else(|| format!("shader {} missing fragment SPIR-V", shader_index))?;

        if vk_shader.geometry_slots != sh.mapping.geometries.total_slots
            || vk_shader.instance_slots != sh.mapping.instances.total_slots
        {
            crate::warning!(
                "Android Vulkan slot mismatch: shader={}, wgsl_geom_slots={}, map_geom_slots={}, wgsl_inst_slots={}, map_inst_slots={}",
                shader_index,
                vk_shader.geometry_slots,
                sh.mapping.geometries.total_slots,
                vk_shader.instance_slots,
                sh.mapping.instances.total_slots
            );
        }

        let has_descriptors = !sh.mapping.uniform_buffer_bindings.bindings.is_empty()
            || !sh.mapping.dyn_uniforms.inputs.is_empty()
            || !sh.mapping.scope_uniforms.inputs.is_empty()
            || !sh.mapping.textures.is_empty()
            || !sh.mapping.samplers.is_empty()
            || vk_shader.xr_depth_binding != 0;

        let mut descriptor_bindings: Vec<(u32, vk::DescriptorType)> = Vec::new();
        let mut combined_immutable_sampler_bindings = HashSet::new();
        for (_, idx) in &sh.mapping.uniform_buffer_bindings.bindings {
            let binding = *idx as u32;
            descriptor_bindings.push((
                binding,
                reflected_vulkan_descriptor_type(
                    vk_shader,
                    binding,
                    vk::DescriptorType::UNIFORM_BUFFER,
                ),
            ));
        }
        if !sh.mapping.dyn_uniforms.inputs.is_empty() {
            descriptor_bindings.push((
                vk_shader.dyn_uniform_binding,
                reflected_vulkan_descriptor_type(
                    vk_shader,
                    vk_shader.dyn_uniform_binding,
                    vk::DescriptorType::UNIFORM_BUFFER,
                ),
            ));
        }
        if !sh.mapping.scope_uniforms.inputs.is_empty() {
            if let Some(idx) = sh
                .mapping
                .uniform_buffer_bindings
                .scope_uniform_buffer_index
            {
                let binding = idx as u32;
                descriptor_bindings.push((
                    binding,
                    reflected_vulkan_descriptor_type(
                        vk_shader,
                        binding,
                        vk::DescriptorType::UNIFORM_BUFFER,
                    ),
                ));
            }
        }
        for (slot, texture) in sh.mapping.textures.iter().enumerate() {
            let texture_binding = vk_shader.texture_binding_base + slot as u32;
            let remap = vk_shader
                .video_combined_image_sampler_remaps
                .iter()
                .find(|remap| remap.texture_binding == texture_binding);
            let combined_immutable_video = remap.is_some()
                && video_immutable_sampler_by_binding.contains_key(&texture_binding);
            let texture_descriptor_type = if combined_immutable_video {
                vk::DescriptorType::COMBINED_IMAGE_SAMPLER
            } else {
                reflected_vulkan_descriptor_type(
                    vk_shader,
                    texture_binding,
                    vk::DescriptorType::SAMPLED_IMAGE,
                )
            };
            descriptor_bindings.push((texture_binding, texture_descriptor_type));
            if texture.tex_type == TextureType::TextureVideo {
                let sampler_index = sh
                    .mapping
                    .texture_sampler_indices
                    .get(slot)
                    .copied()
                    .unwrap_or(0);
                let sampler_binding = vk_shader.sampler_binding_base + sampler_index as u32;
                let remapped_sampler_binding = remap
                    .map(|remap| remap.sampler_binding)
                    .unwrap_or(sampler_binding);
                if combined_immutable_video {
                    combined_immutable_sampler_bindings.insert(remapped_sampler_binding);
                }
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
                    "RUSTY_XR_MAKEPAD_VULKAN_VIDEO_SHADER_INTERFACE schema=rusty.xr.makepad-vulkan-video-shader-interface.v1 shaderIndex={} shaderVariant={} slot={} textureBinding={} shaderTextureResourceKind={} textureDescriptorType={} samplerIndex={} samplerBinding={} remappedSamplerBinding={} shaderSamplerResourceKind={} samplerDescriptorType={} combinedImageSamplerExpected={} immutableSamplerExpected={} samplerBindingMode={} samplerBindingCompliance={} wgslTextureType=texture_2d_f32 shaderSampleLowering={} colorFixAttempt=hwb-external-combined-immutable-v4-default-sampler-remap",
                    shader_index,
                    shader_variant,
                    slot,
                    texture_binding,
                    reflected_shader_descriptor_kind_name(vk_shader, texture_binding),
                    vulkan_descriptor_type_name(texture_descriptor_type),
                    sampler_index,
                    sampler_binding,
                    remapped_sampler_binding,
                    if combined_immutable_video {
                        "remapped-sampler-same-binding"
                    } else {
                        reflected_shader_descriptor_kind_name(vk_shader, sampler_binding)
                    },
                    vulkan_descriptor_type_name(sampler_descriptor_type),
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
                    if combined_immutable_video {
                        "textureSampleLevel_combined_image_sampler_same_binding"
                    } else {
                        "textureSampleLevel_separate_texture_sampler"
                    },
                );
            }
        }
        for sampler_index in 0..sh.mapping.samplers.len() {
            let binding = vk_shader.sampler_binding_base + sampler_index as u32;
            if combined_immutable_sampler_bindings.contains(&binding) {
                continue;
            }
            descriptor_bindings.push((
                binding,
                reflected_vulkan_descriptor_type(vk_shader, binding, vk::DescriptorType::SAMPLER),
            ));
        }
        descriptor_bindings.push((
            vk_shader.xr_depth_binding,
            reflected_vulkan_descriptor_type(
                vk_shader,
                vk_shader.xr_depth_binding,
                vk::DescriptorType::SAMPLED_IMAGE,
            ),
        ));
        descriptor_bindings.sort_by_key(|(binding, _)| *binding);
        descriptor_bindings.dedup_by_key(|(binding, _)| *binding);

        let descriptor_set_layout = {
            let mut dsl_bindings = Vec::new();
            let immutable_sampler_storage = descriptor_bindings
                .iter()
                .map(|(binding, _)| {
                    video_immutable_sampler_by_binding
                        .get(binding)
                        .map(|sampler| [*sampler])
                })
                .collect::<Vec<_>>();
            for ((binding, descriptor_type), immutable_samplers) in descriptor_bindings
                .iter()
                .zip(immutable_sampler_storage.iter())
            {
                let mut layout_binding = vk::DescriptorSetLayoutBinding::default()
                    .binding(*binding)
                    .descriptor_count(1)
                    .descriptor_type(*descriptor_type)
                    .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT);
                if let Some(immutable_samplers) = immutable_samplers {
                    layout_binding = layout_binding.immutable_samplers(immutable_samplers);
                }
                dsl_bindings.push(layout_binding);
            }
            let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&dsl_bindings);
            unsafe { self.device.create_descriptor_set_layout(&info, None) }
                .map_err(|e| format!("create_descriptor_set_layout failed: {e:?}"))?
        };

        let set_layouts = [descriptor_set_layout];
        let pipeline_layout_info =
            vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts);
        let pipeline_layout = match unsafe {
            self.device
                .create_pipeline_layout(&pipeline_layout_info, None)
        } {
            Ok(pipeline_layout) => pipeline_layout,
            Err(e) => {
                unsafe {
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err(format!("create_pipeline_layout failed: {e:?}"));
            }
        };

        let vs_module_info = vk::ShaderModuleCreateInfo::default().code(vs_spv);
        let fs_module_info = vk::ShaderModuleCreateInfo::default().code(fs_spv);
        let vs_module = match unsafe { self.device.create_shader_module(&vs_module_info, None) } {
            Ok(vs_module) => vs_module,
            Err(e) => {
                unsafe {
                    self.device.destroy_pipeline_layout(pipeline_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err(format!("create_shader_module(vertex) failed: {e:?}"));
            }
        };
        let fs_module = match unsafe { self.device.create_shader_module(&fs_module_info, None) } {
            Ok(fs_module) => fs_module,
            Err(e) => {
                unsafe {
                    self.device.destroy_shader_module(vs_module, None);
                    self.device.destroy_pipeline_layout(pipeline_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err(format!("create_shader_module(fragment) failed: {e:?}"));
            }
        };

        let vs_entry = std::ffi::CString::new("vertex_main").unwrap();
        let fs_entry = std::ffi::CString::new("fragment_main").unwrap();
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vs_module)
                .name(&vs_entry),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fs_module)
                .name(&fs_entry),
        ];

        let geometry_formats =
            Self::collect_attribute_chunk_formats(sh.mapping.geometries.total_slots);
        let instance_formats =
            Self::collect_attribute_chunk_formats(sh.mapping.instances.total_slots);

        let mut vertex_bindings = Vec::new();
        vertex_bindings.push(
            vk::VertexInputBindingDescription::default()
                .binding(0)
                .stride((sh.mapping.geometries.total_slots * std::mem::size_of::<f32>()) as u32)
                .input_rate(vk::VertexInputRate::VERTEX),
        );
        vertex_bindings.push(
            vk::VertexInputBindingDescription::default()
                .binding(1)
                .stride((sh.mapping.instances.total_slots * std::mem::size_of::<f32>()) as u32)
                .input_rate(vk::VertexInputRate::INSTANCE),
        );

        let mut vertex_attributes = Vec::new();
        let mut location = 0u32;
        for (chunk_index, format) in geometry_formats.iter().enumerate() {
            let remaining = sh
                .mapping
                .geometries
                .total_slots
                .saturating_sub(chunk_index * 4);
            let components = remaining.min(4);
            vertex_attributes.push(
                vk::VertexInputAttributeDescription::default()
                    .location(location)
                    .binding(0)
                    .format(Self::vk_vertex_format(*format, components))
                    .offset((chunk_index * 4 * std::mem::size_of::<f32>()) as u32),
            );
            location += 1;
        }
        for (chunk_index, format) in instance_formats.iter().enumerate() {
            let remaining = sh
                .mapping
                .instances
                .total_slots
                .saturating_sub(chunk_index * 4);
            let components = remaining.min(4);
            vertex_attributes.push(
                vk::VertexInputAttributeDescription::default()
                    .location(location)
                    .binding(1)
                    .format(Self::vk_vertex_format(*format, components))
                    .offset((chunk_index * 4 * std::mem::size_of::<f32>()) as u32),
            );
            location += 1;
        }

        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&vertex_bindings)
            .vertex_attribute_descriptions(&vertex_attributes);
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST)
            .primitive_restart_enable(false);

        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let rasterization = vk::PipelineRasterizationStateCreateInfo::default()
            .depth_clamp_enable(false)
            .rasterizer_discard_enable(false)
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(if backface_culling {
                vk::CullModeFlags::BACK
            } else {
                vk::CullModeFlags::NONE
            })
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let color_blend_attachment = vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(alpha_blend)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA);
        let color_blend_attachments = [color_blend_attachment];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&color_blend_attachments);
        let has_depth = render_pass_key.depth_format.is_some();
        let make_depth_stencil = |depth_write| {
            vk::PipelineDepthStencilStateCreateInfo::default()
                .depth_test_enable(has_depth)
                .depth_write_enable(has_depth && depth_write)
                .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL)
                .depth_bounds_test_enable(false)
                .stencil_test_enable(false)
        };
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let render_pass = self.get_or_create_pipeline_render_pass(render_pass_key)?;
        let depth_stencil_write = make_depth_stencil(true);
        let depth_stencil_no_write = make_depth_stencil(false);
        let create_info_write = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil_write)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(pipeline_layout)
            .render_pass(render_pass)
            .subpass(0);
        let create_info_no_write = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil_no_write)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(pipeline_layout)
            .render_pass(render_pass)
            .subpass(0);

        let pipeline_result = unsafe {
            self.device.create_graphics_pipelines(
                vk::PipelineCache::null(),
                &[create_info_write, create_info_no_write],
                None,
            )
        };

        unsafe {
            self.device.destroy_shader_module(vs_module, None);
            self.device.destroy_shader_module(fs_module, None);
        }
        let (pipeline_write, pipeline_no_write) = match pipeline_result {
            Ok(pipelines) if pipelines.len() >= 2 => (pipelines[0], pipelines[1]),
            Ok(pipelines) => {
                unsafe {
                    for pipeline in pipelines {
                        self.device.destroy_pipeline(pipeline, None);
                    }
                    self.device.destroy_pipeline_layout(pipeline_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err("create_graphics_pipelines returned fewer than 2 pipelines".to_string());
            }
            Err((pipelines, e)) => {
                unsafe {
                    for pipeline in pipelines {
                        self.device.destroy_pipeline(pipeline, None);
                    }
                    self.device.destroy_pipeline_layout(pipeline_layout, None);
                    self.device
                        .destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err(format!("create_graphics_pipelines failed: {e:?}"));
            }
        };

        let mut sampler_handles = Vec::with_capacity(sh.mapping.samplers.len());
        for sampler_desc in &sh.mapping.samplers {
            let filter = match sampler_desc.filter {
                crate::makepad_script::shader::SamplerFilter::Nearest => vk::Filter::NEAREST,
                crate::makepad_script::shader::SamplerFilter::Linear => vk::Filter::LINEAR,
            };
            let (address_mode, border_color) = match sampler_desc.address {
                crate::makepad_script::shader::SamplerAddress::Repeat => (
                    vk::SamplerAddressMode::REPEAT,
                    vk::BorderColor::FLOAT_TRANSPARENT_BLACK,
                ),
                crate::makepad_script::shader::SamplerAddress::ClampToEdge => (
                    vk::SamplerAddressMode::CLAMP_TO_EDGE,
                    vk::BorderColor::FLOAT_TRANSPARENT_BLACK,
                ),
                crate::makepad_script::shader::SamplerAddress::ClampToZero => (
                    vk::SamplerAddressMode::CLAMP_TO_BORDER,
                    vk::BorderColor::FLOAT_TRANSPARENT_BLACK,
                ),
                crate::makepad_script::shader::SamplerAddress::MirroredRepeat => (
                    vk::SamplerAddressMode::MIRRORED_REPEAT,
                    vk::BorderColor::FLOAT_TRANSPARENT_BLACK,
                ),
            };
            let mut sampler_info = vk::SamplerCreateInfo::default()
                .mag_filter(filter)
                .min_filter(filter)
                .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                .address_mode_u(address_mode)
                .address_mode_v(address_mode)
                .address_mode_w(address_mode)
                .border_color(border_color)
                .unnormalized_coordinates(false)
                .compare_enable(false)
                .min_lod(0.0)
                .max_lod(vk::LOD_CLAMP_NONE);
            if sampler_desc.coord == crate::makepad_script::shader::SamplerCoord::Pixel {
                sampler_info = sampler_info.unnormalized_coordinates(true);
            }
            let sampler = match unsafe { self.device.create_sampler(&sampler_info, None) } {
                Ok(sampler) => sampler,
                Err(e) => {
                    unsafe {
                        for sampler in sampler_handles.drain(..) {
                            self.device.destroy_sampler(sampler, None);
                        }
                        self.device.destroy_pipeline(pipeline_write, None);
                        self.device.destroy_pipeline(pipeline_no_write, None);
                        self.device.destroy_pipeline_layout(pipeline_layout, None);
                        self.device
                            .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    }
                    return Err(format!("create_sampler failed: {e:?}"));
                }
            };
            sampler_handles.push(sampler);
        }

        self.pipelines.insert(
            pipeline_key,
            VulkanPipeline {
                pipeline_write,
                pipeline_no_write,
                layout: pipeline_layout,
                descriptor_set_layout,
                has_descriptors,
                sampler_handles,
            },
        );

        Ok(())
    }

    fn collect_attribute_chunk_formats(total_slots: usize) -> Vec<DrawShaderAttrFormat> {
        vec![DrawShaderAttrFormat::Float; (total_slots + 3) / 4]
    }

    fn vk_vertex_format(attr_format: DrawShaderAttrFormat, components: usize) -> vk::Format {
        match (attr_format, components.max(1).min(4)) {
            (DrawShaderAttrFormat::Float, 1) => vk::Format::R32_SFLOAT,
            (DrawShaderAttrFormat::Float, 2) => vk::Format::R32G32_SFLOAT,
            (DrawShaderAttrFormat::Float, 3) => vk::Format::R32G32B32_SFLOAT,
            (DrawShaderAttrFormat::Float, _) => vk::Format::R32G32B32A32_SFLOAT,
            (DrawShaderAttrFormat::UInt, 1) => vk::Format::R32_UINT,
            (DrawShaderAttrFormat::UInt, 2) => vk::Format::R32G32_UINT,
            (DrawShaderAttrFormat::UInt, 3) => vk::Format::R32G32B32_UINT,
            (DrawShaderAttrFormat::UInt, _) => vk::Format::R32G32B32A32_UINT,
            (DrawShaderAttrFormat::SInt, 1) => vk::Format::R32_SINT,
            (DrawShaderAttrFormat::SInt, 2) => vk::Format::R32G32_SINT,
            (DrawShaderAttrFormat::SInt, 3) => vk::Format::R32G32B32_SINT,
            (DrawShaderAttrFormat::SInt, _) => vk::Format::R32G32B32A32_SINT,
        }
    }
}
