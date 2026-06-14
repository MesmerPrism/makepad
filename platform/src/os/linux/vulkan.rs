#![cfg(target_os = "android")]

mod basic_compute_probe;
mod field_force_sample_probe;
mod field_sample_probe;
mod frame_resources;
mod mesh_sdf_probe;
mod openxr_targets;
mod pipeline_resources;
mod skinning_mesh_probe;
mod skinning_probe;
mod texture_lifetime;
mod texture_resources;
mod video_hardware_buffer;
mod volume_image_preview;
mod volume_probe;
mod volume_raymarch_preview;

use crate::{
    cx::Cx,
    draw_list::DrawListId,
    draw_pass::{DrawPassClearColor, DrawPassClearDepth, DrawPassId},
    event::video_playback::VideoTextureYcbcrConversionMetadata,
    geometry::GeometryId,
    makepad_live_id::*,
    makepad_script::shader::TextureType,
    os::linux::{
        android::ndk_sys,
        openxr_sys::{
            LibOpenXr, VkDeviceCreateInfo, VkInstanceCreateInfo, XrInstance, XrResult, XrSystemId,
            XrVulkanDeviceCreateInfoKHR, XrVulkanGraphicsDeviceGetInfoKHR,
            XrVulkanInstanceCreateInfoKHR,
        },
        vulkan_naga::{CxVulkanShaderBinary, CxVulkanShaderDescriptorKind},
    },
    texture::{TextureCategory, TextureFormat, TextureId, TexturePixel, TextureUpdated},
};
use ash::vk::{self, Handle};

use self::openxr_targets::VulkanXrInFlightFrame;
pub(crate) use self::openxr_targets::{
    CxVulkanOpenXrFoveationImageInfo, CxVulkanOpenXrSessionData, OpenXrVulkanRepaintStats,
};
use self::pipeline_resources::{
    VulkanPipeline, VulkanPipelineKey, VulkanVideoCombinedImmutableSamplerKey,
};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    ffi::CStr,
    os::raw::{c_char, c_void},
};

#[link(name = "nativewindow")]
extern "C" {
    fn ANativeWindow_acquire(window: *mut ndk_sys::ANativeWindow);
}

fn vulkan_descriptor_type_for_shader_kind(
    kind: CxVulkanShaderDescriptorKind,
) -> Option<vk::DescriptorType> {
    match kind {
        CxVulkanShaderDescriptorKind::UniformBuffer => Some(vk::DescriptorType::UNIFORM_BUFFER),
        CxVulkanShaderDescriptorKind::StorageBuffer => Some(vk::DescriptorType::STORAGE_BUFFER),
        CxVulkanShaderDescriptorKind::SampledImage
        | CxVulkanShaderDescriptorKind::DepthImage
        | CxVulkanShaderDescriptorKind::ExternalImage => Some(vk::DescriptorType::SAMPLED_IMAGE),
        CxVulkanShaderDescriptorKind::StorageImage => Some(vk::DescriptorType::STORAGE_IMAGE),
        CxVulkanShaderDescriptorKind::Sampler | CxVulkanShaderDescriptorKind::ComparisonSampler => {
            Some(vk::DescriptorType::SAMPLER)
        }
        CxVulkanShaderDescriptorKind::OtherHandle => None,
    }
}

fn vulkan_descriptor_type_name(descriptor_type: vk::DescriptorType) -> &'static str {
    match descriptor_type {
        vk::DescriptorType::SAMPLER => "SAMPLER",
        vk::DescriptorType::COMBINED_IMAGE_SAMPLER => "COMBINED_IMAGE_SAMPLER",
        vk::DescriptorType::SAMPLED_IMAGE => "SAMPLED_IMAGE",
        vk::DescriptorType::STORAGE_IMAGE => "STORAGE_IMAGE",
        vk::DescriptorType::UNIFORM_BUFFER => "UNIFORM_BUFFER",
        vk::DescriptorType::STORAGE_BUFFER => "STORAGE_BUFFER",
        _ => "OTHER",
    }
}

fn reflected_vulkan_descriptor_type(
    vk_shader: &CxVulkanShaderBinary,
    binding: u32,
    fallback: vk::DescriptorType,
) -> vk::DescriptorType {
    vk_shader
        .resource_interface
        .descriptor_kind(0, binding)
        .and_then(vulkan_descriptor_type_for_shader_kind)
        .unwrap_or(fallback)
}

fn reflected_shader_descriptor_kind_name(
    vk_shader: &CxVulkanShaderBinary,
    binding: u32,
) -> &'static str {
    vk_shader
        .resource_interface
        .descriptor_kind(0, binding)
        .map(CxVulkanShaderDescriptorKind::stable_name)
        .unwrap_or("missing")
}

unsafe extern "system" fn vulkan_debug_callback(
    message_severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    message_types: vk::DebugUtilsMessageTypeFlagsEXT,
    p_callback_data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _p_user_data: *mut c_void,
) -> vk::Bool32 {
    let msg = if p_callback_data.is_null() {
        "<null debug callback data>".into()
    } else {
        CStr::from_ptr((*p_callback_data).p_message)
            .to_string_lossy()
            .into_owned()
    };
    if message_severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        crate::error!("Vulkan validation [{message_types:?}] {msg}");
    } else if message_severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
        crate::warning!("Vulkan validation [{message_types:?}] {msg}");
    } else {
        crate::log!("Vulkan validation [{message_types:?}] {msg}");
    }
    vk::FALSE
}

fn vulkan_debug_messenger_create_info() -> vk::DebugUtilsMessengerCreateInfoEXT<'static> {
    vk::DebugUtilsMessengerCreateInfoEXT::default()
        .message_severity(
            vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                | vk::DebugUtilsMessageSeverityFlagsEXT::INFO,
        )
        .message_type(
            vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
        )
        .pfn_user_callback(Some(vulkan_debug_callback))
}

#[derive(Clone, Copy)]
struct VulkanBuffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    size: vk::DeviceSize,
}

#[derive(Clone, Copy)]
struct VulkanGeometryResource {
    vertex_buffer: VulkanBuffer,
    index_buffer: VulkanBuffer,
}

#[derive(Default)]
struct FrameResources {
    buffers: Vec<VulkanBuffer>,
    descriptor_pools: Vec<vk::DescriptorPool>,
    packet_buffer: Option<VulkanBuffer>,
    packet_buffer_used: vk::DeviceSize,
    texture_upload_buffer: Option<VulkanBuffer>,
    texture_upload_buffer_used: vk::DeviceSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct VulkanExternalYcbcrSamplerKey {
    external_format: u64,
    ycbcr_model: i32,
    ycbcr_range: i32,
    component_mapping: [i32; 4],
    x_chroma_offset: i32,
    y_chroma_offset: i32,
    chroma_filter: i32,
    force_explicit_reconstruction: bool,
}

struct VulkanExternalYcbcrSampler {
    sampler: vk::Sampler,
    conversion: vk::SamplerYcbcrConversion,
    metadata: VideoTextureYcbcrConversionMetadata,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct VulkanRenderPassKey {
    color_formats: Vec<i32>,
    depth_format: Option<i32>,
}

impl VulkanRenderPassKey {
    fn new(color_formats: &[vk::Format], depth_format: Option<vk::Format>) -> Self {
        Self {
            color_formats: color_formats.iter().map(|format| format.as_raw()).collect(),
            depth_format: depth_format.map(|format| format.as_raw()),
        }
    }

    fn color_vk_formats(&self) -> Vec<vk::Format> {
        self.color_formats
            .iter()
            .map(|format| vk::Format::from_raw(*format))
            .collect()
    }

    fn depth_vk_format(&self) -> Option<vk::Format> {
        self.depth_format.map(vk::Format::from_raw)
    }
}

struct VulkanDrawPacket {
    shader_index: usize,
    shader_variant: usize,
    geometry_id: GeometryId,
    depth_write: bool,
    alpha_blend: bool,
    backface_culling: bool,
    instances: Vec<f32>,
    draw_call_uniforms: Vec<f32>,
    dyn_uniforms: Vec<f32>,
    scope_uniforms: Vec<f32>,
    uniform_bindings: Vec<(LiveId, usize)>,
    dyn_uniform_binding: u32,
    scope_uniform_binding: Option<usize>,
    texture_ids: Vec<TextureId>,
    texture_types: Vec<TextureType>,
}

struct VulkanTextureResource {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    face_views: [vk::ImageView; 6],
    width: u32,
    height: u32,
    layers: u32,
    is_cube: bool,
    format: vk::Format,
    layout: vk::ImageLayout,
    hardware_buffer: Option<*mut ndk_sys::AHardwareBuffer>,
    sampler: Option<vk::Sampler>,
    ycbcr_conversion: Option<vk::SamplerYcbcrConversion>,
    ycbcr_conversion_metadata: Option<VideoTextureYcbcrConversionMetadata>,
    owns_sampler_ycbcr_conversion: bool,
    owns_image: bool,
}

struct RetiredTextureResource {
    retire_after_submit_serial: u64,
    resource: VulkanTextureResource,
}

struct VideoHardwareBufferTextureCacheEntry {
    texture_key: VulkanTextureKey,
    hardware_buffer_key: u64,
    resource: VulkanTextureResource,
    last_used_submit_serial: u64,
}

struct VulkanTextureUpload<'a> {
    data: Cow<'a, [u8]>,
    offset_x: u32,
    offset_y: u32,
    width: u32,
    height: u32,
    layers: u32,
}

type VulkanTextureKey = usize;

#[derive(Default)]
struct VulkanDrawStats {
    draw_items: usize,
    draw_calls: usize,
    packets_recorded: usize,
    instances: u64,
    indices: u64,
    skipped_non_draw_call: usize,
    skipped_no_os_shader: usize,
    skipped_no_vulkan_shader: usize,
    skipped_missing_spirv: usize,
    skipped_no_instance_slots: usize,
    skipped_no_instances_buffer: usize,
    skipped_instances_too_short: usize,
    skipped_zero_instances: usize,
    skipped_no_geometry_id: usize,
    skipped_empty_geometry: usize,
}

pub struct CxVulkan {
    instance: ash::Instance,
    surface_loader: ash::khr::surface::Instance,
    android_surface_loader: ash::khr::android_surface::Instance,
    surface: vk::SurfaceKHR,
    physical_device: vk::PhysicalDevice,
    queue_family_index: u32,
    min_uniform_buffer_offset_alignment: vk::DeviceSize,
    device: ash::Device,
    external_memory_android_hardware_buffer:
        ash::android::external_memory_android_hardware_buffer::Device,
    queue: vk::Queue,
    swapchain_loader: ash::khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    swapchain_images: Vec<vk::Image>,
    swapchain_image_views: Vec<vk::ImageView>,
    swapchain_depth_targets: Vec<VulkanTextureResource>,
    swapchain_readback_buffer: Option<VulkanBuffer>,
    swapchain_format: vk::Format,
    depth_format: vk::Format,
    swapchain_extent: vk::Extent2D,
    render_pass: vk::RenderPass,
    xr_render_pass: vk::RenderPass,
    framebuffers: Vec<vk::Framebuffer>,
    pipelines: HashMap<VulkanPipelineKey, VulkanPipeline>,
    offscreen_render_passes: HashMap<VulkanRenderPassKey, vk::RenderPass>,
    geometries: HashMap<GeometryId, VulkanGeometryResource>,
    textures: HashMap<VulkanTextureKey, VulkanTextureResource>,
    video_hardware_buffer_texture_cache: Vec<VideoHardwareBufferTextureCacheEntry>,
    video_hardware_buffer_texture_cache_hit_count: u64,
    video_hardware_buffer_texture_cache_miss_count: u64,
    video_hardware_buffer_texture_cache_evict_count: u64,
    retired_texture_resources: Vec<RetiredTextureResource>,
    external_ycbcr_samplers: HashMap<VulkanExternalYcbcrSamplerKey, VulkanExternalYcbcrSampler>,
    reported_video_descriptor_shapes: HashSet<(usize, usize, usize, usize)>,
    frame_resources: FrameResources,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    image_available_semaphore: vk::Semaphore,
    render_finished_semaphore: vk::Semaphore,
    in_flight_fence: vk::Fence,
    window_in_flight_submit_serial: u64,
    gpu_submit_serial: u64,
    gpu_completed_submit_serial: u64,
    window: *mut ndk_sys::ANativeWindow,
    requested_width: u32,
    requested_height: u32,
    texture_upload_count_this_frame: u32,
    texture_upload_bytes_this_frame: u64,
    xr_packet_buffer_count_this_frame: u32,
    xr_packet_buffer_bytes_this_frame: u64,
    xr_geometry_upload_bytes_this_frame: u64,
    xr_descriptor_set_count_this_frame: u32,
    debug_utils_enabled: bool,
    debug_utils_loader: Option<ash::ext::debug_utils::Instance>,
    debug_messenger: vk::DebugUtilsMessengerEXT,
    xr_multiview_enabled: bool,
    xr_fragment_density_map_enabled: bool,
    xr_render_pass_uses_fragment_density_map: bool,
    xr_depth_dummy: Option<VulkanTextureResource>,
    xr_depth_dummy_multiview: Option<VulkanTextureResource>,
    xr_timestamp_period_ns: f64,
    xr_gpu_timestamps_supported: bool,
    xr_last_gpu_frame_time_ms: Option<f64>,
    xr_in_flight_frames: Vec<VulkanXrInFlightFrame>,
    xr_in_flight_index: usize,
    xr_u32_compute_probe_resources: Vec<basic_compute_probe::VulkanXrU32ComputeProbeResources>,
    xr_f32_force_probe_resources: Vec<basic_compute_probe::VulkanXrF32ForceProbeResources>,
    xr_f32_skinning_probe_resources: Vec<skinning_probe::VulkanXrF32SkinningProbeResources>,
    xr_f32_skinning_mesh_probe_resources:
        Vec<skinning_mesh_probe::VulkanXrF32SkinningMeshProbeResources>,
    xr_f32_mesh_sdf_probe_program: Option<mesh_sdf_probe::VulkanXrF32MeshSdfProbeProgram>,
    xr_f32_mesh_sdf_probe_source_mesh_buffers:
        Option<mesh_sdf_probe::VulkanXrF32MeshSdfProbeSourceMeshBuffers>,
    xr_f32_mesh_sdf_probe_derived_buffers:
        Option<mesh_sdf_probe::VulkanXrF32MeshSdfProbeDerivedBuffers>,
    xr_f32_mesh_sdf_probe_resources: Vec<mesh_sdf_probe::VulkanXrF32MeshSdfProbeResources>,
    xr_f32_field_sample_probe_program:
        Option<field_sample_probe::VulkanXrF32FieldSampleProbeProgram>,
    xr_f32_field_sample_probe_resources:
        Vec<field_sample_probe::VulkanXrF32FieldSampleProbeResources>,
    xr_f32_field_force_sample_probe_program:
        Option<field_force_sample_probe::VulkanXrF32FieldForceSampleProbeProgram>,
    xr_f32_field_force_sample_probe_resources:
        Vec<field_force_sample_probe::VulkanXrF32FieldForceSampleProbeResources>,
    xr_f32_volume_probe_resources: Vec<volume_probe::VulkanXrF32VolumeProbeResources>,
    xr_f32_volume_image_preview_resources:
        Vec<volume_image_preview::VulkanXrF32VolumeImagePreviewResources>,
    xr_f32_volume_raymarch_preview_resources:
        Vec<volume_raymarch_preview::VulkanXrF32VolumeRaymarchPreviewResources>,
    xr_storage_buffer_probe_resources:
        Vec<basic_compute_probe::VulkanXrStorageBufferProbeResources>,
}

impl CxVulkan {
    pub(crate) fn has_drawable_surface(&self) -> bool {
        !self.window.is_null()
            && self.surface != vk::SurfaceKHR::null()
            && self.swapchain != vk::SwapchainKHR::null()
    }

    fn geometry_id_is_live(cx: &Cx, geometry_id: GeometryId) -> bool {
        let slot_index = geometry_id.slot_index();
        cx.geometries
            .0
            .pool
            .get(slot_index)
            .map(|item| item.generation == geometry_id.generation())
            .unwrap_or(false)
    }

    fn prune_stale_geometry_resources(&mut self, cx: &Cx) {
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

    pub fn new(
        window: *mut ndk_sys::ANativeWindow,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        if window.is_null() {
            return Err("Android Vulkan init failed: null ANativeWindow".to_string());
        }

        let entry = unsafe { ash::Entry::load() }
            .map_err(|e| format!("Android Vulkan init failed: Entry::load: {e:?}"))?;

        let available_layers = unsafe { entry.enumerate_instance_layer_properties() }
            .map_err(|e| format!("Android Vulkan init failed: enumerate layers: {e:?}"))?;
        let has_validation_layer = available_layers.iter().any(|layer| {
            let name = unsafe { CStr::from_ptr(layer.layer_name.as_ptr()) };
            name.to_bytes() == b"VK_LAYER_KHRONOS_validation"
        });

        let available_exts = unsafe { entry.enumerate_instance_extension_properties(None) }
            .map_err(|e| format!("Android Vulkan init failed: enumerate extensions: {e:?}"))?;
        let has_debug_utils_ext = available_exts.iter().any(|ext| {
            let name = unsafe { CStr::from_ptr(ext.extension_name.as_ptr()) };
            name.to_bytes() == vk::EXT_DEBUG_UTILS_NAME.to_bytes()
        });

        let mut instance_extensions = vec![
            vk::KHR_SURFACE_NAME.as_ptr(),
            vk::KHR_ANDROID_SURFACE_NAME.as_ptr(),
        ];
        if has_debug_utils_ext {
            instance_extensions.push(vk::EXT_DEBUG_UTILS_NAME.as_ptr());
        }
        let validation_layer_name = b"VK_LAYER_KHRONOS_validation\0";
        let enabled_layers: Vec<*const c_char> = if has_validation_layer {
            vec![validation_layer_name.as_ptr() as *const c_char]
        } else {
            Vec::new()
        };

        let app_info = vk::ApplicationInfo {
            api_version: vk::API_VERSION_1_1,
            ..Default::default()
        };
        let mut instance_create_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&instance_extensions)
            .enabled_layer_names(&enabled_layers);
        let mut debug_create_info = vulkan_debug_messenger_create_info();
        if has_debug_utils_ext {
            instance_create_info = instance_create_info.push_next(&mut debug_create_info);
        }

        let instance = unsafe { entry.create_instance(&instance_create_info, None) }
            .map_err(|e| format!("Android Vulkan init failed: create_instance: {e:?}"))?;

        let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);
        let android_surface_loader = ash::khr::android_surface::Instance::new(&entry, &instance);

        unsafe { ANativeWindow_acquire(window) };

        let create_surface_result = Self::create_surface(&android_surface_loader, window);
        let surface = match create_surface_result {
            Ok(surface) => surface,
            Err(err) => {
                unsafe { ndk_sys::ANativeWindow_release(window) };
                unsafe { instance.destroy_instance(None) };
                return Err(err);
            }
        };

        let pick_result = Self::pick_device_and_queue_family(&instance, &surface_loader, surface);
        let (physical_device, queue_family_index) = match pick_result {
            Ok(pick) => pick,
            Err(err) => {
                unsafe {
                    surface_loader.destroy_surface(surface, None);
                    ndk_sys::ANativeWindow_release(window);
                    instance.destroy_instance(None);
                }
                return Err(err);
            }
        };

        let props = unsafe { instance.get_physical_device_properties(physical_device) };
        let queue_family_props =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        let gpu_timestamps_supported = queue_family_props
            .get(queue_family_index as usize)
            .map(|props| props.timestamp_valid_bits > 0)
            .unwrap_or(false);
        let timestamp_period_ns = props.limits.timestamp_period as f64;
        let device_name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        if device_name.contains("SwiftShader") || props.vendor_id == 0x1AE0 {
            crate::warning!(
                "Android Vulkan: SwiftShader/software device detected; expect very low performance"
            );
        }
        let queue_priorities = [1.0f32];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family_index)
            .queue_priorities(&queue_priorities)];
        let device_extensions = [
            vk::KHR_SWAPCHAIN_NAME.as_ptr(),
            vk::ANDROID_EXTERNAL_MEMORY_ANDROID_HARDWARE_BUFFER_NAME.as_ptr(),
        ];
        let mut sampler_ycbcr_features =
            vk::PhysicalDeviceSamplerYcbcrConversionFeatures::default()
                .sampler_ycbcr_conversion(true);
        let device_create_info = vk::DeviceCreateInfo::default()
            .push_next(&mut sampler_ycbcr_features)
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&device_extensions);

        let device =
            match unsafe { instance.create_device(physical_device, &device_create_info, None) } {
                Ok(device) => device,
                Err(err) => {
                    unsafe {
                        surface_loader.destroy_surface(surface, None);
                        ndk_sys::ANativeWindow_release(window);
                        instance.destroy_instance(None);
                    }
                    return Err(format!(
                        "Android Vulkan init failed: create_device: {err:?}"
                    ));
                }
            };

        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };
        let external_memory_android_hardware_buffer =
            ash::android::external_memory_android_hardware_buffer::Device::new(&instance, &device);
        let swapchain_loader = ash::khr::swapchain::Device::new(&instance, &device);

        let command_pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let command_pool = match unsafe { device.create_command_pool(&command_pool_info, None) } {
            Ok(pool) => pool,
            Err(err) => {
                unsafe {
                    device.destroy_device(None);
                    surface_loader.destroy_surface(surface, None);
                    ndk_sys::ANativeWindow_release(window);
                    instance.destroy_instance(None);
                }
                return Err(format!(
                    "Android Vulkan init failed: create_command_pool: {err:?}"
                ));
            }
        };

        let command_buffer_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command_buffer = match unsafe { device.allocate_command_buffers(&command_buffer_info) }
        {
            Ok(cmds) => cmds[0],
            Err(err) => {
                unsafe {
                    device.destroy_command_pool(command_pool, None);
                    device.destroy_device(None);
                    surface_loader.destroy_surface(surface, None);
                    ndk_sys::ANativeWindow_release(window);
                    instance.destroy_instance(None);
                }
                return Err(format!(
                    "Android Vulkan init failed: allocate_command_buffers: {err:?}"
                ));
            }
        };

        let semaphore_info = vk::SemaphoreCreateInfo::default();
        let image_available_semaphore =
            match unsafe { device.create_semaphore(&semaphore_info, None) } {
                Ok(semaphore) => semaphore,
                Err(err) => {
                    unsafe {
                        device.free_command_buffers(command_pool, &[command_buffer]);
                        device.destroy_command_pool(command_pool, None);
                        device.destroy_device(None);
                        surface_loader.destroy_surface(surface, None);
                        ndk_sys::ANativeWindow_release(window);
                        instance.destroy_instance(None);
                    }
                    return Err(format!(
                        "Android Vulkan init failed: create image semaphore: {err:?}"
                    ));
                }
            };

        let render_finished_semaphore =
            match unsafe { device.create_semaphore(&semaphore_info, None) } {
                Ok(semaphore) => semaphore,
                Err(err) => {
                    unsafe {
                        device.destroy_semaphore(image_available_semaphore, None);
                        device.free_command_buffers(command_pool, &[command_buffer]);
                        device.destroy_command_pool(command_pool, None);
                        device.destroy_device(None);
                        surface_loader.destroy_surface(surface, None);
                        ndk_sys::ANativeWindow_release(window);
                        instance.destroy_instance(None);
                    }
                    return Err(format!(
                        "Android Vulkan init failed: create render semaphore: {err:?}"
                    ));
                }
            };

        let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        let in_flight_fence = match unsafe { device.create_fence(&fence_info, None) } {
            Ok(fence) => fence,
            Err(err) => {
                unsafe {
                    device.destroy_semaphore(render_finished_semaphore, None);
                    device.destroy_semaphore(image_available_semaphore, None);
                    device.free_command_buffers(command_pool, &[command_buffer]);
                    device.destroy_command_pool(command_pool, None);
                    device.destroy_device(None);
                    surface_loader.destroy_surface(surface, None);
                    ndk_sys::ANativeWindow_release(window);
                    instance.destroy_instance(None);
                }
                return Err(format!("Android Vulkan init failed: create_fence: {err:?}"));
            }
        };

        let mut vulkan = Self {
            instance,
            surface_loader,
            android_surface_loader,
            surface,
            physical_device,
            queue_family_index,
            min_uniform_buffer_offset_alignment: props
                .limits
                .min_uniform_buffer_offset_alignment
                .max(4),
            device,
            external_memory_android_hardware_buffer,
            queue,
            swapchain_loader,
            swapchain: vk::SwapchainKHR::null(),
            swapchain_images: Vec::new(),
            swapchain_image_views: Vec::new(),
            swapchain_depth_targets: Vec::new(),
            swapchain_readback_buffer: None,
            swapchain_format: vk::Format::UNDEFINED,
            depth_format: vk::Format::UNDEFINED,
            swapchain_extent: vk::Extent2D {
                width: 0,
                height: 0,
            },
            render_pass: vk::RenderPass::null(),
            xr_render_pass: vk::RenderPass::null(),
            framebuffers: Vec::new(),
            pipelines: HashMap::new(),
            offscreen_render_passes: HashMap::new(),
            geometries: HashMap::new(),
            textures: HashMap::new(),
            video_hardware_buffer_texture_cache: Vec::new(),
            video_hardware_buffer_texture_cache_hit_count: 0,
            video_hardware_buffer_texture_cache_miss_count: 0,
            video_hardware_buffer_texture_cache_evict_count: 0,
            retired_texture_resources: Vec::new(),
            external_ycbcr_samplers: HashMap::new(),
            reported_video_descriptor_shapes: HashSet::new(),
            frame_resources: FrameResources::default(),
            command_pool,
            command_buffer,
            image_available_semaphore,
            render_finished_semaphore,
            in_flight_fence,
            window_in_flight_submit_serial: 0,
            gpu_submit_serial: 0,
            gpu_completed_submit_serial: 0,
            window,
            requested_width: width.max(1),
            requested_height: height.max(1),
            texture_upload_count_this_frame: 0,
            texture_upload_bytes_this_frame: 0,
            xr_packet_buffer_count_this_frame: 0,
            xr_packet_buffer_bytes_this_frame: 0,
            xr_geometry_upload_bytes_this_frame: 0,
            xr_descriptor_set_count_this_frame: 0,
            debug_utils_enabled: has_debug_utils_ext,
            debug_utils_loader: None,
            debug_messenger: vk::DebugUtilsMessengerEXT::null(),
            xr_multiview_enabled: false,
            xr_fragment_density_map_enabled: false,
            xr_render_pass_uses_fragment_density_map: false,
            xr_depth_dummy: None,
            xr_depth_dummy_multiview: None,
            xr_timestamp_period_ns: timestamp_period_ns,
            xr_gpu_timestamps_supported: gpu_timestamps_supported,
            xr_last_gpu_frame_time_ms: None,
            xr_in_flight_frames: Vec::new(),
            xr_in_flight_index: 0,
            xr_u32_compute_probe_resources: Vec::new(),
            xr_f32_force_probe_resources: Vec::new(),
            xr_f32_skinning_probe_resources: Vec::new(),
            xr_f32_skinning_mesh_probe_resources: Vec::new(),
            xr_f32_mesh_sdf_probe_program: None,
            xr_f32_mesh_sdf_probe_source_mesh_buffers: None,
            xr_f32_mesh_sdf_probe_derived_buffers: None,
            xr_f32_mesh_sdf_probe_resources: Vec::new(),
            xr_f32_field_sample_probe_program: None,
            xr_f32_field_sample_probe_resources: Vec::new(),
            xr_f32_field_force_sample_probe_program: None,
            xr_f32_field_force_sample_probe_resources: Vec::new(),
            xr_f32_volume_probe_resources: Vec::new(),
            xr_f32_volume_image_preview_resources: Vec::new(),
            xr_f32_volume_raymarch_preview_resources: Vec::new(),
            xr_storage_buffer_probe_resources: Vec::new(),
        };

        if let Err(err) = vulkan.recreate_swapchain() {
            return Err(format!(
                "Android Vulkan init failed: recreate_swapchain: {err}"
            ));
        }

        vulkan.try_enable_debug_messenger(&entry);

        Ok(vulkan)
    }

    pub fn new_from_openxr(
        xr: &LibOpenXr,
        xr_instance: XrInstance,
        xr_system_id: XrSystemId,
        window: *mut ndk_sys::ANativeWindow,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        if window.is_null() {
            return Err("Android Vulkan XR init failed: null ANativeWindow".to_string());
        }

        let entry = unsafe { ash::Entry::load() }
            .map_err(|e| format!("Android Vulkan XR init failed: Entry::load: {e:?}"))?;

        let available_layers = unsafe { entry.enumerate_instance_layer_properties() }
            .map_err(|e| format!("Android Vulkan XR init failed: enumerate layers: {e:?}"))?;
        let has_validation_layer = available_layers.iter().any(|layer| {
            let name = unsafe { CStr::from_ptr(layer.layer_name.as_ptr()) };
            name.to_bytes() == b"VK_LAYER_KHRONOS_validation"
        });

        let available_exts = unsafe { entry.enumerate_instance_extension_properties(None) }
            .map_err(|e| format!("Android Vulkan XR init failed: enumerate extensions: {e:?}"))?;
        let has_debug_utils_ext = available_exts.iter().any(|ext| {
            let name = unsafe { CStr::from_ptr(ext.extension_name.as_ptr()) };
            name.to_bytes() == vk::EXT_DEBUG_UTILS_NAME.to_bytes()
        });

        let mut instance_extensions = vec![
            vk::KHR_SURFACE_NAME.as_ptr(),
            vk::KHR_ANDROID_SURFACE_NAME.as_ptr(),
        ];
        if has_debug_utils_ext {
            instance_extensions.push(vk::EXT_DEBUG_UTILS_NAME.as_ptr());
        }
        let validation_layer_name = b"VK_LAYER_KHRONOS_validation\0";
        let enabled_layers: Vec<*const c_char> = if has_validation_layer {
            vec![validation_layer_name.as_ptr() as *const c_char]
        } else {
            Vec::new()
        };

        let app_info = vk::ApplicationInfo {
            api_version: vk::API_VERSION_1_1,
            ..Default::default()
        };
        let mut instance_create_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&instance_extensions)
            .enabled_layer_names(&enabled_layers);
        let mut debug_create_info = vulkan_debug_messenger_create_info();
        if has_debug_utils_ext {
            instance_create_info = instance_create_info.push_next(&mut debug_create_info);
        }

        let mut xr_vk_instance = std::ptr::null();
        let mut xr_vk_instance_result = 0;
        let xr_instance_create_info = XrVulkanInstanceCreateInfoKHR {
            system_id: xr_system_id,
            pfn_get_instance_proc_addr: Some(unsafe {
                std::mem::transmute(entry.static_fn().get_instance_proc_addr)
            }),
            vulkan_create_info: &instance_create_info as *const _ as *const VkInstanceCreateInfo,
            ..Default::default()
        };
        unsafe {
            (xr.xrCreateVulkanInstanceKHR)(
                xr_instance,
                &xr_instance_create_info,
                &mut xr_vk_instance,
                &mut xr_vk_instance_result,
            )
        }
        .to_result("xrCreateVulkanInstanceKHR")?;
        let xr_vk_instance_result = vk::Result::from_raw(xr_vk_instance_result);
        if xr_vk_instance_result != vk::Result::SUCCESS {
            return Err(format!(
                "Android Vulkan XR init failed: xrCreateVulkanInstanceKHR returned Vulkan error {xr_vk_instance_result:?}"
            ));
        }
        let instance = unsafe {
            ash::Instance::load(
                entry.static_fn(),
                vk::Instance::from_raw(xr_vk_instance as _),
            )
        };
        let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);
        let android_surface_loader = ash::khr::android_surface::Instance::new(&entry, &instance);

        unsafe { ANativeWindow_acquire(window) };
        let surface = vk::SurfaceKHR::null();

        let get_info = XrVulkanGraphicsDeviceGetInfoKHR {
            system_id: xr_system_id,
            vulkan_instance: xr_vk_instance,
            ..Default::default()
        };
        let mut runtime_physical_device = std::ptr::null();
        let xr_get_device_result = unsafe {
            (xr.xrGetVulkanGraphicsDevice2KHR)(xr_instance, &get_info, &mut runtime_physical_device)
        };
        if xr_get_device_result != XrResult::SUCCESS {
            unsafe {
                surface_loader.destroy_surface(surface, None);
                ndk_sys::ANativeWindow_release(window);
                instance.destroy_instance(None);
            }
            return Err(format!(
                "OpenXR error in xrGetVulkanGraphicsDevice2KHR: {}",
                xr_get_device_result
            ));
        }
        let physical_device = vk::PhysicalDevice::from_raw(runtime_physical_device as _);

        let queue_family_index =
            match Self::pick_graphics_queue_family_for_device(&instance, physical_device) {
                Ok(index) => index,
                Err(err) => {
                    unsafe {
                        surface_loader.destroy_surface(surface, None);
                        ndk_sys::ANativeWindow_release(window);
                        instance.destroy_instance(None);
                    }
                    return Err(err);
                }
            };

        let props = unsafe { instance.get_physical_device_properties(physical_device) };
        let queue_family_props =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        let gpu_timestamps_supported = queue_family_props
            .get(queue_family_index as usize)
            .map(|props| props.timestamp_valid_bits > 0)
            .unwrap_or(false);
        let timestamp_period_ns = props.limits.timestamp_period as f64;
        let device_name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        if device_name.contains("SwiftShader") || props.vendor_id == 0x1AE0 {
            crate::warning!(
                "Android Vulkan: SwiftShader/software device detected; expect very low performance"
            );
        }
        let xr_multiview_enabled = Self::query_multiview_support(&instance, physical_device);
        if !xr_multiview_enabled {
            unsafe {
                surface_loader.destroy_surface(surface, None);
                ndk_sys::ANativeWindow_release(window);
                instance.destroy_instance(None);
            }
            return Err(
                "Android Vulkan XR init failed: the OpenXR Vulkan backend requires multiview support"
                    .to_string(),
            );
        }
        let xr_fragment_density_map_enabled =
            Self::query_fragment_density_map_support(&instance, physical_device);

        let queue_priorities = [1.0f32];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family_index)
            .queue_priorities(&queue_priorities)];
        let mut device_extensions = vec![
            vk::KHR_SWAPCHAIN_NAME.as_ptr(),
            vk::ANDROID_EXTERNAL_MEMORY_ANDROID_HARDWARE_BUFFER_NAME.as_ptr(),
        ];
        if xr_fragment_density_map_enabled {
            device_extensions.push(vk::EXT_FRAGMENT_DENSITY_MAP_NAME.as_ptr());
        }
        let mut multiview_features =
            vk::PhysicalDeviceMultiviewFeatures::default().multiview(xr_multiview_enabled);
        let mut sampler_ycbcr_features =
            vk::PhysicalDeviceSamplerYcbcrConversionFeatures::default()
                .sampler_ycbcr_conversion(true);
        let mut fragment_density_map_features =
            vk::PhysicalDeviceFragmentDensityMapFeaturesEXT::default()
                .fragment_density_map(xr_fragment_density_map_enabled);
        let mut device_create_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&device_extensions);
        device_create_info = device_create_info.push_next(&mut sampler_ycbcr_features);
        device_create_info = device_create_info.push_next(&mut multiview_features);
        if xr_fragment_density_map_enabled {
            device_create_info = device_create_info.push_next(&mut fragment_density_map_features);
        }

        let mut xr_vk_device = std::ptr::null();
        let mut xr_vk_device_result = 0;
        let xr_device_create_info = XrVulkanDeviceCreateInfoKHR {
            system_id: xr_system_id,
            pfn_get_instance_proc_addr: Some(unsafe {
                std::mem::transmute(entry.static_fn().get_instance_proc_addr)
            }),
            vulkan_physical_device: runtime_physical_device,
            vulkan_create_info: &device_create_info as *const _ as *const VkDeviceCreateInfo,
            ..Default::default()
        };
        unsafe {
            (xr.xrCreateVulkanDeviceKHR)(
                xr_instance,
                &xr_device_create_info,
                &mut xr_vk_device,
                &mut xr_vk_device_result,
            )
        }
        .to_result("xrCreateVulkanDeviceKHR")?;
        let xr_vk_device_result = vk::Result::from_raw(xr_vk_device_result);
        if xr_vk_device_result != vk::Result::SUCCESS {
            unsafe {
                surface_loader.destroy_surface(surface, None);
                ndk_sys::ANativeWindow_release(window);
                instance.destroy_instance(None);
            }
            return Err(format!(
                "Android Vulkan XR init failed: xrCreateVulkanDeviceKHR returned Vulkan error {xr_vk_device_result:?}"
            ));
        }
        let device = unsafe {
            ash::Device::load(instance.fp_v1_0(), vk::Device::from_raw(xr_vk_device as _))
        };
        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };
        let external_memory_android_hardware_buffer =
            ash::android::external_memory_android_hardware_buffer::Device::new(&instance, &device);
        let swapchain_loader = ash::khr::swapchain::Device::new(&instance, &device);

        let command_pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let command_pool = match unsafe { device.create_command_pool(&command_pool_info, None) } {
            Ok(pool) => pool,
            Err(err) => {
                unsafe {
                    device.destroy_device(None);
                    surface_loader.destroy_surface(surface, None);
                    ndk_sys::ANativeWindow_release(window);
                    instance.destroy_instance(None);
                }
                return Err(format!(
                    "Android Vulkan XR init failed: create_command_pool: {err:?}"
                ));
            }
        };

        let command_buffer_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command_buffer = match unsafe { device.allocate_command_buffers(&command_buffer_info) }
        {
            Ok(cmds) => cmds[0],
            Err(err) => {
                unsafe {
                    device.destroy_command_pool(command_pool, None);
                    device.destroy_device(None);
                    surface_loader.destroy_surface(surface, None);
                    ndk_sys::ANativeWindow_release(window);
                    instance.destroy_instance(None);
                }
                return Err(format!(
                    "Android Vulkan XR init failed: allocate_command_buffers: {err:?}"
                ));
            }
        };

        let semaphore_info = vk::SemaphoreCreateInfo::default();
        let image_available_semaphore =
            match unsafe { device.create_semaphore(&semaphore_info, None) } {
                Ok(semaphore) => semaphore,
                Err(err) => {
                    unsafe {
                        device.free_command_buffers(command_pool, &[command_buffer]);
                        device.destroy_command_pool(command_pool, None);
                        device.destroy_device(None);
                        surface_loader.destroy_surface(surface, None);
                        ndk_sys::ANativeWindow_release(window);
                        instance.destroy_instance(None);
                    }
                    return Err(format!(
                        "Android Vulkan XR init failed: create image semaphore: {err:?}"
                    ));
                }
            };

        let render_finished_semaphore =
            match unsafe { device.create_semaphore(&semaphore_info, None) } {
                Ok(semaphore) => semaphore,
                Err(err) => {
                    unsafe {
                        device.destroy_semaphore(image_available_semaphore, None);
                        device.free_command_buffers(command_pool, &[command_buffer]);
                        device.destroy_command_pool(command_pool, None);
                        device.destroy_device(None);
                        surface_loader.destroy_surface(surface, None);
                        ndk_sys::ANativeWindow_release(window);
                        instance.destroy_instance(None);
                    }
                    return Err(format!(
                        "Android Vulkan XR init failed: create render semaphore: {err:?}"
                    ));
                }
            };

        let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        let in_flight_fence = match unsafe { device.create_fence(&fence_info, None) } {
            Ok(fence) => fence,
            Err(err) => {
                unsafe {
                    device.destroy_semaphore(render_finished_semaphore, None);
                    device.destroy_semaphore(image_available_semaphore, None);
                    device.free_command_buffers(command_pool, &[command_buffer]);
                    device.destroy_command_pool(command_pool, None);
                    device.destroy_device(None);
                    surface_loader.destroy_surface(surface, None);
                    ndk_sys::ANativeWindow_release(window);
                    instance.destroy_instance(None);
                }
                return Err(format!(
                    "Android Vulkan XR init failed: create_fence: {err:?}"
                ));
            }
        };

        let mut vulkan = Self {
            instance,
            surface_loader,
            android_surface_loader,
            surface,
            physical_device,
            queue_family_index,
            min_uniform_buffer_offset_alignment: props
                .limits
                .min_uniform_buffer_offset_alignment
                .max(4),
            device,
            external_memory_android_hardware_buffer,
            queue,
            swapchain_loader,
            swapchain: vk::SwapchainKHR::null(),
            swapchain_images: Vec::new(),
            swapchain_image_views: Vec::new(),
            swapchain_depth_targets: Vec::new(),
            swapchain_readback_buffer: None,
            swapchain_format: vk::Format::UNDEFINED,
            depth_format: vk::Format::UNDEFINED,
            swapchain_extent: vk::Extent2D {
                width: 0,
                height: 0,
            },
            render_pass: vk::RenderPass::null(),
            xr_render_pass: vk::RenderPass::null(),
            framebuffers: Vec::new(),
            pipelines: HashMap::new(),
            offscreen_render_passes: HashMap::new(),
            geometries: HashMap::new(),
            textures: HashMap::new(),
            video_hardware_buffer_texture_cache: Vec::new(),
            video_hardware_buffer_texture_cache_hit_count: 0,
            video_hardware_buffer_texture_cache_miss_count: 0,
            video_hardware_buffer_texture_cache_evict_count: 0,
            retired_texture_resources: Vec::new(),
            external_ycbcr_samplers: HashMap::new(),
            reported_video_descriptor_shapes: HashSet::new(),
            frame_resources: FrameResources::default(),
            command_pool,
            command_buffer,
            image_available_semaphore,
            render_finished_semaphore,
            in_flight_fence,
            window_in_flight_submit_serial: 0,
            gpu_submit_serial: 0,
            gpu_completed_submit_serial: 0,
            window,
            requested_width: width.max(1),
            requested_height: height.max(1),
            texture_upload_count_this_frame: 0,
            texture_upload_bytes_this_frame: 0,
            xr_packet_buffer_count_this_frame: 0,
            xr_packet_buffer_bytes_this_frame: 0,
            xr_geometry_upload_bytes_this_frame: 0,
            xr_descriptor_set_count_this_frame: 0,
            debug_utils_enabled: has_debug_utils_ext,
            debug_utils_loader: None,
            debug_messenger: vk::DebugUtilsMessengerEXT::null(),
            xr_multiview_enabled,
            xr_fragment_density_map_enabled,
            xr_render_pass_uses_fragment_density_map: false,
            xr_depth_dummy: None,
            xr_depth_dummy_multiview: None,
            xr_timestamp_period_ns: timestamp_period_ns,
            xr_gpu_timestamps_supported: gpu_timestamps_supported,
            xr_last_gpu_frame_time_ms: None,
            xr_in_flight_frames: Vec::new(),
            xr_in_flight_index: 0,
            xr_u32_compute_probe_resources: Vec::new(),
            xr_f32_force_probe_resources: Vec::new(),
            xr_f32_skinning_probe_resources: Vec::new(),
            xr_f32_skinning_mesh_probe_resources: Vec::new(),
            xr_f32_mesh_sdf_probe_program: None,
            xr_f32_mesh_sdf_probe_source_mesh_buffers: None,
            xr_f32_mesh_sdf_probe_derived_buffers: None,
            xr_f32_mesh_sdf_probe_resources: Vec::new(),
            xr_f32_field_sample_probe_program: None,
            xr_f32_field_sample_probe_resources: Vec::new(),
            xr_f32_field_force_sample_probe_program: None,
            xr_f32_field_force_sample_probe_resources: Vec::new(),
            xr_f32_volume_probe_resources: Vec::new(),
            xr_f32_volume_image_preview_resources: Vec::new(),
            xr_f32_volume_raymarch_preview_resources: Vec::new(),
            xr_storage_buffer_probe_resources: Vec::new(),
        };

        vulkan.xr_in_flight_frames =
            vulkan.create_xr_in_flight_frames(openxr_targets::XR_MAX_FRAMES_IN_FLIGHT)?;

        vulkan.try_enable_debug_messenger(&entry);

        Ok(vulkan)
    }

    fn try_enable_debug_messenger(&mut self, entry: &ash::Entry) {
        if !self.debug_utils_enabled {
            return;
        }
        let debug_loader = ash::ext::debug_utils::Instance::new(entry, &self.instance);
        let create_info = vulkan_debug_messenger_create_info();
        match unsafe { debug_loader.create_debug_utils_messenger(&create_info, None) } {
            Ok(messenger) => {
                self.debug_utils_loader = Some(debug_loader);
                self.debug_messenger = messenger;
            }
            Err(err) => {
                crate::warning!("Android Vulkan: failed to create debug messenger: {err:?}");
            }
        }
    }

    pub fn update_surface(
        &mut self,
        window: *mut ndk_sys::ANativeWindow,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        if window.is_null() {
            return Err("Android Vulkan surface update failed: null ANativeWindow".to_string());
        }

        self.requested_width = width.max(1);
        self.requested_height = height.max(1);

        if self.window != window || self.surface == vk::SurfaceKHR::null() {
            unsafe { ANativeWindow_acquire(window) };

            self.device_wait_idle();
            self.destroy_swapchain();
            self.destroy_surface();

            unsafe { ndk_sys::ANativeWindow_release(self.window) };
            self.window = window;

            self.surface = Self::create_surface(&self.android_surface_loader, window)?;
        }

        self.recreate_swapchain()
    }

    pub fn suspend_surface(&mut self) {
        self.device_wait_idle();
        self.destroy_swapchain();
        self.destroy_surface();

        if !self.window.is_null() {
            unsafe { ndk_sys::ANativeWindow_release(self.window) };
            self.window = std::ptr::null_mut();
        }
    }

    pub(crate) fn swapchain_format(&self) -> vk::Format {
        self.swapchain_format
    }

    pub(crate) fn instance_handle(&self) -> vk::Instance {
        self.instance.handle()
    }

    pub(crate) fn physical_device_handle(&self) -> vk::PhysicalDevice {
        self.physical_device
    }

    pub(crate) fn device_handle(&self) -> vk::Device {
        self.device.handle()
    }

    pub(crate) fn queue_family_index(&self) -> u32 {
        self.queue_family_index
    }

    fn swapchain_readback_supported(&self) -> bool {
        matches!(
            self.swapchain_format,
            vk::Format::B8G8R8A8_UNORM
                | vk::Format::B8G8R8A8_SRGB
                | vk::Format::R8G8B8A8_UNORM
                | vk::Format::R8G8B8A8_SRGB
        )
    }

    fn read_swapchain_color_image_rgba(&mut self, image_index: usize) -> Result<Vec<u8>, String> {
        let _image = *self
            .swapchain_images
            .get(image_index)
            .ok_or_else(|| format!("invalid swapchain image index {image_index}"))?;
        let staging = self
            .swapchain_readback_buffer
            .ok_or_else(|| "swapchain color readback buffer unavailable".to_string())?;
        let width = self.swapchain_extent.width;
        let height = self.swapchain_extent.height;
        if width == 0 || height == 0 {
            return Err("swapchain color readback dimensions are zero".to_string());
        }

        let byte_len = width as vk::DeviceSize * height as vk::DeviceSize * 4;
        let mut rgba = unsafe {
            let mapped = self
                .device
                .map_memory(staging.memory, 0, byte_len, vk::MemoryMapFlags::empty())
                .map_err(|e| format!("map_memory(swapchain color readback) failed: {e:?}"))?;
            let bytes = std::slice::from_raw_parts(mapped as *const u8, byte_len as usize).to_vec();
            self.device.unmap_memory(staging.memory);
            bytes
        };

        match self.swapchain_format {
            vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB => {
                for px in rgba.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
            }
            vk::Format::R8G8B8A8_UNORM | vk::Format::R8G8B8A8_SRGB => {}
            other => {
                return Err(format!(
                    "swapchain color readback does not support format {:?}",
                    other
                ));
            }
        }

        Ok(rgba)
    }

    pub fn draw_pass_and_present(
        &mut self,
        cx: &mut Cx,
        draw_pass_id: DrawPassId,
    ) -> Result<bool, String> {
        if self.surface == vk::SurfaceKHR::null() || self.swapchain == vk::SwapchainKHR::null() {
            return Ok(false);
        }

        let draw_list_id = if let Some(id) = cx.passes[draw_pass_id].main_draw_list_id {
            id
        } else {
            return Ok(false);
        };

        let dpi_factor = cx.passes[draw_pass_id].dpi_factor.unwrap_or(1.0);
        let pass_rect = match cx.get_pass_rect(draw_pass_id, dpi_factor) {
            Some(rect) => rect,
            None => return Ok(false),
        };
        if pass_rect.size.x < 0.5 || pass_rect.size.y < 0.5 {
            return Ok(false);
        }

        {
            let pass = &mut cx.passes[draw_pass_id];
            pass.paint_dirty = false;
            if !pass.keep_camera_matrix {
                pass.set_ortho_matrix(pass_rect.pos, pass_rect.size);
            }
            pass.set_dpi_factor(dpi_factor);
        }

        let clear_color = if cx.passes[draw_pass_id].color_textures.is_empty() {
            cx.passes[draw_pass_id].clear_color
        } else {
            match cx.passes[draw_pass_id].color_textures[0].clear_color {
                DrawPassClearColor::InitWith(color) => color,
                DrawPassClearColor::ClearWith(color) => color,
            }
        };

        unsafe {
            self.device
                .wait_for_fences(&[self.in_flight_fence], true, u64::MAX)
                .map_err(|e| format!("wait_for_fences failed: {e:?}"))?;
            self.gpu_completed_submit_serial = self
                .gpu_completed_submit_serial
                .max(self.window_in_flight_submit_serial);
            self.window_in_flight_submit_serial = 0;
            self.collect_retired_texture_resources();
            self.device
                .reset_fences(&[self.in_flight_fence])
                .map_err(|e| format!("reset_fences failed: {e:?}"))?;
        }

        self.destroy_frame_resources();

        let (image_index, acquire_suboptimal) = match unsafe {
            self.swapchain_loader.acquire_next_image(
                self.swapchain,
                u64::MAX,
                self.image_available_semaphore,
                vk::Fence::null(),
            )
        } {
            Ok(v) => v,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.recreate_swapchain()?;
                return Ok(false);
            }
            Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                self.suspend_surface();
                return Ok(false);
            }
            Err(err) => {
                return Err(format!("acquire_next_image failed: {err:?}"));
            }
        };
        if self.swapchain_images.get(image_index as usize).is_none() {
            return Err(format!("invalid swapchain image index {image_index}"));
        }
        let screenshot_request_ids = cx.take_studio_screenshot_request_ids(0);
        let run_view_request = cx.take_studio_run_view_frame_request(0);
        let capture_swapchain = !screenshot_request_ids.is_empty() || run_view_request.is_some();
        if capture_swapchain && self.swapchain_readback_buffer.is_none() {
            return Err(
                "swapchain capture requested but readback buffer is unavailable".to_string(),
            );
        }

        unsafe {
            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| format!("reset_command_buffer failed: {e:?}"))?;
        }

        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe {
            self.device
                .begin_command_buffer(self.command_buffer, &begin_info)
                .map_err(|e| format!("begin_command_buffer failed: {e:?}"))?;
        }

        self.texture_upload_count_this_frame = 0;
        self.texture_upload_bytes_this_frame = 0;
        self.prepare_draw_list_textures(cx, draw_list_id)?;

        let mut zbias = 0.0f32;
        let zbias_step = cx.passes[draw_pass_id].zbias_step;
        let mut draw_stats = VulkanDrawStats::default();
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
        ];
        let framebuffer = *self
            .framebuffers
            .get(image_index as usize)
            .ok_or_else(|| format!("invalid framebuffer index {image_index}"))?;
        let render_pass_info = vk::RenderPassBeginInfo::default()
            .render_pass(self.render_pass)
            .framebuffer(framebuffer)
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: self.swapchain_extent,
            })
            .clear_values(&clear_values);

        unsafe {
            self.device.cmd_begin_render_pass(
                self.command_buffer,
                &render_pass_info,
                vk::SubpassContents::INLINE,
            );
            self.device.cmd_set_viewport(
                self.command_buffer,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: self.swapchain_extent.height as f32,
                    width: self.swapchain_extent.width as f32,
                    height: -(self.swapchain_extent.height as f32),
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            self.device.cmd_set_scissor(
                self.command_buffer,
                0,
                &[vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: self.swapchain_extent,
                }],
            );
        }

        let xr_depth_view = self.ensure_xr_depth_dummy()?;
        let render_pass_key = self.main_render_pass_key();
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
        unsafe {
            self.device.cmd_end_render_pass(self.command_buffer);
        }

        if capture_swapchain {
            let width = self.swapchain_extent.width;
            let height = self.swapchain_extent.height;
            let byte_len = width as vk::DeviceSize * height as vk::DeviceSize * 4;
            let staging = self
                .swapchain_readback_buffer
                .ok_or_else(|| "swapchain color readback buffer unavailable".to_string())?;
            let image = self.swapchain_images[image_index as usize];
            let to_transfer = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .old_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .image(image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .base_mip_level(0)
                        .level_count(1)
                        .base_array_layer(0)
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
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .image_extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                });
            let to_present = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                .dst_access_mask(vk::AccessFlags::empty())
                .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .image(image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .base_mip_level(0)
                        .level_count(1)
                        .base_array_layer(0)
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
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    staging.buffer,
                    &[copy_region],
                );
                self.device.cmd_pipeline_barrier(
                    self.command_buffer,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE | vk::PipelineStageFlags::HOST,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[buffer_ready],
                    &[to_present],
                );
            }
        }

        unsafe {
            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| format!("end_command_buffer failed: {e:?}"))?;
        }

        let wait_semaphores = [self.image_available_semaphore];
        let signal_semaphores = [self.render_finished_semaphore];
        let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let cmd_buffers = [self.command_buffer];
        let submit_info = vk::SubmitInfo::default()
            .wait_semaphores(&wait_semaphores)
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(&cmd_buffers)
            .signal_semaphores(&signal_semaphores);

        unsafe {
            self.device
                .queue_submit(self.queue, &[submit_info], self.in_flight_fence)
                .map_err(|e| format!("queue_submit failed: {e:?}"))?;
        }
        self.gpu_submit_serial = self.gpu_submit_serial.saturating_add(1);
        self.window_in_flight_submit_serial = self.gpu_submit_serial;

        if capture_swapchain {
            unsafe {
                self.device
                    .wait_for_fences(&[self.in_flight_fence], true, u64::MAX)
                    .map_err(|e| format!("wait_for_fences(swapchain capture) failed: {e:?}"))?;
            }
            self.gpu_completed_submit_serial = self
                .gpu_completed_submit_serial
                .max(self.window_in_flight_submit_serial);
            self.window_in_flight_submit_serial = 0;
            self.collect_retired_texture_resources();
            let width = self.swapchain_extent.width.max(1);
            let height = self.swapchain_extent.height.max(1);
            let rgba = self.read_swapchain_color_image_rgba(image_index as usize)?;

            if !screenshot_request_ids.is_empty() {
                let png = Cx::encode_rgba_as_png(width, height, &rgba)?;
                Cx::send_studio_screenshot_response(screenshot_request_ids, width, height, png);
            }

            if let Some(request) = run_view_request {
                cx.encode_studio_run_view_frame_async(request, width, height, rgba);
            }
        }

        let swapchains = [self.swapchain];
        let image_indices = [image_index];
        let present_info = vk::PresentInfoKHR::default()
            .wait_semaphores(&signal_semaphores)
            .swapchains(&swapchains)
            .image_indices(&image_indices);

        let present_suboptimal = match unsafe {
            self.swapchain_loader
                .queue_present(self.queue, &present_info)
        } {
            Ok(suboptimal) => suboptimal,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.wait_for_window_frame_fence("out-of-date swapchain recreate")?;
                self.recreate_swapchain()?;
                return Ok(false);
            }
            Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                self.suspend_surface();
                return Ok(false);
            }
            Err(err) => {
                return Err(format!("queue_present failed: {err:?}"));
            }
        };

        if acquire_suboptimal || present_suboptimal {
            self.wait_for_window_frame_fence("suboptimal swapchain recreate")?;
            self.recreate_swapchain()?;
        }

        Ok(true)
    }

    fn ensure_pass_color_target(
        &mut self,
        cx: &mut Cx,
        texture_id: TextureId,
        width: usize,
        height: usize,
    ) -> Result<(), String> {
        let texture_key = Self::texture_key(texture_id);
        let (alloc_changed, alloc) = {
            let cxtexture = &mut cx.textures[texture_id];
            let alloc_changed = cxtexture.alloc_render(width, height);
            let alloc = cxtexture.alloc.clone().ok_or_else(|| {
                format!(
                    "render target texture {} missing allocation metadata",
                    texture_key
                )
            })?;
            (alloc_changed, alloc)
        };

        let format = Self::vk_color_format_from_texture_pixel(alloc.pixel).ok_or_else(|| {
            format!(
                "unsupported Vulkan render target pixel format for texture {}",
                texture_key
            )
        })?;
        let is_cube_target = matches!(alloc.category, TextureCategory::RenderCube);
        let target_width = alloc.width.max(1) as u32;
        let target_height = alloc.height.max(1) as u32;
        let needs_recreate = match self.textures.get(&texture_key) {
            Some(resource) => {
                alloc_changed
                    || resource.width != target_width
                    || resource.height != target_height
                    || resource.format != format
                    || resource.layers != if is_cube_target { 6 } else { 1 }
                    || resource.is_cube != is_cube_target
            }
            None => true,
        };

        if needs_recreate {
            if let Some(old_resource) = self.textures.remove(&texture_key) {
                self.retire_texture_resource(old_resource);
            }
            let resource = self.create_color_target_resource(
                target_width,
                target_height,
                format,
                is_cube_target,
            )?;
            self.textures.insert(texture_key, resource);
        }
        Ok(())
    }

    fn ensure_pass_depth_target(
        &mut self,
        cx: &mut Cx,
        texture_id: TextureId,
        width: usize,
        height: usize,
    ) -> Result<(), String> {
        let texture_key = Self::texture_key(texture_id);
        let (alloc_changed, alloc) = {
            let cxtexture = &mut cx.textures[texture_id];
            let alloc_changed = cxtexture.alloc_depth(width, height);
            let alloc = cxtexture.alloc.clone().ok_or_else(|| {
                format!(
                    "depth target texture {} missing allocation metadata",
                    texture_key
                )
            })?;
            (alloc_changed, alloc)
        };

        let format = match alloc.pixel {
            TexturePixel::D32 => vk::Format::D32_SFLOAT,
            _ => {
                return Err(format!(
                    "unsupported Vulkan depth target pixel format for texture {}",
                    texture_key
                ));
            }
        };
        let target_width = alloc.width.max(1) as u32;
        let target_height = alloc.height.max(1) as u32;
        let needs_recreate = match self.textures.get(&texture_key) {
            Some(resource) => {
                alloc_changed
                    || resource.width != target_width
                    || resource.height != target_height
                    || resource.format != format
                    || resource.layers != 1
                    || resource.is_cube
            }
            None => true,
        };

        if needs_recreate {
            if let Some(old_resource) = self.textures.remove(&texture_key) {
                self.retire_texture_resource(old_resource);
            }
            let resource = self.create_depth_target(target_width, target_height, format)?;
            self.textures.insert(texture_key, resource);
        }
        Ok(())
    }

    fn main_render_pass_key(&self) -> VulkanRenderPassKey {
        VulkanRenderPassKey::new(&[self.swapchain_format], Some(self.depth_format))
    }

    fn get_or_create_pipeline_render_pass(
        &mut self,
        key: &VulkanRenderPassKey,
    ) -> Result<vk::RenderPass, String> {
        if *key == self.main_render_pass_key() {
            if self.render_pass != vk::RenderPass::null() {
                return Ok(self.render_pass);
            }
            if self.xr_render_pass != vk::RenderPass::null() {
                return Ok(self.xr_render_pass);
            }
            return Err("main/XR Vulkan render pass is not ready".to_string());
        }
        if let Some(render_pass) = self.offscreen_render_passes.get(key) {
            return Ok(*render_pass);
        }

        let color_formats = key.color_vk_formats();
        let depth_format = key.depth_vk_format();
        let mut attachments =
            Vec::with_capacity(color_formats.len() + depth_format.is_some() as usize);
        let mut color_refs = Vec::with_capacity(color_formats.len());
        for (index, format) in color_formats.iter().enumerate() {
            attachments.push(
                vk::AttachmentDescription::default()
                    .format(*format)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .load_op(vk::AttachmentLoadOp::LOAD)
                    .store_op(vk::AttachmentStoreOp::STORE)
                    .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                    .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                    .final_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
            );
            color_refs.push(
                vk::AttachmentReference::default()
                    .attachment(index as u32)
                    .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL),
            );
        }
        let depth_ref = if let Some(format) = depth_format {
            attachments.push(
                vk::AttachmentDescription::default()
                    .format(format)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .load_op(vk::AttachmentLoadOp::LOAD)
                    .store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                    .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .initial_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
                    .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL),
            );
            Some(
                vk::AttachmentReference::default()
                    .attachment(color_formats.len() as u32)
                    .layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL),
            )
        } else {
            None
        };

        let mut subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_refs);
        if let Some(depth_ref) = depth_ref.as_ref() {
            subpass = subpass.depth_stencil_attachment(depth_ref);
        }
        let dependencies = [vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
                    | vk::PipelineStageFlags::FRAGMENT_SHADER,
            )
            .dst_stage_mask(
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
            )
            .src_access_mask(
                vk::AccessFlags::SHADER_READ
                    | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            )
            .dst_access_mask(
                vk::AccessFlags::COLOR_ATTACHMENT_READ
                    | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            )];
        let subpasses = [subpass];
        let render_pass_info = vk::RenderPassCreateInfo::default()
            .attachments(&attachments)
            .subpasses(&subpasses)
            .dependencies(&dependencies);
        let render_pass = unsafe { self.device.create_render_pass(&render_pass_info, None) }
            .map_err(|e| format!("create_render_pass(pipeline-cache) failed: {e:?}"))?;
        self.offscreen_render_passes
            .insert(key.clone(), render_pass);
        Ok(render_pass)
    }

    pub fn draw_pass_to_texture(
        &mut self,
        cx: &mut Cx,
        draw_pass_id: DrawPassId,
    ) -> Result<(), String> {
        let draw_list_id = if let Some(id) = cx.passes[draw_pass_id].main_draw_list_id {
            id
        } else {
            return Ok(());
        };

        let dpi_factor = cx.passes[draw_pass_id].dpi_factor.unwrap_or(1.0);
        let pass_rect = match cx.get_pass_rect(draw_pass_id, dpi_factor) {
            Some(rect) => rect,
            None => return Ok(()),
        };
        if pass_rect.size.x < 0.5 || pass_rect.size.y < 0.5 {
            return Ok(());
        }

        {
            let pass = &mut cx.passes[draw_pass_id];
            pass.paint_dirty = false;
            if !pass.keep_camera_matrix {
                pass.set_ortho_matrix(pass_rect.pos, pass_rect.size);
            }
            pass.set_dpi_factor(dpi_factor);
        }

        let target_width = (dpi_factor * pass_rect.size.x).max(1.0) as usize;
        let target_height = (dpi_factor * pass_rect.size.y).max(1.0) as usize;

        #[derive(Clone, Copy)]
        struct ColorAttachmentState {
            texture_id: TextureId,
            view: vk::ImageView,
            image: vk::Image,
            format: vk::Format,
            old_layout: vk::ImageLayout,
            layer_count: u32,
            should_clear: bool,
        }

        #[derive(Clone, Copy)]
        struct DepthAttachmentState {
            texture_id: TextureId,
            view: vk::ImageView,
            image: vk::Image,
            format: vk::Format,
            old_layout: vk::ImageLayout,
            should_clear: bool,
        }

        let pass_dont_clear = cx.passes[draw_pass_id].dont_clear;
        let color_targets: Vec<_> = cx.passes[draw_pass_id]
            .color_textures
            .iter()
            .map(|color_texture| {
                (
                    color_texture.texture.texture_id(),
                    color_texture.cube_face,
                    color_texture.clear_color.clone(),
                )
            })
            .collect();
        if color_targets.is_empty() {
            return Ok(());
        }
        let depth_target = cx.passes[draw_pass_id]
            .depth_texture
            .as_ref()
            .map(|texture| texture.texture_id());
        let clear_depth_value = match cx.passes[draw_pass_id].clear_depth {
            DrawPassClearDepth::InitWith(depth) | DrawPassClearDepth::ClearWith(depth) => depth,
        };

        for (texture_id, _, _) in &color_targets {
            self.ensure_pass_color_target(cx, *texture_id, target_width, target_height)?;
        }
        if let Some(texture_id) = depth_target {
            self.ensure_pass_depth_target(cx, texture_id, target_width, target_height)?;
        }

        unsafe {
            self.device
                .wait_for_fences(&[self.in_flight_fence], true, u64::MAX)
                .map_err(|e| format!("wait_for_fences(offscreen) failed: {e:?}"))?;
            self.device
                .reset_fences(&[self.in_flight_fence])
                .map_err(|e| format!("reset_fences(offscreen) failed: {e:?}"))?;
        }

        self.destroy_frame_resources();
        unsafe {
            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| format!("reset_command_buffer(offscreen) failed: {e:?}"))?;
            self.device
                .begin_command_buffer(
                    self.command_buffer,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(|e| format!("begin_command_buffer(offscreen) failed: {e:?}"))?;
        }

        self.texture_upload_count_this_frame = 0;
        self.texture_upload_bytes_this_frame = 0;
        self.prepare_draw_list_textures(cx, draw_list_id)?;

        let mut color_attachments = Vec::with_capacity(color_targets.len());
        let mut clear_values = Vec::with_capacity(color_targets.len() + 1);
        for (texture_id, cube_face, clear_color) in &color_targets {
            let should_clear = match clear_color {
                DrawPassClearColor::InitWith(_) => {
                    !pass_dont_clear && cx.textures[*texture_id].take_initial()
                }
                DrawPassClearColor::ClearWith(_) => !pass_dont_clear,
            };
            let clear = match clear_color {
                DrawPassClearColor::InitWith(color) | DrawPassClearColor::ClearWith(color) => {
                    *color
                }
            };
            let resource = self
                .textures
                .get(&Self::texture_key(*texture_id))
                .ok_or_else(|| {
                    format!("missing Vulkan color target for texture {:?}", texture_id)
                })?;
            color_attachments.push(ColorAttachmentState {
                texture_id: *texture_id,
                view: (*cube_face)
                    .and_then(|face| resource.face_views.get(face as usize).copied())
                    .filter(|view| *view != vk::ImageView::null())
                    .unwrap_or(resource.view),
                image: resource.image,
                format: resource.format,
                old_layout: resource.layout,
                layer_count: resource.layers,
                should_clear,
            });
            clear_values.push(vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [clear.x, clear.y, clear.z, clear.w],
                },
            });
        }

        let depth_attachment = if let Some(texture_id) = depth_target {
            let should_clear = match cx.passes[draw_pass_id].clear_depth {
                DrawPassClearDepth::InitWith(_) => {
                    !pass_dont_clear && cx.textures[texture_id].take_initial()
                }
                DrawPassClearDepth::ClearWith(_) => !pass_dont_clear,
            };
            let resource = self
                .textures
                .get(&Self::texture_key(texture_id))
                .ok_or_else(|| {
                    format!("missing Vulkan depth target for texture {:?}", texture_id)
                })?;
            clear_values.push(vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: clear_depth_value,
                    stencil: 0,
                },
            });
            Some(DepthAttachmentState {
                texture_id,
                view: resource.view,
                image: resource.image,
                format: resource.format,
                old_layout: resource.layout,
                should_clear,
            })
        } else {
            None
        };

        for attachment in &color_attachments {
            self.transition_image_layout(
                attachment.image,
                vk::ImageAspectFlags::COLOR,
                attachment.layer_count,
                attachment.old_layout,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            );
        }
        if let Some(depth) = depth_attachment {
            self.transition_image_layout(
                depth.image,
                vk::ImageAspectFlags::DEPTH,
                1,
                depth.old_layout,
                vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
            );
        }

        let color_attachment_descriptions: Vec<_> = color_attachments
            .iter()
            .map(|attachment| {
                vk::AttachmentDescription::default()
                    .format(attachment.format)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .load_op(if attachment.should_clear {
                        vk::AttachmentLoadOp::CLEAR
                    } else {
                        vk::AttachmentLoadOp::LOAD
                    })
                    .store_op(vk::AttachmentStoreOp::STORE)
                    .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                    .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                    .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            })
            .collect();
        let mut attachments = color_attachment_descriptions;
        let color_refs: Vec<_> = (0..color_attachments.len())
            .map(|index| {
                vk::AttachmentReference::default()
                    .attachment(index as u32)
                    .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            })
            .collect();
        let depth_ref = depth_attachment.as_ref().map(|_| {
            vk::AttachmentReference::default()
                .attachment(color_attachments.len() as u32)
                .layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
        });
        if let Some(depth) = depth_attachment {
            attachments.push(
                vk::AttachmentDescription::default()
                    .format(depth.format)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .load_op(if depth.should_clear {
                        vk::AttachmentLoadOp::CLEAR
                    } else {
                        vk::AttachmentLoadOp::LOAD
                    })
                    .store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                    .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .initial_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
                    .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL),
            );
        }

        let mut subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_refs);
        if let Some(depth_ref) = depth_ref.as_ref() {
            subpass = subpass.depth_stencil_attachment(depth_ref);
        }
        let dependencies = [vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
                    | vk::PipelineStageFlags::FRAGMENT_SHADER,
            )
            .dst_stage_mask(
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
            )
            .src_access_mask(
                vk::AccessFlags::SHADER_READ
                    | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            )
            .dst_access_mask(
                vk::AccessFlags::COLOR_ATTACHMENT_READ
                    | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            )];
        let subpasses = [subpass];
        let render_pass_info = vk::RenderPassCreateInfo::default()
            .attachments(&attachments)
            .subpasses(&subpasses)
            .dependencies(&dependencies);
        let render_pass = unsafe { self.device.create_render_pass(&render_pass_info, None) }
            .map_err(|e| format!("create_render_pass(offscreen) failed: {e:?}"))?;

        let mut framebuffer_attachments: Vec<vk::ImageView> = color_attachments
            .iter()
            .map(|attachment| attachment.view)
            .collect();
        if let Some(depth) = depth_attachment {
            framebuffer_attachments.push(depth.view);
        }
        let framebuffer_info = vk::FramebufferCreateInfo::default()
            .render_pass(render_pass)
            .attachments(&framebuffer_attachments)
            .width(target_width as u32)
            .height(target_height as u32)
            .layers(1);
        let framebuffer = unsafe { self.device.create_framebuffer(&framebuffer_info, None) }
            .map_err(|e| format!("create_framebuffer(offscreen) failed: {e:?}"))?;

        unsafe {
            self.device.cmd_begin_render_pass(
                self.command_buffer,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(render_pass)
                    .framebuffer(framebuffer)
                    .render_area(vk::Rect2D {
                        offset: vk::Offset2D { x: 0, y: 0 },
                        extent: vk::Extent2D {
                            width: target_width as u32,
                            height: target_height as u32,
                        },
                    })
                    .clear_values(&clear_values),
                vk::SubpassContents::INLINE,
            );
            self.device.cmd_set_viewport(
                self.command_buffer,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: target_height as f32,
                    width: target_width as f32,
                    height: -(target_height as f32),
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
                        width: target_width as u32,
                        height: target_height as u32,
                    },
                }],
            );
        }

        let xr_depth_view = self.ensure_xr_depth_dummy()?;
        let render_pass_key = VulkanRenderPassKey::new(
            &color_attachments
                .iter()
                .map(|attachment| attachment.format)
                .collect::<Vec<_>>(),
            depth_attachment.map(|depth| depth.format),
        );
        let mut zbias = 0.0f32;
        let zbias_step = cx.passes[draw_pass_id].zbias_step;
        let mut draw_stats = VulkanDrawStats::default();
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
        unsafe {
            self.device.cmd_end_render_pass(self.command_buffer);
        }
        for attachment in &color_attachments {
            self.transition_image_layout(
                attachment.image,
                vk::ImageAspectFlags::COLOR,
                attachment.layer_count,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            );
        }
        unsafe {
            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| format!("end_command_buffer(offscreen) failed: {e:?}"))?;
            self.device
                .queue_submit(
                    self.queue,
                    &[vk::SubmitInfo::default().command_buffers(&[self.command_buffer])],
                    self.in_flight_fence,
                )
                .map_err(|e| format!("queue_submit(offscreen) failed: {e:?}"))?;
            self.device
                .wait_for_fences(&[self.in_flight_fence], true, u64::MAX)
                .map_err(|e| format!("wait_for_fences(offscreen submit) failed: {e:?}"))?;
            self.device.destroy_framebuffer(framebuffer, None);
            self.device.destroy_render_pass(render_pass, None);
        }

        for attachment in &color_attachments {
            if let Some(resource) = self
                .textures
                .get_mut(&Self::texture_key(attachment.texture_id))
            {
                resource.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
            }
        }
        if let Some(depth) = depth_attachment {
            if let Some(resource) = self.textures.get_mut(&Self::texture_key(depth.texture_id)) {
                resource.layout = vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
            }
        }

        Ok(())
    }

    fn record_draw_list(
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

    fn record_draw_packet(
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

    fn align_device_size(value: vk::DeviceSize, alignment: vk::DeviceSize) -> vk::DeviceSize {
        if alignment <= 1 {
            value
        } else {
            value.div_ceil(alignment) * alignment
        }
    }

    fn create_host_buffer(
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

    fn destroy_buffer(&self, buffer: VulkanBuffer) {
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

    fn ensure_geometry_resource(
        &mut self,
        geometry_id: GeometryId,
        geometry: &mut crate::geometry::CxGeometry,
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

    fn alloc_frame_descriptor_set(
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

    fn create_host_buffer_with_data<T: Copy>(
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

    fn find_memory_type(
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

    fn create_surface(
        android_surface_loader: &ash::khr::android_surface::Instance,
        window: *mut ndk_sys::ANativeWindow,
    ) -> Result<vk::SurfaceKHR, String> {
        let surface_create_info = vk::AndroidSurfaceCreateInfoKHR::default().window(window.cast());
        unsafe { android_surface_loader.create_android_surface(&surface_create_info, None) }
            .map_err(|e| format!("create_android_surface failed: {e:?}"))
    }

    fn pick_device_and_queue_family(
        instance: &ash::Instance,
        surface_loader: &ash::khr::surface::Instance,
        surface: vk::SurfaceKHR,
    ) -> Result<(vk::PhysicalDevice, u32), String> {
        let physical_devices = unsafe { instance.enumerate_physical_devices() }
            .map_err(|e| format!("enumerate_physical_devices failed: {e:?}"))?;

        for physical_device in physical_devices {
            if let Ok(queue_family_index) = Self::pick_queue_family_for_device(
                instance,
                surface_loader,
                surface,
                physical_device,
            ) {
                return Ok((physical_device, queue_family_index));
            }
        }

        Err("No Vulkan physical device with graphics+present support found".to_string())
    }

    fn pick_queue_family_for_device(
        instance: &ash::Instance,
        surface_loader: &ash::khr::surface::Instance,
        surface: vk::SurfaceKHR,
        physical_device: vk::PhysicalDevice,
    ) -> Result<u32, String> {
        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        for (index, family) in queue_families.iter().enumerate() {
            if !family.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
                continue;
            }
            let supports_surface = unsafe {
                surface_loader.get_physical_device_surface_support(
                    physical_device,
                    index as u32,
                    surface,
                )
            }
            .map_err(|e| format!("get_physical_device_surface_support failed: {e:?}"))?;
            if supports_surface {
                return Ok(index as u32);
            }
        }
        Err("No Vulkan queue family with graphics+present support found".to_string())
    }

    fn pick_graphics_queue_family_for_device(
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
    ) -> Result<u32, String> {
        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        for (index, family) in queue_families.iter().enumerate() {
            if family.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
                return Ok(index as u32);
            }
        }
        Err("No graphics queue family found for OpenXR Vulkan device".to_string())
    }

    fn recreate_swapchain(&mut self) -> Result<(), String> {
        if self.surface == vk::SurfaceKHR::null() {
            return Ok(());
        }

        let capabilities = unsafe {
            self.surface_loader
                .get_physical_device_surface_capabilities(self.physical_device, self.surface)
        }
        .map_err(|e| format!("get_surface_capabilities failed: {e:?}"))?;

        let formats = unsafe {
            self.surface_loader
                .get_physical_device_surface_formats(self.physical_device, self.surface)
        }
        .map_err(|e| format!("get_surface_formats failed: {e:?}"))?;
        if formats.is_empty() {
            return Err("No Vulkan surface formats available".to_string());
        }

        let format = formats
            .iter()
            .copied()
            .find(|f| {
                f.format == vk::Format::B8G8R8A8_UNORM
                    && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
            })
            .unwrap_or(formats[0]);

        let extent = if capabilities.current_extent.width == u32::MAX {
            vk::Extent2D {
                width: self.requested_width.clamp(
                    capabilities.min_image_extent.width,
                    capabilities.max_image_extent.width,
                ),
                height: self.requested_height.clamp(
                    capabilities.min_image_extent.height,
                    capabilities.max_image_extent.height,
                ),
            }
        } else {
            capabilities.current_extent
        };

        let mut image_count = capabilities.min_image_count + 1;
        if capabilities.max_image_count > 0 {
            image_count = image_count.min(capabilities.max_image_count);
        }

        let present_modes = unsafe {
            self.surface_loader
                .get_physical_device_surface_present_modes(self.physical_device, self.surface)
        }
        .map_err(|e| format!("get_surface_present_modes failed: {e:?}"))?;
        let present_mode = if present_modes.contains(&vk::PresentModeKHR::FIFO) {
            vk::PresentModeKHR::FIFO
        } else {
            present_modes
                .first()
                .copied()
                .unwrap_or(vk::PresentModeKHR::FIFO)
        };

        let usage = capabilities.supported_usage_flags;
        if !usage.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT) {
            return Err("Vulkan surface does not support COLOR_ATTACHMENT usage".to_string());
        }
        let mut image_usage = vk::ImageUsageFlags::COLOR_ATTACHMENT;
        if usage.contains(vk::ImageUsageFlags::TRANSFER_DST) {
            image_usage |= vk::ImageUsageFlags::TRANSFER_DST;
        }

        let pre_transform = if capabilities
            .supported_transforms
            .contains(vk::SurfaceTransformFlagsKHR::IDENTITY)
        {
            vk::SurfaceTransformFlagsKHR::IDENTITY
        } else {
            capabilities.current_transform
        };

        let composite_alpha = [
            vk::CompositeAlphaFlagsKHR::OPAQUE,
            vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
            vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
            vk::CompositeAlphaFlagsKHR::INHERIT,
        ]
        .into_iter()
        .find(|mode| capabilities.supported_composite_alpha.contains(*mode))
        .unwrap_or(vk::CompositeAlphaFlagsKHR::OPAQUE);

        let old_swapchain = self.swapchain;
        self.destroy_swapchain_targets();
        self.destroy_pipelines();

        let queue_family_indices = [self.queue_family_index];
        let create_info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(image_count)
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(image_usage)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .queue_family_indices(&queue_family_indices)
            .pre_transform(pre_transform)
            .composite_alpha(composite_alpha)
            .present_mode(present_mode)
            .clipped(true)
            .old_swapchain(old_swapchain);

        let new_swapchain = unsafe { self.swapchain_loader.create_swapchain(&create_info, None) }
            .map_err(|e| format!("create_swapchain failed: {e:?}"))?;
        let new_images = unsafe { self.swapchain_loader.get_swapchain_images(new_swapchain) }
            .map_err(|e| format!("get_swapchain_images failed: {e:?}"))?;

        if old_swapchain != vk::SwapchainKHR::null() {
            unsafe { self.swapchain_loader.destroy_swapchain(old_swapchain, None) };
        }

        self.swapchain = new_swapchain;
        self.swapchain_images = new_images;
        self.swapchain_format = format.format;
        self.depth_format = self.pick_depth_format()?;
        self.swapchain_extent = extent;

        let color_attachment = vk::AttachmentDescription::default()
            .format(self.swapchain_format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::PRESENT_SRC_KHR);
        let depth_attachment = vk::AttachmentDescription::default()
            .format(self.depth_format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::DONT_CARE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL);
        let color_ref = vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let depth_ref = vk::AttachmentReference::default()
            .attachment(1)
            .layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL);
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
                    | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
            )
            .dst_stage_mask(
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
            )
            .dst_access_mask(
                vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            )];
        let attachments = [color_attachment, depth_attachment];
        let subpasses = [subpass];
        let render_pass_info = vk::RenderPassCreateInfo::default()
            .attachments(&attachments)
            .subpasses(&subpasses)
            .dependencies(&dependencies);
        self.render_pass = unsafe { self.device.create_render_pass(&render_pass_info, None) }
            .map_err(|e| format!("create_render_pass failed: {e:?}"))?;
        let xr_color_attachment =
            color_attachment.final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let xr_attachments = [xr_color_attachment, depth_attachment];
        let xr_render_pass_info = vk::RenderPassCreateInfo::default()
            .attachments(&xr_attachments)
            .subpasses(&subpasses)
            .dependencies(&dependencies);
        self.xr_render_pass = unsafe { self.device.create_render_pass(&xr_render_pass_info, None) }
            .map_err(|e| format!("create_render_pass(openxr) failed: {e:?}"))?;
        self.xr_render_pass_uses_fragment_density_map = false;

        for image in &self.swapchain_images {
            let view_info = vk::ImageViewCreateInfo::default()
                .image(*image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(self.swapchain_format)
                .components(vk::ComponentMapping::default())
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let view = unsafe { self.device.create_image_view(&view_info, None) }
                .map_err(|e| format!("create_image_view failed: {e:?}"))?;
            self.swapchain_image_views.push(view);
        }

        self.swapchain_depth_targets
            .reserve(self.swapchain_images.len());
        for _ in &self.swapchain_images {
            let depth_target = self.create_depth_target(
                self.swapchain_extent.width,
                self.swapchain_extent.height,
                self.depth_format,
            )?;
            self.swapchain_depth_targets.push(depth_target);
        }

        self.swapchain_readback_buffer = if self.swapchain_readback_supported()
            && self.swapchain_extent.width > 0
            && self.swapchain_extent.height > 0
        {
            Some(self.create_host_buffer(
                vk::BufferUsageFlags::TRANSFER_DST,
                self.swapchain_extent.width as vk::DeviceSize
                    * self.swapchain_extent.height as vk::DeviceSize
                    * 4,
            )?)
        } else {
            None
        };

        for (index, view) in self.swapchain_image_views.iter().enumerate() {
            let depth_view = self
                .swapchain_depth_targets
                .get(index)
                .ok_or_else(|| format!("missing depth target for framebuffer {index}"))?
                .view;
            let attachments = [*view, depth_view];
            let framebuffer_info = vk::FramebufferCreateInfo::default()
                .render_pass(self.render_pass)
                .attachments(&attachments)
                .width(self.swapchain_extent.width)
                .height(self.swapchain_extent.height)
                .layers(1);
            let framebuffer = unsafe { self.device.create_framebuffer(&framebuffer_info, None) }
                .map_err(|e| format!("create_framebuffer failed: {e:?}"))?;
            self.framebuffers.push(framebuffer);
        }

        Ok(())
    }

    fn destroy_frame_resources(&mut self) {
        Self::destroy_owned_frame_resources(&self.device, &mut self.frame_resources);
    }

    fn destroy_pipelines(&mut self) {
        unsafe {
            for (_, pipeline) in self.pipelines.drain() {
                for sampler in pipeline.sampler_handles {
                    self.device.destroy_sampler(sampler, None);
                }
                self.device.destroy_pipeline(pipeline.pipeline_write, None);
                self.device
                    .destroy_pipeline(pipeline.pipeline_no_write, None);
                self.device.destroy_pipeline_layout(pipeline.layout, None);
                self.device
                    .destroy_descriptor_set_layout(pipeline.descriptor_set_layout, None);
            }
            for (_, render_pass) in self.offscreen_render_passes.drain() {
                self.device.destroy_render_pass(render_pass, None);
            }
        }
    }

    fn destroy_swapchain_targets(&mut self) {
        unsafe {
            for framebuffer in self.framebuffers.drain(..) {
                self.device.destroy_framebuffer(framebuffer, None);
            }
            let depth_targets: Vec<VulkanTextureResource> =
                self.swapchain_depth_targets.drain(..).collect();
            for depth in depth_targets {
                self.destroy_texture_resource(depth);
            }
            for image_view in self.swapchain_image_views.drain(..) {
                self.device.destroy_image_view(image_view, None);
            }
            if self.render_pass != vk::RenderPass::null() {
                self.device.destroy_render_pass(self.render_pass, None);
                self.render_pass = vk::RenderPass::null();
            }
            if self.xr_render_pass != vk::RenderPass::null() {
                self.device.destroy_render_pass(self.xr_render_pass, None);
                self.xr_render_pass = vk::RenderPass::null();
            }
            if let Some(buffer) = self.swapchain_readback_buffer.take() {
                self.device.destroy_buffer(buffer.buffer, None);
                self.device.free_memory(buffer.memory, None);
            }
            self.depth_format = vk::Format::UNDEFINED;
        }
    }

    fn destroy_swapchain(&mut self) {
        self.destroy_frame_resources();
        self.destroy_pipelines();
        self.destroy_swapchain_targets();
        if self.swapchain != vk::SwapchainKHR::null() {
            unsafe {
                self.swapchain_loader
                    .destroy_swapchain(self.swapchain, None)
            };
            self.swapchain = vk::SwapchainKHR::null();
        }
        self.swapchain_images.clear();
    }

    fn destroy_texture_resources(&mut self) {
        let retired: Vec<VulkanTextureResource> = self
            .retired_texture_resources
            .drain(..)
            .map(|retired| retired.resource)
            .collect();
        for resource in retired {
            self.destroy_texture_resource(resource);
        }
        let cached_video_hardware_buffer_resources: Vec<VulkanTextureResource> = self
            .video_hardware_buffer_texture_cache
            .drain(..)
            .map(|entry| entry.resource)
            .collect();
        for resource in cached_video_hardware_buffer_resources {
            self.destroy_texture_resource(resource);
        }
        let mut resources: Vec<VulkanTextureResource> =
            self.textures.drain().map(|(_, r)| r).collect();
        resources.sort_by_key(|resource| resource.owns_image);
        for resource in resources {
            self.destroy_texture_resource(resource);
        }
        if let Some(resource) = self.xr_depth_dummy.take() {
            self.destroy_texture_resource(resource);
        }
        if let Some(resource) = self.xr_depth_dummy_multiview.take() {
            self.destroy_texture_resource(resource);
        }
    }

    fn destroy_external_ycbcr_samplers(&mut self) {
        let samplers: Vec<VulkanExternalYcbcrSampler> = self
            .external_ycbcr_samplers
            .drain()
            .map(|(_, r)| r)
            .collect();
        for sampler in samplers {
            unsafe {
                self.device.destroy_sampler(sampler.sampler, None);
                self.device
                    .destroy_sampler_ycbcr_conversion(sampler.conversion, None);
            }
        }
    }

    fn destroy_geometry_resources(&mut self) {
        let resources: Vec<VulkanGeometryResource> = self
            .geometries
            .drain()
            .map(|(_, resource)| resource)
            .collect();
        for resource in resources {
            self.destroy_geometry_resource(resource);
        }
    }

    fn destroy_surface(&mut self) {
        if self.surface != vk::SurfaceKHR::null() {
            unsafe { self.surface_loader.destroy_surface(self.surface, None) };
            self.surface = vk::SurfaceKHR::null();
        }
    }

    fn device_wait_idle(&self) {
        let _ = unsafe { self.device.device_wait_idle() };
    }

    fn wait_for_window_frame_fence(&self, reason: &str) -> Result<(), String> {
        if self.in_flight_fence == vk::Fence::null() {
            return Ok(());
        }
        // Avoid teardown while the submitted frame still owns swapchain-backed resources.
        unsafe {
            self.device
                .wait_for_fences(&[self.in_flight_fence], true, u64::MAX)
                .map_err(|e| format!("wait_for_fences({reason}) failed: {e:?}"))?;
        }
        Ok(())
    }
}

impl Drop for CxVulkan {
    fn drop(&mut self) {
        self.device_wait_idle();
        self.destroy_swapchain();
        self.destroy_xr_in_flight_frames();
        self.destroy_xr_storage_buffer_probe_resources();
        self.destroy_xr_u32_compute_probe_resources();
        self.destroy_xr_f32_force_probe_resources();
        self.destroy_xr_f32_skinning_probe_resources();
        self.destroy_xr_f32_skinning_mesh_probe_resources();
        self.destroy_xr_f32_field_force_sample_probe_resources();
        self.destroy_xr_f32_field_force_sample_probe_program();
        self.destroy_xr_f32_field_sample_probe_resources();
        self.destroy_xr_f32_field_sample_probe_program();
        self.destroy_xr_f32_mesh_sdf_probe_resources();
        self.destroy_xr_f32_mesh_sdf_probe_derived_buffers();
        self.destroy_xr_f32_mesh_sdf_probe_source_mesh_buffers();
        self.destroy_xr_f32_mesh_sdf_probe_program();
        self.destroy_xr_f32_volume_probe_resources();
        self.destroy_xr_f32_volume_image_preview_resources();
        self.destroy_xr_f32_volume_raymarch_preview_resources();
        self.destroy_geometry_resources();
        self.destroy_texture_resources();
        self.destroy_external_ycbcr_samplers();

        unsafe {
            if self.in_flight_fence != vk::Fence::null() {
                self.device.destroy_fence(self.in_flight_fence, None);
            }
            if self.render_finished_semaphore != vk::Semaphore::null() {
                self.device
                    .destroy_semaphore(self.render_finished_semaphore, None);
            }
            if self.image_available_semaphore != vk::Semaphore::null() {
                self.device
                    .destroy_semaphore(self.image_available_semaphore, None);
            }
            if self.command_pool != vk::CommandPool::null() {
                self.device.destroy_command_pool(self.command_pool, None);
            }
            self.device.destroy_device(None);
        }

        self.destroy_surface();
        if let Some(loader) = &self.debug_utils_loader {
            if self.debug_messenger != vk::DebugUtilsMessengerEXT::null() {
                unsafe { loader.destroy_debug_utils_messenger(self.debug_messenger, None) };
                self.debug_messenger = vk::DebugUtilsMessengerEXT::null();
            }
        }
        unsafe { self.instance.destroy_instance(None) };

        if !self.window.is_null() {
            unsafe { ndk_sys::ANativeWindow_release(self.window) };
            self.window = std::ptr::null_mut();
        }
    }
}
