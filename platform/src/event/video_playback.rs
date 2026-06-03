use std::rc::Rc;

use crate::makepad_live_id::LiveId;
use crate::texture::Texture;
use crate::video::{VideoFormatId, VideoInputId};
use crate::{MediaPlaybackSessionId, TextureId, VideoFrameSessionId};

#[derive(Clone, Debug)]
pub struct VideoPlaybackPreparedEvent {
    pub video_id: LiveId,
    pub video_width: u32,
    pub video_height: u32,
    pub duration: u128,
    /// Whether the source supports seeking.
    pub is_seekable: bool,
    /// Descriptive labels for video tracks (empty for audio-only sources).
    pub video_tracks: Vec<String>,
    /// Descriptive labels for audio tracks.
    pub audio_tracks: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct VideoPlaybackMetadataEvent {
    pub video_id: LiveId,
    pub metadata_json: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct VideoYuvMetadata {
    /// When true, the shader should use YUV textures instead of external RGB.
    pub enabled: bool,
    /// Color matrix selector: 0.0 = BT.709, 1.0 = BT.601, 2.0 = BT.2020.
    pub matrix: f32,
    /// When true, UV is in a single RG8 texture (NV12 biplanar).
    pub biplanar: bool,
    /// YUV texture rotation in quarter turns clockwise (0, 1, 2, 3).
    pub rotation_steps: f32,
}

impl VideoYuvMetadata {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn shader_enabled(self) -> f32 {
        if self.enabled {
            1.0
        } else {
            0.0
        }
    }

    pub fn shader_biplanar(self) -> f32 {
        if self.biplanar {
            1.0
        } else {
            0.0
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VideoTextureResourcePath {
    #[default]
    Unspecified,
    CpuYuvPlanes,
    HardwareBufferExternal,
    HardwareBufferYuvPlanes,
    SurfaceTextureExternal,
    SoftwareYuvPlanes,
}

impl VideoTextureResourcePath {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unspecified => "unspecified",
            Self::CpuYuvPlanes => "cpu-yuv-planes",
            Self::HardwareBufferExternal => "hardware-buffer-external",
            Self::HardwareBufferYuvPlanes => "hardware-buffer-yuv-planes",
            Self::SurfaceTextureExternal => "surface-texture-external",
            Self::SoftwareYuvPlanes => "software-yuv-planes",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VideoTextureDescriptorShape {
    #[default]
    Unspecified,
    CpuYuvPlaneTextures,
    ImportedYuvPlaneTextures,
    SampledImageAndSampler,
    SampledImageAndSamplerYcbcrConversion,
    CombinedImmutableSamplerYcbcrConversion,
    SurfaceTextureExternalOes,
}

impl VideoTextureDescriptorShape {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unspecified => "unspecified",
            Self::CpuYuvPlaneTextures => "cpu-yuv-plane-textures",
            Self::ImportedYuvPlaneTextures => "imported-yuv-plane-textures",
            Self::SampledImageAndSampler => "sampled-image-and-sampler",
            Self::SampledImageAndSamplerYcbcrConversion => {
                "sampled-image-and-sampler-ycbcr-conversion"
            }
            Self::CombinedImmutableSamplerYcbcrConversion => {
                "combined-immutable-sampler-ycbcr-conversion"
            }
            Self::SurfaceTextureExternalOes => "surface-texture-external-oes",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VideoTextureYcbcrConversionMetadata {
    pub suggested_model: String,
    pub suggested_range: String,
    pub effective_model: String,
    pub effective_range: String,
    pub components: String,
    pub suggested_x_chroma_offset: String,
    pub suggested_y_chroma_offset: String,
    pub conversion_mode: String,
    pub sampler_binding_mode: String,
    pub sampler_binding_compliance: String,
    pub shader_sample_lowering: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VideoTextureUpdateMetadata {
    pub resource_path: VideoTextureResourcePath,
    pub descriptor_shape: VideoTextureDescriptorShape,
    pub camera_input_id: Option<VideoInputId>,
    pub camera_format_id: Option<VideoFormatId>,
    pub camera_frame_sequence: Option<u64>,
    pub camera_timestamp_ns: Option<u64>,
    pub acquire_time_ns: Option<u64>,
    pub upload_sequence: Option<u64>,
    pub upload_time_ns: Option<u64>,
    pub import_sequence: Option<u64>,
    pub import_time_ns: Option<u64>,
    pub texture_update_sequence: Option<u64>,
    pub width: u32,
    pub height: u32,
    pub vulkan_format: Option<String>,
    pub vulkan_external_format: Option<u64>,
    pub ycbcr_conversion: Option<VideoTextureYcbcrConversionMetadata>,
    pub resource_reused: Option<bool>,
    pub fallback_active: bool,
    pub fallback_reason: Option<String>,
}

impl VideoTextureUpdateMetadata {
    pub fn with_resource(
        mut self,
        resource_path: VideoTextureResourcePath,
        descriptor_shape: VideoTextureDescriptorShape,
        width: u32,
        height: u32,
    ) -> Self {
        self.resource_path = resource_path;
        self.descriptor_shape = descriptor_shape;
        self.width = width;
        self.height = height;
        self
    }

    pub fn with_camera_source(mut self, input_id: VideoInputId, format_id: VideoFormatId) -> Self {
        self.camera_input_id = Some(input_id);
        self.camera_format_id = Some(format_id);
        self
    }

    pub fn with_camera_frame(
        mut self,
        sequence: u64,
        timestamp_ns: u64,
        acquire_time_ns: Option<u64>,
    ) -> Self {
        self.camera_frame_sequence = Some(sequence);
        self.camera_timestamp_ns = Some(timestamp_ns);
        self.acquire_time_ns = acquire_time_ns;
        self
    }

    pub fn with_cpu_yuv_upload(mut self, upload_sequence: u64, upload_time_ns: u64) -> Self {
        self.upload_sequence = Some(upload_sequence);
        self.upload_time_ns = Some(upload_time_ns);
        self.texture_update_sequence = Some(upload_sequence);
        self
    }

    pub fn with_hardware_buffer_import(
        mut self,
        import_sequence: u64,
        import_time_ns: u64,
    ) -> Self {
        self.import_sequence = Some(import_sequence);
        self.import_time_ns = Some(import_time_ns);
        self.texture_update_sequence = Some(import_sequence);
        self
    }

    pub fn with_vulkan_format(
        mut self,
        vulkan_format: impl Into<String>,
        external_format: Option<u64>,
    ) -> Self {
        self.vulkan_format = Some(vulkan_format.into());
        self.vulkan_external_format = external_format;
        self
    }

    pub fn with_ycbcr_conversion(
        mut self,
        ycbcr_conversion: VideoTextureYcbcrConversionMetadata,
    ) -> Self {
        self.ycbcr_conversion = Some(ycbcr_conversion);
        self
    }

    pub fn with_resource_reused(mut self, resource_reused: bool) -> Self {
        self.resource_reused = Some(resource_reused);
        self
    }

    pub fn with_fallback(mut self, reason: impl Into<String>) -> Self {
        self.fallback_active = true;
        self.fallback_reason = Some(reason.into());
        self
    }
}

#[derive(Clone, Debug)]
pub struct VideoTextureUpdatedEvent {
    pub video_id: LiveId,
    pub current_position_ms: u128,
    pub yuv: VideoYuvMetadata,
    pub metadata: VideoTextureUpdateMetadata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraPreviewMode {
    Texture,
    Native,
    Auto,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BrokerH264VideoSource {
    pub broker_host: String,
    pub broker_port: u16,
    pub stream_port: u16,
    pub source_mode: String,
    /// `auto` keeps the historical Android behavior: SurfaceTexture when an
    /// external texture handle is supplied, otherwise CPU-YUV plane callbacks.
    pub decode_output_mode: String,
    pub synthetic_pattern: String,
    pub synthetic_projection_profile: String,
    pub source_sampling_mode: String,
    pub target_screen_uv_rect: String,
    pub camera_id: String,
    pub stereo_pair_id: String,
    pub stereo_pair_role: String,
    pub stereo_pair_max_delta_ns: u32,
    pub preferred_width: u32,
    pub preferred_height: u32,
    pub capture_ms: u32,
    pub max_packets: u32,
    pub bitrate_bps: u32,
    pub frame_rate_hz: u32,
    pub command_timeout_ms: u32,
    pub stream_timeout_ms: u32,
    pub decode_timeout_ms: u32,
    pub live_stream: bool,
}

impl Default for BrokerH264VideoSource {
    fn default() -> Self {
        Self {
            broker_host: "127.0.0.1".to_string(),
            broker_port: 8765,
            stream_port: 8879,
            source_mode: "broker-synthetic".to_string(),
            decode_output_mode: "auto".to_string(),
            synthetic_pattern: "diagnostic-grid".to_string(),
            synthetic_projection_profile: "head-anchored-virtual-camera".to_string(),
            source_sampling_mode: String::new(),
            target_screen_uv_rect: String::new(),
            camera_id: String::new(),
            stereo_pair_id: String::new(),
            stereo_pair_role: String::new(),
            stereo_pair_max_delta_ns: 25_000_000,
            preferred_width: 1280,
            preferred_height: 1280,
            capture_ms: 900,
            max_packets: 32,
            bitrate_bps: 2_000_000,
            frame_rate_hz: 30,
            command_timeout_ms: 10_000,
            stream_timeout_ms: 20_000,
            decode_timeout_ms: 5_000,
            live_stream: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum VideoSource {
    InMemory(Rc<Vec<u8>>),
    Network(String),
    Filesystem(String),
    Camera(VideoInputId, VideoFormatId),
    BrokerH264(BrokerH264VideoSource),
    PlaybackSession(MediaPlaybackSessionId),
    Session(VideoFrameSessionId),
}

impl VideoSource {
    pub fn is_session(&self) -> bool {
        matches!(self, Self::PlaybackSession(..) | Self::Session(..))
    }

    pub fn supports_software_fallback(&self) -> bool {
        matches!(
            self,
            Self::InMemory(..)
                | Self::Network(..)
                | Self::Filesystem(..)
                | Self::PlaybackSession(..)
                | Self::Session(..)
        )
    }
}

#[derive(Clone, Debug)]
pub struct VideoPlaybackCompletedEvent {
    pub video_id: LiveId,
}

#[derive(Clone, Debug)]
pub struct VideoPlaybackResourcesReleasedEvent {
    pub video_id: LiveId,
}

#[derive(Clone, Debug)]
pub struct VideoDecodingErrorEvent {
    pub video_id: LiveId,
    pub error: String,
}

#[derive(Clone, Debug)]
pub struct TextureHandleReadyEvent {
    pub texture_id: TextureId,
    pub handle: u32,
}

/// Emitted by platform backends when YUV plane textures have been allocated
/// internally. The Video widget uses this to bind the textures to shader slots.
#[derive(Clone, Debug)]
pub struct VideoYuvTexturesReady {
    pub video_id: LiveId,
    pub tex_y: Texture,
    pub tex_u: Texture,
    pub tex_v: Texture,
}

/// Seekable time ranges for a video, in seconds.
#[derive(Clone, Debug)]
pub struct VideoSeekableRangesEvent {
    pub video_id: LiveId,
    pub ranges: Vec<(f64, f64)>,
}

/// Buffered (already downloaded/decoded) time ranges for a video, in seconds.
#[derive(Clone, Debug)]
pub struct VideoBufferedRangesEvent {
    pub video_id: LiveId,
    pub ranges: Vec<(f64, f64)>,
}
