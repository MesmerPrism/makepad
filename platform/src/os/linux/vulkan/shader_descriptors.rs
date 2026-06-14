use crate::os::linux::vulkan_naga::{CxVulkanShaderBinary, CxVulkanShaderDescriptorKind};
use ash::vk;

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

pub(super) fn vulkan_descriptor_type_name(descriptor_type: vk::DescriptorType) -> &'static str {
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

pub(super) fn reflected_vulkan_descriptor_type(
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

pub(super) fn reflected_shader_descriptor_kind_name(
    vk_shader: &CxVulkanShaderBinary,
    binding: u32,
) -> &'static str {
    vk_shader
        .resource_interface
        .descriptor_kind(0, binding)
        .map(CxVulkanShaderDescriptorKind::stable_name)
        .unwrap_or("missing")
}
