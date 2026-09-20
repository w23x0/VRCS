mod capture;
mod devices;
mod pcm;

pub(crate) use capture::capture_main;
pub(crate) use devices::{find_process_id, list_devices, resolve_device_id};

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
