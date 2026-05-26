use {
    crate::makepad_script::{
        shader::ShaderOutput, shader_wgsl::compile_draw_shader_wgsl_source, value::ScriptObject,
        vm::ScriptVm,
    },
    std::fmt::Write,
};

#[derive(Clone)]
pub struct CxVulkanShaderBinary {
    pub vertex_spirv: Option<Vec<u32>>,
    pub fragment_spirv: Option<Vec<u32>>,
    pub dyn_uniform_binding: u32,
    pub texture_binding_base: u32,
    pub sampler_binding_base: u32,
    pub xr_depth_binding: u32,
    pub geometry_slots: usize,
    pub instance_slots: usize,
    pub resource_interface: CxVulkanShaderResourceInterface,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CxVulkanShaderDescriptorKind {
    UniformBuffer,
    StorageBuffer,
    SampledImage,
    DepthImage,
    ExternalImage,
    StorageImage,
    Sampler,
    ComparisonSampler,
    OtherHandle,
}

impl CxVulkanShaderDescriptorKind {
    pub fn stable_name(self) -> &'static str {
        match self {
            Self::UniformBuffer => "uniform-buffer",
            Self::StorageBuffer => "storage-buffer",
            Self::SampledImage => "sampled-image",
            Self::DepthImage => "depth-image",
            Self::ExternalImage => "external-image",
            Self::StorageImage => "storage-image",
            Self::Sampler => "sampler",
            Self::ComparisonSampler => "comparison-sampler",
            Self::OtherHandle => "other-handle",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CxVulkanShaderResourceBinding {
    pub name: Option<String>,
    pub group: u32,
    pub binding: u32,
    pub descriptor_kind: CxVulkanShaderDescriptorKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CxVulkanShaderResourceInterface {
    pub bindings: Vec<CxVulkanShaderResourceBinding>,
}

impl CxVulkanShaderResourceInterface {
    pub fn descriptor_kind(
        &self,
        group: u32,
        binding: u32,
    ) -> Option<CxVulkanShaderDescriptorKind> {
        self.bindings
            .iter()
            .find(|resource| resource.group == group && resource.binding == binding)
            .map(|resource| resource.descriptor_kind)
    }
}

fn compile_wgsl_to_spirv(
    wgsl: &str,
) -> Result<
    (
        Option<Vec<u32>>,
        Option<Vec<u32>>,
        CxVulkanShaderResourceInterface,
    ),
    String,
> {
    use naga::{back::spv, valid};

    fn extract_error_line(details: &str) -> Option<usize> {
        let marker = "wgsl:";
        let start = details.find(marker)? + marker.len();
        let rest = &details[start..];
        let end = rest.find(':')?;
        rest[..end].trim().parse::<usize>().ok()
    }

    fn wgsl_context(wgsl: &str, line: usize, radius: usize) -> String {
        let start = line.saturating_sub(radius).max(1);
        let end = line.saturating_add(radius);
        let mut out = String::new();
        for (i, src_line) in wgsl.lines().enumerate() {
            let ln = i + 1;
            if ln >= start && ln <= end {
                let _ = writeln!(out, "{ln:4} | {src_line}");
            }
        }
        out
    }

    let module = naga::front::wgsl::parse_str(wgsl).map_err(|e| {
        let details = e.emit_to_string(wgsl);
        let context = extract_error_line(&details)
            .map(|line| {
                format!(
                    "\nWGSL context around line {line}:\n{}",
                    wgsl_context(wgsl, line, 4)
                )
            })
            .unwrap_or_default();
        format!("WGSL parse error: {e}\n{details}{context}")
    })?;

    let mut validator =
        valid::Validator::new(valid::ValidationFlags::all(), valid::Capabilities::all());
    let module_info = validator
        .validate(&module)
        .map_err(|e| format!("WGSL validation error: {e}"))?;
    let resource_interface = reflect_shader_resource_interface(&module);

    let options = spv::Options {
        lang_version: (1, 3),
        flags: spv::WriterFlags::empty(),
        fake_missing_bindings: true,
        binding_map: spv::BindingMap::default(),
        capabilities: None,
        bounds_check_policies: naga::proc::BoundsCheckPolicies::default(),
        zero_initialize_workgroup_memory: spv::ZeroInitializeWorkgroupMemoryMode::None,
        force_loop_bounding: false,
        use_storage_input_output_16: false,
        debug_info: None,
    };

    let has_vertex = module
        .entry_points
        .iter()
        .any(|ep| ep.stage == naga::ShaderStage::Vertex && ep.name == "vertex_main");
    let has_fragment = module
        .entry_points
        .iter()
        .any(|ep| ep.stage == naga::ShaderStage::Fragment && ep.name == "fragment_main");

    if !has_vertex && !has_fragment {
        return Err("WGSL module has no entry points".to_string());
    }

    let vertex_spirv = if has_vertex {
        let pipeline = spv::PipelineOptions {
            shader_stage: naga::ShaderStage::Vertex,
            entry_point: "vertex_main".to_string(),
        };
        Some(
            spv::write_vec(&module, &module_info, &options, Some(&pipeline))
                .map_err(|e| format!("SPIR-V write failed for vertex_main: {e}"))?,
        )
    } else {
        None
    };

    let fragment_spirv = if has_fragment {
        let pipeline = spv::PipelineOptions {
            shader_stage: naga::ShaderStage::Fragment,
            entry_point: "fragment_main".to_string(),
        };
        Some(
            spv::write_vec(&module, &module_info, &options, Some(&pipeline))
                .map_err(|e| format!("SPIR-V write failed for fragment_main: {e}"))?,
        )
    } else {
        None
    };

    Ok((vertex_spirv, fragment_spirv, resource_interface))
}

fn reflect_shader_resource_interface(module: &naga::Module) -> CxVulkanShaderResourceInterface {
    use naga::{AddressSpace, ImageClass, TypeInner};

    let mut bindings = Vec::new();
    for (_, global) in module.global_variables.iter() {
        let Some(binding) = global.binding else {
            continue;
        };
        let ty = &module.types[global.ty];
        let descriptor_kind = match global.space {
            AddressSpace::Uniform => CxVulkanShaderDescriptorKind::UniformBuffer,
            AddressSpace::Storage { .. } => CxVulkanShaderDescriptorKind::StorageBuffer,
            AddressSpace::Handle => match &ty.inner {
                TypeInner::Image { class, .. } => match class {
                    ImageClass::Sampled { .. } => CxVulkanShaderDescriptorKind::SampledImage,
                    ImageClass::Depth { .. } => CxVulkanShaderDescriptorKind::DepthImage,
                    ImageClass::External => CxVulkanShaderDescriptorKind::ExternalImage,
                    ImageClass::Storage { .. } => CxVulkanShaderDescriptorKind::StorageImage,
                },
                TypeInner::Sampler { comparison } => {
                    if *comparison {
                        CxVulkanShaderDescriptorKind::ComparisonSampler
                    } else {
                        CxVulkanShaderDescriptorKind::Sampler
                    }
                }
                _ => CxVulkanShaderDescriptorKind::OtherHandle,
            },
            _ => continue,
        };
        bindings.push(CxVulkanShaderResourceBinding {
            name: global.name.clone(),
            group: binding.group,
            binding: binding.binding,
            descriptor_kind,
        });
    }
    bindings.sort_by_key(|resource| (resource.group, resource.binding));
    CxVulkanShaderResourceInterface { bindings }
}

pub(crate) fn compile_draw_shader_wgsl_to_spirv(
    vm: &mut ScriptVm,
    io_self: ScriptObject,
    layout_source: &ShaderOutput,
    xr_multiview: bool,
) -> Result<CxVulkanShaderBinary, String> {
    let wgsl_source = compile_draw_shader_wgsl_source(vm, io_self, layout_source, xr_multiview)?;

    if std::env::var_os("MAKEPAD_DUMP_VULKAN_WGSL").is_some() {
        let variant = if xr_multiview { "xr" } else { "window" };
        crate::log!("---- Vulkan WGSL ({variant}) ----\n{}", wgsl_source.wgsl);
    }

    let (vertex_spirv, fragment_spirv, resource_interface) =
        compile_wgsl_to_spirv(&wgsl_source.wgsl).map_err(|err| {
            format!("{err}\nSet MAKEPAD_DUMP_VULKAN_WGSL=1 to dump generated WGSL.")
        })?;

    Ok(CxVulkanShaderBinary {
        vertex_spirv,
        fragment_spirv,
        dyn_uniform_binding: wgsl_source.dyn_uniform_binding,
        texture_binding_base: wgsl_source.texture_binding_base,
        sampler_binding_base: wgsl_source.sampler_binding_base,
        xr_depth_binding: wgsl_source.xr_depth_binding,
        geometry_slots: wgsl_source.geometry_slots,
        instance_slots: wgsl_source.instance_slots,
        resource_interface,
    })
}
