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
    /// 按进程采集。进程名 `name` 只有 Linux 后端用来跟随 VRChat 重启；WASAPI 的进程回环
    /// 整个采集会话都绑定在启动时解析出的那个 pid 上，因此这里刻意忽略它，运行时行为不变。
    pub(crate) fn process(pid: u32, _name: &str) -> Self {
        Self::Process(pid)
    }

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
