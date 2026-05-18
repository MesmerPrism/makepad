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

#[derive(Clone, Debug)]
pub struct VideoTextureUpdatedEvent {
    pub video_id: LiveId,
    pub current_position_ms: u128,
    pub yuv: VideoYuvMetadata,
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
    pub synthetic_pattern: String,
    pub synthetic_projection_profile: String,
    pub camera_id: String,
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
            synthetic_pattern: "diagnostic-grid".to_string(),
            synthetic_projection_profile: "head-anchored-virtual-camera".to_string(),
            camera_id: String::new(),
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
