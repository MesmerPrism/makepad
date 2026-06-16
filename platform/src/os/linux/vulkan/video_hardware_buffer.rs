use crate::{
    event::video_playback::{
        VideoTextureDescriptorShape, VideoTextureResourcePath, VideoTextureUpdateMetadata,
        VideoTextureYcbcrConversionMetadata, VideoYuvMetadata,
    },
    os::linux::android::ndk_sys,
    texture::TextureId,
};
use ash::vk::{self, Handle};

use super::{
    CxVulkan, VulkanExternalYcbcrSampler, VulkanExternalYcbcrSamplerKey, VulkanTextureResource,
};

fn ycbcr_component_swizzle_name(swizzle: vk::ComponentSwizzle) -> &'static str {
    match swizzle {
        vk::ComponentSwizzle::IDENTITY => "IDENTITY",
        vk::ComponentSwizzle::ZERO => "ZERO",
        vk::ComponentSwizzle::ONE => "ONE",
        vk::ComponentSwizzle::R => "R",
        vk::ComponentSwizzle::G => "G",
        vk::ComponentSwizzle::B => "B",
        vk::ComponentSwizzle::A => "A",
        _ => "OTHER",
    }
}

fn ycbcr_component_mapping_label(components: vk::ComponentMapping) -> String {
    format!(
        "r:{},g:{},b:{},a:{}",
        ycbcr_component_swizzle_name(components.r),
        ycbcr_component_swizzle_name(components.g),
        ycbcr_component_swizzle_name(components.b),
        ycbcr_component_swizzle_name(components.a),
    )
}

#[derive(Clone, Copy)]
struct ImportedYuvPlaneLayout {
    biplanar: bool,
    plane0_view_format: vk::Format,
    plane1_view_format: vk::Format,
    plane2_view_format: Option<vk::Format>,
}

impl CxVulkan {
    fn create_imported_hardware_buffer_texture_resource(
        &mut self,
        hardware_buffer: *mut ndk_sys::AHardwareBuffer,
        width: u32,
        height: u32,
    ) -> Result<VulkanTextureResource, String> {
        if hardware_buffer.is_null() {
            return Err("Android Vulkan camera import failed: null AHardwareBuffer".to_string());
        }

        let (vk_format, external_format, allocation_size, android_memory_type_bits) = {
            let mut format_properties = vk::AndroidHardwareBufferFormatPropertiesANDROID::default();
            let (allocation_size, android_memory_type_bits) = {
                let mut properties = vk::AndroidHardwareBufferPropertiesANDROID::default()
                    .push_next(&mut format_properties);
                unsafe {
                    self.external_memory_android_hardware_buffer
                        .get_android_hardware_buffer_properties(
                            hardware_buffer.cast(),
                            &mut properties,
                        )
                        .map_err(|e| {
                            format!(
                                "Android Vulkan camera import failed: get_android_hardware_buffer_properties: {e:?}"
                            )
                        })?;
                }
                (properties.allocation_size, properties.memory_type_bits)
            };
            (
                format_properties.format,
                format_properties.external_format,
                allocation_size,
                android_memory_type_bits,
            )
        };
        if vk_format == vk::Format::UNDEFINED {
            return Err(format!(
                "Android Vulkan camera import failed: hardware buffer reported undefined Vulkan format (external_format={external_format})"
            ));
        }

        let mut external_memory = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::ANDROID_HARDWARE_BUFFER_ANDROID);
        let image_info = vk::ImageCreateInfo::default()
            .push_next(&mut external_memory)
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width: width.max(1),
                height: height.max(1),
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { self.device.create_image(&image_info, None) }
            .map_err(|e| format!("Android Vulkan camera import failed: create_image: {e:?}"))?;
        let memory_req = unsafe { self.device.get_image_memory_requirements(image) };
        let compatible_memory_bits = memory_req.memory_type_bits & android_memory_type_bits;
        let memory_type_index = self
            .find_memory_type(
                compatible_memory_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .or_else(|_| {
                self.find_memory_type(compatible_memory_bits, vk::MemoryPropertyFlags::empty())
            })
            .map_err(|err| {
                unsafe {
                    self.device.destroy_image(image, None);
                }
                err
            })?;

        let mut import_info =
            vk::ImportAndroidHardwareBufferInfoANDROID::default().buffer(hardware_buffer.cast());
        let alloc_info = vk::MemoryAllocateInfo::default()
            .push_next(&mut import_info)
            .allocation_size(allocation_size.max(memory_req.size))
            .memory_type_index(memory_type_index);
        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            unsafe {
                self.device.destroy_image(image, None);
            }
            format!("Android Vulkan camera import failed: allocate_memory: {e:?}")
        })?;

        if let Err(e) = unsafe { self.device.bind_image_memory(image, memory, 0) } {
            unsafe {
                self.device.free_memory(memory, None);
                self.device.destroy_image(image, None);
            }
            return Err(format!(
                "Android Vulkan camera import failed: bind_image_memory: {e:?}"
            ));
        }

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk_format)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1),
            );
        let view = match unsafe { self.device.create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(e) => {
                unsafe {
                    self.device.free_memory(memory, None);
                    self.device.destroy_image(image, None);
                }
                return Err(format!(
                    "Android Vulkan camera import failed: create_image_view: {e:?}"
                ));
            }
        };

        unsafe {
            ndk_sys::AHardwareBuffer_acquire(hardware_buffer);
        }
        crate::warning!(
            "Android Vulkan camera import: size={}x{} vk_format={:?} external_format={} alloc_size={}",
            width.max(1),
            height.max(1),
            vk_format,
            external_format,
            allocation_size.max(memory_req.size),
        );

        let resource = VulkanTextureResource {
            image,
            memory,
            view,
            face_views: [vk::ImageView::null(); 6],
            width: width.max(1),
            height: height.max(1),
            layers: 1,
            is_cube: false,
            format: vk_format,
            layout: vk::ImageLayout::UNDEFINED,
            hardware_buffer: Some(hardware_buffer),
            sampler: None,
            ycbcr_conversion: None,
            ycbcr_conversion_metadata: None,
            owns_sampler_ycbcr_conversion: false,
            owns_image: true,
        };
        Ok(resource)
    }

    fn external_ycbcr_sampler_key(
        external_format: u64,
        component_mapping: vk::ComponentMapping,
        ycbcr_model: vk::SamplerYcbcrModelConversion,
        ycbcr_range: vk::SamplerYcbcrRange,
        x_chroma_offset: vk::ChromaLocation,
        y_chroma_offset: vk::ChromaLocation,
    ) -> VulkanExternalYcbcrSamplerKey {
        VulkanExternalYcbcrSamplerKey {
            external_format,
            ycbcr_model: ycbcr_model.as_raw(),
            ycbcr_range: ycbcr_range.as_raw(),
            component_mapping: [
                component_mapping.r.as_raw(),
                component_mapping.g.as_raw(),
                component_mapping.b.as_raw(),
                component_mapping.a.as_raw(),
            ],
            x_chroma_offset: x_chroma_offset.as_raw(),
            y_chroma_offset: y_chroma_offset.as_raw(),
            chroma_filter: vk::Filter::LINEAR.as_raw(),
            force_explicit_reconstruction: false,
        }
    }

    fn get_or_create_external_ycbcr_sampler(
        &mut self,
        external_format: u64,
        format_props: &vk::AndroidHardwareBufferFormatPropertiesANDROID,
    ) -> Result<
        (
            vk::SamplerYcbcrConversion,
            vk::Sampler,
            VideoTextureYcbcrConversionMetadata,
            bool,
            vk::SamplerYcbcrModelConversion,
            vk::SamplerYcbcrRange,
            vk::SamplerYcbcrModelConversion,
            vk::SamplerYcbcrRange,
            String,
        ),
        String,
    > {
        let suggested_ycbcr_model = format_props.suggested_ycbcr_model;
        let suggested_ycbcr_range = format_props.suggested_ycbcr_range;
        let effective_ycbcr_model = vk::SamplerYcbcrModelConversion::YCBCR_601;
        let effective_ycbcr_range = vk::SamplerYcbcrRange::ITU_NARROW;
        let component_mapping = format_props.sampler_ycbcr_conversion_components;
        let x_chroma_offset = format_props.suggested_x_chroma_offset;
        let y_chroma_offset = format_props.suggested_y_chroma_offset;
        let ycbcr_components = ycbcr_component_mapping_label(component_mapping);
        let key = Self::external_ycbcr_sampler_key(
            external_format,
            component_mapping,
            effective_ycbcr_model,
            effective_ycbcr_range,
            x_chroma_offset,
            y_chroma_offset,
        );

        if let Some(cached) = self.external_ycbcr_samplers.get(&key) {
            crate::log!(
                "RUSTY_XR_MAKEPAD_VULKAN_VIDEO_IMPORT schema=rusty.xr.makepad-vulkan-video-import.v1 phase=ycbcr-sampler-cache status=reused externalFormat={} samplerHandle={} conversionHandle={} samplerBindingMode=combined-immutable-sampler stableImmutableSampler=true pipelineKeyStable=true colorFixAttempt=hwb-external-combined-immutable-v4-default-sampler-remap",
                external_format,
                cached.sampler.as_raw(),
                cached.conversion.as_raw(),
            );
            return Ok((
                cached.conversion,
                cached.sampler,
                cached.metadata.clone(),
                true,
                suggested_ycbcr_model,
                suggested_ycbcr_range,
                effective_ycbcr_model,
                effective_ycbcr_range,
                ycbcr_components,
            ));
        }

        let mut conversion_external_format =
            vk::ExternalFormatANDROID::default().external_format(external_format);
        let conversion_info = vk::SamplerYcbcrConversionCreateInfo::default()
            .push_next(&mut conversion_external_format)
            .format(vk::Format::UNDEFINED)
            .ycbcr_model(effective_ycbcr_model)
            .ycbcr_range(effective_ycbcr_range)
            .components(component_mapping)
            .x_chroma_offset(x_chroma_offset)
            .y_chroma_offset(y_chroma_offset)
            .chroma_filter(vk::Filter::LINEAR)
            .force_explicit_reconstruction(false);
        let ycbcr_conversion = unsafe {
            self.device
                .create_sampler_ycbcr_conversion(&conversion_info, None)
        }
        .map_err(|e| {
            format!("Android Vulkan camera import failed: create_sampler_ycbcr_conversion: {e:?}")
        })?;

        let mut sampler_conversion =
            vk::SamplerYcbcrConversionInfo::default().conversion(ycbcr_conversion);
        let sampler_info = vk::SamplerCreateInfo::default()
            .push_next(&mut sampler_conversion)
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .unnormalized_coordinates(false)
            .compare_enable(false)
            .min_lod(0.0)
            .max_lod(vk::LOD_CLAMP_NONE);
        let sampler = unsafe { self.device.create_sampler(&sampler_info, None) }.map_err(|e| {
            unsafe {
                self.device
                    .destroy_sampler_ycbcr_conversion(ycbcr_conversion, None);
            }
            format!("Android Vulkan camera import failed: create_sampler(external): {e:?}")
        })?;

        let metadata = VideoTextureYcbcrConversionMetadata {
            suggested_model: format!("{suggested_ycbcr_model:?}"),
            suggested_range: format!("{suggested_ycbcr_range:?}"),
            effective_model: format!("{effective_ycbcr_model:?}"),
            effective_range: format!("{effective_ycbcr_range:?}"),
            components: ycbcr_components.clone(),
            suggested_x_chroma_offset: format!("{x_chroma_offset:?}"),
            suggested_y_chroma_offset: format!("{y_chroma_offset:?}"),
            conversion_mode: "forced-bt601-limited-cpuyuv-reference".to_string(),
            sampler_binding_mode: "combined-immutable-sampler".to_string(),
            sampler_binding_compliance: "pure-hwb-reference-combined-immutable".to_string(),
            shader_sample_lowering: "textureSampleLevel_combined_image_sampler_same_binding"
                .to_string(),
        };
        self.external_ycbcr_samplers.insert(
            key,
            VulkanExternalYcbcrSampler {
                sampler,
                conversion: ycbcr_conversion,
                metadata: metadata.clone(),
            },
        );
        crate::log!(
            "RUSTY_XR_MAKEPAD_VULKAN_VIDEO_IMPORT schema=rusty.xr.makepad-vulkan-video-import.v1 phase=ycbcr-sampler-cache status=created externalFormat={} samplerHandle={} conversionHandle={} samplerBindingMode=combined-immutable-sampler stableImmutableSampler=true pipelineKeyStable=true colorFixAttempt=hwb-external-combined-immutable-v4-default-sampler-remap",
            external_format,
            sampler.as_raw(),
            ycbcr_conversion.as_raw(),
        );
        Ok((
            ycbcr_conversion,
            sampler,
            metadata,
            false,
            suggested_ycbcr_model,
            suggested_ycbcr_range,
            effective_ycbcr_model,
            effective_ycbcr_range,
            ycbcr_components,
        ))
    }

    fn create_imported_external_hardware_buffer_texture_resource(
        &mut self,
        hardware_buffer: *mut ndk_sys::AHardwareBuffer,
        width: u32,
        height: u32,
    ) -> Result<
        (
            VulkanTextureResource,
            String,
            Option<u64>,
            Option<VideoTextureYcbcrConversionMetadata>,
        ),
        String,
    > {
        if hardware_buffer.is_null() {
            return Err("Android Vulkan camera import failed: null AHardwareBuffer".to_string());
        }

        let (vk_format, external_format, allocation_size, android_memory_type_bits, format_props) = {
            let mut format_properties = vk::AndroidHardwareBufferFormatPropertiesANDROID::default();
            let (allocation_size, android_memory_type_bits) = {
                let mut properties = vk::AndroidHardwareBufferPropertiesANDROID::default()
                    .push_next(&mut format_properties);
                unsafe {
                    self.external_memory_android_hardware_buffer
                        .get_android_hardware_buffer_properties(
                            hardware_buffer.cast(),
                            &mut properties,
                        )
                        .map_err(|e| {
                            format!(
                                "Android Vulkan camera import failed: get_android_hardware_buffer_properties: {e:?}"
                            )
                        })?;
                }
                (properties.allocation_size, properties.memory_type_bits)
            };
            (
                format_properties.format,
                format_properties.external_format,
                allocation_size,
                android_memory_type_bits,
                format_properties,
            )
        };

        if external_format == 0 {
            if vk_format != vk::Format::UNDEFINED {
                let resource = self.create_imported_hardware_buffer_texture_resource(
                    hardware_buffer,
                    width,
                    height,
                )?;
                return Ok((resource, format!("{vk_format:?}"), None, None));
            }
            return Err(
                "Android Vulkan camera import failed: external-format camera buffer missing external format"
                    .to_string(),
            );
        }

        let mut external_memory = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::ANDROID_HARDWARE_BUFFER_ANDROID);
        let mut external_format_info =
            vk::ExternalFormatANDROID::default().external_format(external_format);
        let image_info = vk::ImageCreateInfo::default()
            .push_next(&mut external_memory)
            .push_next(&mut external_format_info)
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::UNDEFINED)
            .extent(vk::Extent3D {
                width: width.max(1),
                height: height.max(1),
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { self.device.create_image(&image_info, None) }.map_err(|e| {
            format!("Android Vulkan camera import failed: create_image(external): {e:?}")
        })?;
        let memory_req = unsafe { self.device.get_image_memory_requirements(image) };
        let compatible_memory_bits = memory_req.memory_type_bits & android_memory_type_bits;
        let memory_type_index = self
            .find_memory_type(
                compatible_memory_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .or_else(|_| {
                self.find_memory_type(compatible_memory_bits, vk::MemoryPropertyFlags::empty())
            })
            .map_err(|err| {
                unsafe {
                    self.device.destroy_image(image, None);
                }
                err
            })?;

        let mut import_info =
            vk::ImportAndroidHardwareBufferInfoANDROID::default().buffer(hardware_buffer.cast());
        let alloc_info = vk::MemoryAllocateInfo::default()
            .push_next(&mut import_info)
            .allocation_size(allocation_size.max(memory_req.size))
            .memory_type_index(memory_type_index);
        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            unsafe {
                self.device.destroy_image(image, None);
            }
            format!("Android Vulkan camera import failed: allocate_memory(external): {e:?}")
        })?;

        if let Err(e) = unsafe { self.device.bind_image_memory(image, memory, 0) } {
            unsafe {
                self.device.free_memory(memory, None);
                self.device.destroy_image(image, None);
            }
            return Err(format!(
                "Android Vulkan camera import failed: bind_image_memory(external): {e:?}"
            ));
        }

        let (
            ycbcr_conversion,
            sampler,
            ycbcr_conversion_metadata,
            ycbcr_sampler_cache_reused,
            suggested_ycbcr_model,
            suggested_ycbcr_range,
            effective_ycbcr_model,
            effective_ycbcr_range,
            ycbcr_components,
        ) = self
            .get_or_create_external_ycbcr_sampler(external_format, &format_props)
            .map_err(|err| {
                unsafe {
                    self.device.free_memory(memory, None);
                    self.device.destroy_image(image, None);
                }
                err
            })?;

        let mut view_conversion =
            vk::SamplerYcbcrConversionInfo::default().conversion(ycbcr_conversion);
        let view_info = vk::ImageViewCreateInfo::default()
            .push_next(&mut view_conversion)
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::UNDEFINED)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1),
            );
        let view = match unsafe { self.device.create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(e) => {
                unsafe {
                    self.device.free_memory(memory, None);
                    self.device.destroy_image(image, None);
                }
                return Err(format!(
                    "Android Vulkan camera import failed: create_image_view(external): {e:?}"
                ));
            }
        };

        unsafe {
            ndk_sys::AHardwareBuffer_acquire(hardware_buffer);
        }
        crate::log!(
            "RUSTY_XR_MAKEPAD_VULKAN_VIDEO_IMPORT schema=rusty.xr.makepad-vulkan-video-import.v1 path=external-ahardwarebuffer-ycbcr size={}x{} vkFormat={:?} externalFormat={} samplerYcbcrConversion=true resourceSampler=true resourceShape=image-view-plus-combined-immutable-sampler-ycbcr-conversion importImageLayout=shader-read-transition initialLayout=undefined descriptorImageLayout=shader-read-only-optimal suggestedYcbcrModel={:?} suggestedYcbcrRange={:?} effectiveYcbcrModel={:?} effectiveYcbcrRange={:?} ycbcrComponents={} suggestedXChromaOffset={:?} suggestedYChromaOffset={:?} conversionMode=forced-bt601-limited-cpuyuv-reference samplerBindingMode=combined-immutable-sampler samplerBindingCompliance=pure-hwb-reference-combined-immutable combinedImageSampler=true immutableSampler=true ycbcrSamplerCacheReused={} stableImmutableSampler=true shaderSampleLowering=textureSampleLevel_combined_image_sampler_same_binding colorFixAttempt=hwb-external-combined-immutable-v4-default-sampler-remap",
            width.max(1),
            height.max(1),
            vk_format,
            external_format,
            suggested_ycbcr_model,
            suggested_ycbcr_range,
            effective_ycbcr_model,
            effective_ycbcr_range,
            ycbcr_components,
            format_props.suggested_x_chroma_offset,
            format_props.suggested_y_chroma_offset,
            ycbcr_sampler_cache_reused,
        );

        Ok((
            VulkanTextureResource {
                image,
                memory,
                view,
                face_views: [vk::ImageView::null(); 6],
                width: width.max(1),
                height: height.max(1),
                layers: 1,
                is_cube: false,
                format: vk::Format::UNDEFINED,
                layout: vk::ImageLayout::UNDEFINED,
                hardware_buffer: Some(hardware_buffer),
                sampler: Some(sampler),
                ycbcr_conversion: Some(ycbcr_conversion),
                ycbcr_conversion_metadata: Some(ycbcr_conversion_metadata.clone()),
                owns_sampler_ycbcr_conversion: false,
                owns_image: true,
            },
            format!("{vk_format:?}"),
            Some(external_format),
            Some(ycbcr_conversion_metadata),
        ))
    }

    fn imported_yuv_plane_layout(vk_format: vk::Format) -> Option<ImportedYuvPlaneLayout> {
        match vk_format {
            vk::Format::G8_B8_R8_3PLANE_420_UNORM => Some(ImportedYuvPlaneLayout {
                biplanar: false,
                plane0_view_format: vk::Format::R8_UNORM,
                plane1_view_format: vk::Format::R8_UNORM,
                plane2_view_format: Some(vk::Format::R8_UNORM),
            }),
            vk::Format::G8_B8R8_2PLANE_420_UNORM => Some(ImportedYuvPlaneLayout {
                biplanar: true,
                plane0_view_format: vk::Format::R8_UNORM,
                plane1_view_format: vk::Format::R8G8_UNORM,
                plane2_view_format: None,
            }),
            _ => None,
        }
    }

    fn create_imported_hardware_buffer_plane_view(
        &self,
        image: vk::Image,
        view_format: vk::Format,
        aspect_mask: vk::ImageAspectFlags,
    ) -> Result<vk::ImageView, String> {
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(view_format)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(aspect_mask)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1),
            );
        unsafe { self.device.create_image_view(&view_info, None) }
            .map_err(|e| format!("Android Vulkan camera import failed: create_plane_view: {e:?}"))
    }

    pub fn update_video_yuv_hardware_buffer_textures(
        &mut self,
        tex_y_id: TextureId,
        tex_u_id: TextureId,
        tex_v_id: TextureId,
        hardware_buffer: *mut ndk_sys::AHardwareBuffer,
        width: u32,
        height: u32,
    ) -> Result<(VideoYuvMetadata, VideoTextureUpdateMetadata), String> {
        if hardware_buffer.is_null() {
            return Err("Android Vulkan camera import failed: null AHardwareBuffer".to_string());
        }
        let hardware_buffer_id = Self::hardware_buffer_cache_key(hardware_buffer);

        let tex_y_key = Self::texture_key(tex_y_id);
        let tex_u_key = Self::texture_key(tex_u_id);
        let tex_v_key = Self::texture_key(tex_v_id);

        let same_source = self
            .textures
            .get(&tex_y_key)
            .and_then(|resource| resource.hardware_buffer)
            == Some(hardware_buffer);
        if same_source {
            let biplanar = self
                .textures
                .get(&tex_u_key)
                .map(|resource| resource.format == vk::Format::R8G8_UNORM)
                .unwrap_or(false);
            let mut metadata = VideoTextureUpdateMetadata::default()
                .with_resource(
                    VideoTextureResourcePath::HardwareBufferYuvPlanes,
                    VideoTextureDescriptorShape::ImportedYuvPlaneTextures,
                    width,
                    height,
                )
                .with_resource_reused(true);
            if let Some(hardware_buffer_id) = hardware_buffer_id {
                metadata = metadata.with_hardware_buffer_id(hardware_buffer_id);
            }
            return Ok((
                VideoYuvMetadata {
                    enabled: true,
                    matrix: 1.0,
                    biplanar,
                    rotation_steps: 0.0,
                },
                metadata,
            ));
        }

        let (vk_format, external_format, allocation_size, android_memory_type_bits) = {
            let mut format_properties = vk::AndroidHardwareBufferFormatPropertiesANDROID::default();
            let (allocation_size, android_memory_type_bits) = {
                let mut properties = vk::AndroidHardwareBufferPropertiesANDROID::default()
                    .push_next(&mut format_properties);
                unsafe {
                    self.external_memory_android_hardware_buffer
                        .get_android_hardware_buffer_properties(
                            hardware_buffer.cast(),
                            &mut properties,
                        )
                        .map_err(|e| {
                            format!(
                                "Android Vulkan camera import failed: get_android_hardware_buffer_properties: {e:?}"
                            )
                        })?;
                }
                (properties.allocation_size, properties.memory_type_bits)
            };
            (
                format_properties.format,
                format_properties.external_format,
                allocation_size,
                android_memory_type_bits,
            )
        };

        let plane_layout = Self::imported_yuv_plane_layout(vk_format).ok_or_else(|| {
            if vk_format == vk::Format::UNDEFINED {
                format!(
                    "Android Vulkan camera import failed: YUV hardware buffer reported undefined Vulkan format (external_format={external_format})"
                )
            } else {
                format!(
                    "Android Vulkan camera import failed: unsupported YUV Vulkan format {vk_format:?}"
                )
            }
        })?;

        let mut external_memory = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::ANDROID_HARDWARE_BUFFER_ANDROID);
        let image_info = vk::ImageCreateInfo::default()
            .push_next(&mut external_memory)
            .flags(vk::ImageCreateFlags::MUTABLE_FORMAT)
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width: width.max(1),
                height: height.max(1),
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { self.device.create_image(&image_info, None) }.map_err(|e| {
            format!("Android Vulkan camera import failed: create_image(yuv): {e:?}")
        })?;
        let memory_req = unsafe { self.device.get_image_memory_requirements(image) };
        let compatible_memory_bits = memory_req.memory_type_bits & android_memory_type_bits;
        let memory_type_index = self
            .find_memory_type(
                compatible_memory_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .or_else(|_| {
                self.find_memory_type(compatible_memory_bits, vk::MemoryPropertyFlags::empty())
            })
            .map_err(|err| {
                unsafe {
                    self.device.destroy_image(image, None);
                }
                err
            })?;

        let mut import_info =
            vk::ImportAndroidHardwareBufferInfoANDROID::default().buffer(hardware_buffer.cast());
        let alloc_info = vk::MemoryAllocateInfo::default()
            .push_next(&mut import_info)
            .allocation_size(allocation_size.max(memory_req.size))
            .memory_type_index(memory_type_index);
        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            unsafe {
                self.device.destroy_image(image, None);
            }
            format!("Android Vulkan camera import failed: allocate_memory(yuv): {e:?}")
        })?;

        if let Err(e) = unsafe { self.device.bind_image_memory(image, memory, 0) } {
            unsafe {
                self.device.free_memory(memory, None);
                self.device.destroy_image(image, None);
            }
            return Err(format!(
                "Android Vulkan camera import failed: bind_image_memory(yuv): {e:?}"
            ));
        }

        let y_view = match self.create_imported_hardware_buffer_plane_view(
            image,
            plane_layout.plane0_view_format,
            vk::ImageAspectFlags::PLANE_0,
        ) {
            Ok(view) => view,
            Err(err) => {
                unsafe {
                    self.device.free_memory(memory, None);
                    self.device.destroy_image(image, None);
                }
                return Err(err);
            }
        };
        let u_view = match self.create_imported_hardware_buffer_plane_view(
            image,
            plane_layout.plane1_view_format,
            vk::ImageAspectFlags::PLANE_1,
        ) {
            Ok(view) => view,
            Err(err) => {
                unsafe {
                    self.device.destroy_image_view(y_view, None);
                    self.device.free_memory(memory, None);
                    self.device.destroy_image(image, None);
                }
                return Err(err);
            }
        };
        let v_view = match plane_layout.plane2_view_format {
            Some(view_format) => match self.create_imported_hardware_buffer_plane_view(
                image,
                view_format,
                vk::ImageAspectFlags::PLANE_2,
            ) {
                Ok(view) => view,
                Err(err) => {
                    unsafe {
                        self.device.destroy_image_view(u_view, None);
                        self.device.destroy_image_view(y_view, None);
                        self.device.free_memory(memory, None);
                        self.device.destroy_image(image, None);
                    }
                    return Err(err);
                }
            },
            None => match self.create_imported_hardware_buffer_plane_view(
                image,
                plane_layout.plane1_view_format,
                vk::ImageAspectFlags::PLANE_1,
            ) {
                Ok(view) => view,
                Err(err) => {
                    unsafe {
                        self.device.destroy_image_view(u_view, None);
                        self.device.destroy_image_view(y_view, None);
                        self.device.free_memory(memory, None);
                        self.device.destroy_image(image, None);
                    }
                    return Err(err);
                }
            },
        };

        unsafe {
            ndk_sys::AHardwareBuffer_acquire(hardware_buffer);
        }
        crate::warning!(
            "Android Vulkan camera import: YUV size={}x{} vk_format={:?} external_format={} biplanar={}",
            width.max(1),
            height.max(1),
            vk_format,
            external_format,
            plane_layout.biplanar,
        );

        let chroma_width = width.div_ceil(2).max(1);
        let chroma_height = height.div_ceil(2).max(1);
        let y_resource = VulkanTextureResource {
            image,
            memory,
            view: y_view,
            face_views: [vk::ImageView::null(); 6],
            width: width.max(1),
            height: height.max(1),
            layers: 1,
            is_cube: false,
            format: plane_layout.plane0_view_format,
            layout: vk::ImageLayout::GENERAL,
            hardware_buffer: Some(hardware_buffer),
            sampler: None,
            ycbcr_conversion: None,
            ycbcr_conversion_metadata: None,
            owns_sampler_ycbcr_conversion: false,
            owns_image: true,
        };
        let u_resource = VulkanTextureResource {
            image: vk::Image::null(),
            memory: vk::DeviceMemory::null(),
            view: u_view,
            face_views: [vk::ImageView::null(); 6],
            width: chroma_width,
            height: chroma_height,
            layers: 1,
            is_cube: false,
            format: plane_layout.plane1_view_format,
            layout: vk::ImageLayout::GENERAL,
            hardware_buffer: None,
            sampler: None,
            ycbcr_conversion: None,
            ycbcr_conversion_metadata: None,
            owns_sampler_ycbcr_conversion: false,
            owns_image: false,
        };
        let v_resource = VulkanTextureResource {
            image: vk::Image::null(),
            memory: vk::DeviceMemory::null(),
            view: v_view,
            face_views: [vk::ImageView::null(); 6],
            width: chroma_width,
            height: chroma_height,
            layers: 1,
            is_cube: false,
            format: plane_layout
                .plane2_view_format
                .unwrap_or(plane_layout.plane1_view_format),
            layout: vk::ImageLayout::GENERAL,
            hardware_buffer: None,
            sampler: None,
            ycbcr_conversion: None,
            ycbcr_conversion_metadata: None,
            owns_sampler_ycbcr_conversion: false,
            owns_image: false,
        };

        if let Some(old_resource) = self.textures.remove(&tex_v_key) {
            self.retire_texture_resource(old_resource);
        }
        if let Some(old_resource) = self.textures.remove(&tex_u_key) {
            self.retire_texture_resource(old_resource);
        }
        if let Some(old_resource) = self.textures.remove(&tex_y_key) {
            self.retire_texture_resource(old_resource);
        }
        self.textures.insert(tex_y_key, y_resource);
        self.textures.insert(tex_u_key, u_resource);
        self.textures.insert(tex_v_key, v_resource);

        let mut metadata = VideoTextureUpdateMetadata::default()
            .with_resource(
                VideoTextureResourcePath::HardwareBufferYuvPlanes,
                VideoTextureDescriptorShape::ImportedYuvPlaneTextures,
                width,
                height,
            )
            .with_vulkan_format(format!("{vk_format:?}"), Some(external_format))
            .with_resource_reused(false);
        if let Some(hardware_buffer_id) = hardware_buffer_id {
            metadata = metadata.with_hardware_buffer_id(hardware_buffer_id);
        }
        Ok((
            VideoYuvMetadata {
                enabled: true,
                matrix: 1.0,
                biplanar: plane_layout.biplanar,
                rotation_steps: 0.0,
            },
            metadata,
        ))
    }

    pub fn update_video_external_hardware_buffer_texture(
        &mut self,
        texture_id: TextureId,
        hardware_buffer: *mut ndk_sys::AHardwareBuffer,
        width: u32,
        height: u32,
    ) -> Result<(VideoYuvMetadata, VideoTextureUpdateMetadata), String> {
        let texture_key = Self::texture_key(texture_id);
        let hardware_buffer_key =
            Self::hardware_buffer_cache_key(hardware_buffer).ok_or_else(|| {
                "Android Vulkan camera import failed: null AHardwareBuffer".to_string()
            })?;
        let same_source = self
            .textures
            .get(&texture_key)
            .and_then(Self::texture_resource_hardware_buffer_cache_key)
            == Some(hardware_buffer_key);
        let mut metadata = VideoTextureUpdateMetadata::default()
            .with_resource(
                VideoTextureResourcePath::HardwareBufferExternal,
                VideoTextureDescriptorShape::CombinedImmutableSamplerYcbcrConversion,
                width,
                height,
            )
            .with_hardware_buffer_id(hardware_buffer_key)
            .with_resource_reused(same_source);
        if same_source {
            if let Some(ycbcr_conversion) = self
                .textures
                .get(&texture_key)
                .and_then(|resource| resource.ycbcr_conversion_metadata.clone())
            {
                metadata = metadata.with_ycbcr_conversion(ycbcr_conversion);
            }
        }
        if !same_source {
            if let Some(cached_resource) = self.take_cached_video_hardware_buffer_texture_resource(
                texture_key,
                hardware_buffer_key,
            ) {
                if let Some(old_resource) = self.textures.remove(&texture_key) {
                    self.cache_video_hardware_buffer_texture_resource(texture_key, old_resource);
                }
                if let Some(ycbcr_conversion) = cached_resource.ycbcr_conversion_metadata.clone() {
                    metadata = metadata.with_ycbcr_conversion(ycbcr_conversion);
                }
                metadata = metadata
                    .with_vulkan_format(format!("{:?}", cached_resource.format), None)
                    .with_resource_reused(true);
                self.textures.insert(texture_key, cached_resource);
            } else {
                if let Some(old_resource) = self.textures.remove(&texture_key) {
                    self.cache_video_hardware_buffer_texture_resource(texture_key, old_resource);
                }
                let (resource, vk_format, external_format, ycbcr_conversion) = self
                    .create_imported_external_hardware_buffer_texture_resource(
                        hardware_buffer,
                        width,
                        height,
                    )?;
                metadata = metadata.with_vulkan_format(vk_format, external_format);
                if let Some(ycbcr_conversion) = ycbcr_conversion {
                    metadata = metadata.with_ycbcr_conversion(ycbcr_conversion);
                }
                self.textures.insert(texture_key, resource);
            }
        }

        Ok((VideoYuvMetadata::disabled(), metadata))
    }

    pub fn update_video_rgba_hardware_buffer_texture(
        &mut self,
        texture_id: TextureId,
        hardware_buffer: *mut ndk_sys::AHardwareBuffer,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let texture_key = Self::texture_key(texture_id);
        let same_source = self
            .textures
            .get(&texture_key)
            .and_then(|resource| resource.hardware_buffer)
            == Some(hardware_buffer);
        if same_source {
            return Ok(());
        }

        if let Some(old_resource) = self.textures.remove(&texture_key) {
            self.retire_texture_resource(old_resource);
        }
        let resource =
            self.create_imported_hardware_buffer_texture_resource(hardware_buffer, width, height)?;
        self.textures.insert(texture_key, resource);
        Ok(())
    }
}
