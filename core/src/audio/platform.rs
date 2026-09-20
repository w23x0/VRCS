use super::{AudioDevice, AudioError};

#[derive(Clone)]
pub(crate) enum CaptureTarget {
    Process(u32),
    Device {
        wasapi_id: Option<String>,
        direction: DeviceDirection,
    },
}

impl CaptureTarget {
    /// 各平台统一的构造入口：`endpoint` 是后端自己的设备标识。
    pub(crate) fn device(endpoint: Option<String>, direction: DeviceDirection) -> Self {
        Self::Device {
            wasapi_id: endpoint,
            direction,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DeviceDirection {
    Render,
    Capture,
}

pub(crate) fn list_devices() -> Result<Vec<AudioDevice>, AudioError> {
    Err(AudioError::with_code(
        "audio.unsupported_platform",
        "Audio capture is not implemented on this platform",
    ))
}

pub(crate) fn resolve_device_id(
    _id: i64,
    _source: super::CaptureSource,
) -> Result<String, AudioError> {
    Err(AudioError::with_code(
        "audio.unsupported_platform",
        "Audio capture is not implemented on this platform",
    ))
}

pub(crate) fn find_process_id(_name: &str) -> Result<Option<u32>, AudioError> {
    Err(AudioError::with_code(
        "audio.unsupported_platform",
        "Audio capture is not implemented on this platform",
    ))
}

pub(crate) fn capture_main(
    _target: CaptureTarget,
    _rate: u32,
    _stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _tx: tokio::sync::mpsc::Sender<Vec<f32>>,
    ready: std::sync::mpsc::Sender<Result<AudioDevice, AudioError>>,
) {
    let _ = ready.send(Err(AudioError::with_code(
        "audio.unsupported_platform",
        "Audio capture is not implemented on this platform",
    )));
}
