//! PipeWire 设备枚举。
//!
//! 与 `audio/wasapi/` 保持同样的语义：
//! - 系统音频（`DeviceDirection::Render`）→ 每个 `Audio/Sink` 节点一条 `is_loopback = true`
//!   记录，采集的是该 sink 的 monitor 端口；
//! - 麦克风（`DeviceDirection::Capture`）→ 每个 `Audio/Source` 节点一条 `is_loopback = false`
//!   记录。
//!
//! `AudioDevice::id` 由 `node.name` 做稳定哈希得到：PipeWire 的全局 id 每次重连都会变化，
//! 而 `node.name` 由 ALSA/BlueZ 等后端持久化，可以安全地跨重启保存。

use std::cell::RefCell;
use std::rc::Rc;

use pipewire as pw;
use pw::metadata::Metadata;
use pw::properties::PropertiesBox;
use pw::registry::GlobalObject;
use pw::spa::utils::dict::DictRef;
use pw::types::ObjectType;

use super::session::{self, Session};
use super::DeviceDirection;
use crate::audio::{AudioError, CaptureSource};
use crate::models::AudioDevice;

/// 取不到 PipeWire 图速率时的回退值（PipeWire 的默认时钟设置）。
const FALLBACK_SAMPLE_RATE: u32 = 48_000;
const FALLBACK_CHANNELS: u32 = 2;

/// "跟随系统默认设备"时上报的合成设备 id。
pub(crate) const DEFAULT_DEVICE_ID: i64 = 0;

#[derive(Clone)]
struct NodeEntry {
    node_name: String,
    label: String,
    is_sink: bool,
    channels: u32,
}

/// 解析后的采集目标。
pub(crate) struct ResolvedDevice {
    /// `None` 表示不指定目标对象，交由 PipeWire 选择默认设备。
    pub(crate) node_name: Option<String>,
    pub(crate) device: AudioDevice,
}

#[derive(Default)]
struct Snapshot {
    nodes: Vec<NodeEntry>,
    default_sink: Option<String>,
    default_source: Option<String>,
    graph_rate: Option<u32>,
    graph_channels: Option<u32>,
}

impl Snapshot {
    fn sample_rate(&self) -> u32 {
        self.graph_rate.unwrap_or(FALLBACK_SAMPLE_RATE)
    }

    fn default_name(&self, is_sink: bool) -> Option<&str> {
        if is_sink {
            self.default_sink.as_deref()
        } else {
            self.default_source.as_deref()
        }
    }
}

/// 枚举所有可采集设备：系统音频（sink 的 monitor）在前，麦克风在后。
pub(crate) fn list() -> Result<Vec<AudioDevice>, AudioError> {
    let snapshot = collect()?;
    let mut devices: Vec<AudioDevice> = snapshot
        .nodes
        .iter()
        .map(|node| AudioDevice {
            id: device_id(&node.node_name),
            name: node.label.clone(),
            is_default: snapshot.default_name(node.is_sink) == Some(node.node_name.as_str()),
            is_loopback: node.is_sink,
            sample_rate: snapshot.sample_rate(),
            channels: node.channels.max(1),
        })
        .collect();
    devices.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.is_loopback.cmp(&right.is_loopback))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(devices)
}

/// 把设备 id 解析回 PipeWire 节点名。
pub(crate) fn resolve_node_name(id: i64, source: CaptureSource) -> Result<String, AudioError> {
    let want_sink = source == CaptureSource::Speaker;
    collect()?
        .nodes
        .iter()
        .find(|node| node.is_sink == want_sink && device_id(&node.node_name) == id)
        .map(|node| node.node_name.clone())
        .ok_or_else(|| device_unavailable(want_sink, false))
}

/// 设备消失时的错误：与 `audio.rs` 的 `validate_device_id` 保持同样的文案与错误码。
fn device_unavailable(want_sink: bool, retryable: bool) -> AudioError {
    let label = if want_sink {
        "system output"
    } else {
        "microphone"
    };
    let message = format!("The selected {label} device is no longer available");
    if retryable {
        AudioError::retryable_with_code("audio.device_unavailable", message)
    } else {
        AudioError::with_code("audio.device_unavailable", message)
    }
}

/// 解析采集目标：`endpoint` 为 `None` 时跟随系统默认设备。
pub(crate) fn resolve_target(
    endpoint: Option<&str>,
    direction: DeviceDirection,
) -> Result<ResolvedDevice, AudioError> {
    let snapshot = collect()?;
    let want_sink = direction == DeviceDirection::Render;
    let node = match endpoint {
        Some(name) => snapshot
            .nodes
            .iter()
            .find(|node| node.is_sink == want_sink && node.node_name == name),
        None => snapshot.default_name(want_sink).and_then(|name| {
            snapshot
                .nodes
                .iter()
                .find(|node| node.is_sink == want_sink && node.node_name == name)
        }),
    };

    if let Some(node) = node {
        return Ok(ResolvedDevice {
            node_name: Some(node.node_name.clone()),
            device: AudioDevice {
                id: device_id(&node.node_name),
                name: node.label.clone(),
                is_default: snapshot.default_name(node.is_sink) == Some(node.node_name.as_str()),
                is_loopback: node.is_sink,
                sample_rate: snapshot.sample_rate(),
                channels: node.channels.max(1),
            },
        });
    }

    if endpoint.is_some() {
        return Err(device_unavailable(want_sink, true));
    }

    // 连 `default` metadata 都取不到时，让 PipeWire 自己挑默认设备。
    Ok(ResolvedDevice {
        node_name: None,
        device: AudioDevice {
            id: DEFAULT_DEVICE_ID,
            name: "System default".to_string(),
            is_default: true,
            is_loopback: want_sink,
            sample_rate: snapshot.sample_rate(),
            channels: snapshot.graph_channels.unwrap_or(FALLBACK_CHANNELS),
        },
    })
}

/// 按进程名查找 pid；VRChat 在 Proton 下通常以 `VRChat.exe` 作为进程名出现。
pub(crate) fn find_process_id(name: &str) -> Result<Option<u32>, AudioError> {
    let needle = name.trim().trim_end_matches(".exe").to_ascii_lowercase();
    if needle.is_empty() {
        return Ok(None);
    }
    let entries = std::fs::read_dir("/proc").map_err(|error| {
        AudioError::with_code(
            "audio.unavailable",
            format!("Failed to enumerate processes: {error}"),
        )
    })?;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        if comm.trim().to_ascii_lowercase().contains(&needle) {
            return Ok(Some(pid));
        }
        // `comm` 被内核截断到 15 个字符，带路径启动或名字较长的进程只能靠 `cmdline` 匹配。
        let cmdline = std::fs::read_to_string(entry.path().join("cmdline")).unwrap_or_default();
        let executable = cmdline.split('\0').next().unwrap_or_default();
        let file_name = executable.rsplit('/').next().unwrap_or_default();
        if file_name.to_ascii_lowercase().contains(&needle) {
            return Ok(Some(pid));
        }
    }
    Ok(None)
}

/// 读取一次 PipeWire 图快照：节点列表、默认设备，以及图的采样率与声道数。
fn collect() -> Result<Snapshot, AudioError> {
    let session = Session::connect()?;
    let registry = session
        .core
        .get_registry_rc()
        .map_err(|error| session::call_failed("registry lookup", &error))?;

    let snapshot = Rc::new(RefCell::new(Snapshot::default()));
    let metadata_objects: Rc<RefCell<Vec<GlobalObject<PropertiesBox>>>> =
        Rc::new(RefCell::new(Vec::new()));

    let _registry_listener = registry
        .add_listener_local()
        .global({
            let snapshot = Rc::clone(&snapshot);
            let metadata_objects = Rc::clone(&metadata_objects);
            move |object: &GlobalObject<&DictRef>| match object.type_ {
                ObjectType::Node => {
                    if let Some(node) = node_entry(object) {
                        snapshot.borrow_mut().nodes.push(node);
                    }
                }
                ObjectType::Metadata => {
                    let name = object
                        .props
                        .and_then(|props| props.get("metadata.name"))
                        .unwrap_or_default();
                    if name == "default" || name == "settings" {
                        metadata_objects.borrow_mut().push(object.to_owned());
                    }
                }
                _ => {}
            }
        })
        .register();
    session::roundtrip(&session)?;

    // 第二趟：绑定 metadata，读取默认设备与图时钟设置。
    let mut _metadata_bindings = Vec::new();
    for object in std::mem::take(&mut *metadata_objects.borrow_mut()) {
        let metadata: Metadata = registry
            .bind(&object)
            .map_err(|error| session::call_failed("metadata binding", &error))?;
        let is_default_object = object
            .props
            .as_ref()
            .and_then(|props| props.get("metadata.name"))
            .is_some_and(|name| name == "default");
        let snapshot = Rc::clone(&snapshot);
        let listener = metadata
            .add_listener_local()
            .property(move |_subject, key, _type, value| {
                let (Some(key), Some(value)) = (key, value) else {
                    return 0;
                };
                let mut snapshot = snapshot.borrow_mut();
                match (is_default_object, key) {
                    (true, "default.audio.sink") => snapshot.default_sink = json_name(value),
                    (true, "default.audio.source") => snapshot.default_source = json_name(value),
                    (false, "default.clock.rate") => {
                        snapshot.graph_rate = value.trim().parse().ok();
                    }
                    (false, "default.clock.channels") => {
                        snapshot.graph_channels = value.trim().parse().ok();
                    }
                    _ => {}
                }
                0
            })
            .register();
        _metadata_bindings.push((metadata, listener));
    }
    session::roundtrip(&session)?;

    let collected = snapshot.borrow();
    Ok(Snapshot {
        nodes: collected.nodes.clone(),
        default_sink: collected.default_sink.clone(),
        default_source: collected.default_source.clone(),
        graph_rate: collected.graph_rate,
        graph_channels: collected.graph_channels,
    })
}

fn node_entry(object: &GlobalObject<&DictRef>) -> Option<NodeEntry> {
    let props = object.props?;
    let is_sink = match props.get("media.class")? {
        "Audio/Sink" => true,
        "Audio/Source" => false,
        _ => return None,
    };
    let node_name = props.get("node.name")?.to_string();
    let label = props
        .get("node.description")
        .unwrap_or(node_name.as_str())
        .to_string();
    let channels = props
        .get("audio.channels")
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(FALLBACK_CHANNELS);
    Some(NodeEntry {
        node_name,
        label,
        is_sink,
        channels,
    })
}

/// `default.audio.sink` 的值形如 `{"name":"alsa_output.pci-0000_01_00.1.hdmi-stereo"}`。
fn json_name(value: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

/// 由节点名得到稳定的设备 id（FNV-1a 64 位，取正数）。
fn device_id(node_name: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in node_name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let id = (hash & 0x7fff_ffff_ffff_ffff) as i64;
    if id == DEFAULT_DEVICE_ID {
        1
    } else {
        id
    }
}
