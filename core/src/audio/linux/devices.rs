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
use std::path::Path;
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

    /// `default.audio.*` 指向的节点的声道数；节点不在快照里时回退到 `FALLBACK_CHANNELS`。
    fn default_channels(&self, is_sink: bool) -> u32 {
        self.default_name(is_sink)
            .and_then(|name| self.nodes.iter().find(|node| node.node_name == name))
            .map(|node| node.channels.max(1))
            .unwrap_or(FALLBACK_CHANNELS)
    }

    /// 应用一条 metadata 属性；`from_default` 为真表示它来自 `default` metadata，
    /// 否则来自 `settings`。
    fn apply_metadata(&mut self, from_default: bool, key: &str, value: &str) {
        match (from_default, key) {
            (true, "default.audio.sink") => self.default_sink = json_name(value),
            (true, "default.audio.source") => self.default_source = json_name(value),
            // `clock.rate` 是 settings metadata 里真实的图速率键；`default.clock.rate`
            // 只是配置文件（`pipewire.conf` 的 `context.properties`）里的名字，
            // 仅在少数系统/WirePlumber 版本上被显式写入，因此只作为兼容回退。
            (false, "clock.rate") => {
                if let Ok(rate) = value.trim().parse() {
                    self.graph_rate = Some(rate);
                }
            }
            (false, "default.clock.rate") if self.graph_rate.is_none() => {
                self.graph_rate = value.trim().parse().ok();
            }
            _ => {}
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
            channels: snapshot.default_channels(want_sink),
        },
    })
}

/// 按进程名查找 pid；VRChat 在 Proton 下通常以 `VRChat.exe` 作为进程名出现。
pub(crate) fn find_process_id(name: &str) -> Result<Option<u32>, AudioError> {
    find_process_id_in(Path::new("/proc"), name)
}

/// 在给定的 procfs 根目录下查找 pid。
///
/// `/proc` 的遍历顺序是任意的，而 `contains` 会命中名字里只是"包含"目标名的辅助进程
/// （例如 `vrchat-helper`），所以这里扫描完整棵树、按匹配质量取名次最优的那个，
/// 而不是返回第一个命中项；名次相同的候选保留先遇到的那个。
fn find_process_id_in(root: &Path, name: &str) -> Result<Option<u32>, AudioError> {
    let needle = name.trim().trim_end_matches(".exe").to_ascii_lowercase();
    if needle.is_empty() {
        return Ok(None);
    }
    let entries = std::fs::read_dir(root).map_err(|error| {
        AudioError::with_code(
            "audio.unavailable",
            format!("Failed to enumerate processes: {error}"),
        )
    })?;
    let mut best: Option<(u8, u32)> = None;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        // 名次 1 已经是最优解，再往后扫描不可能更好。
        if matches!(best, Some((1, _))) {
            break;
        }
        let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        // `comm` 被内核截断到 15 个字符，带路径启动或名字较长的进程只能靠 `cmdline` 匹配。
        let cmdline = std::fs::read_to_string(entry.path().join("cmdline")).unwrap_or_default();
        let executable = cmdline.split('\0').next().unwrap_or_default();
        let Some(rank) = process_rank(&comm, executable, &needle) else {
            continue;
        };
        if best.is_none_or(|(best_rank, _)| rank < best_rank) {
            best = Some((rank, pid));
        }
    }
    Ok(best.map(|(_, pid)| pid))
}

/// 进程名匹配质量的名次，1 最好、4 最差；`None` 表示不匹配。
///
/// 1. `comm` 完全等于目标名（可带 `.exe`）；
/// 2. 可执行文件名（`cmdline` 的 argv[0] 去掉路径）完全等于目标名；
/// 3. `comm` 包含目标名；
/// 4. 可执行文件名包含目标名。
fn process_rank(comm: &str, executable: &str, needle: &str) -> Option<u8> {
    let comm = comm.trim().to_ascii_lowercase();
    let executable = executable
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if is_same_process_name(&comm, needle) {
        return Some(1);
    }
    if is_same_process_name(&executable, needle) {
        return Some(2);
    }
    if comm.contains(needle) {
        return Some(3);
    }
    if executable.contains(needle) {
        return Some(4);
    }
    None
}

/// 判断 `value` 是否就是目标进程名；`needle` 已去掉 `.exe` 后缀并转成小写。
fn is_same_process_name(value: &str, needle: &str) -> bool {
    value == needle || value.strip_suffix(".exe") == Some(needle)
}

/// 读取一次 PipeWire 图快照：节点列表、默认设备，以及图的采样率。
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
                snapshot
                    .borrow_mut()
                    .apply_metadata(is_default_object, key, value);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 建一个干净的临时目录，当作可注入的 procfs 根。
    fn scratch_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("vrcs-devices-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch root");
        root
    }

    /// 在合成的 procfs 里放一个进程：`comm` 与 `cmdline`（argv[0]）。
    fn fake_process(root: &Path, pid: u32, comm: &str, argv0: &str) {
        let dir = root.join(pid.to_string());
        std::fs::create_dir_all(&dir).expect("create process dir");
        std::fs::write(dir.join("comm"), format!("{comm}\n")).expect("write comm");
        std::fs::write(dir.join("cmdline"), format!("{argv0}\0")).expect("write cmdline");
    }

    #[test]
    fn exact_process_name_beats_a_containing_helper() {
        // 弱匹配的 pid 更小：只看第一个 `contains` 命中的实现会挑中 helper。
        let weak_first = scratch_root("rank-weak-first");
        fake_process(&weak_first, 100, "vrchat-helper", "/usr/bin/vrchat-helper");
        fake_process(&weak_first, 200, "VRChat.exe", "/opt/VRChat.exe");
        assert_eq!(
            find_process_id_in(&weak_first, "VRChat.exe").expect("lookup"),
            Some(200)
        );

        // 目录顺序反过来（强匹配的 pid 更小）同样要挑强匹配。
        let strong_first = scratch_root("rank-strong-first");
        fake_process(&strong_first, 100, "VRChat.exe", "/opt/VRChat.exe");
        fake_process(
            &strong_first,
            200,
            "vrchat-helper",
            "/usr/bin/vrchat-helper",
        );
        assert_eq!(
            find_process_id_in(&strong_first, "VRChat.exe").expect("lookup"),
            Some(100)
        );
    }

    #[test]
    fn exact_comm_beats_a_matching_executable_path() {
        let root = scratch_root("rank-comm");
        // 名次 3（comm 包含）vs 名次 2（argv[0] 文件名完全相等）。
        fake_process(&root, 10, "vrchat-helper", "/usr/bin/python3");
        fake_process(&root, 20, "python3", "/opt/vrchat/VRChat.exe");
        assert_eq!(
            find_process_id_in(&root, "VRChat.exe").expect("lookup"),
            Some(20)
        );
    }

    #[test]
    fn long_executable_names_match_through_argv0_only() {
        let root = scratch_root("rank-argv0");
        // `comm` 被截断到 15 个字符，只有 argv[0] 的文件名里含目标名。
        fake_process(
            &root,
            7,
            "wine64-preloader",
            "/opt/vrcs_core-long-name-helper",
        );
        fake_process(&root, 8, "bash", "/usr/bin/bash");
        assert_eq!(
            find_process_id_in(&root, "vrcs_core-long-name").expect("lookup"),
            Some(7)
        );
    }

    #[test]
    fn blank_name_is_none_and_a_missing_root_is_an_error() {
        let root = scratch_root("blank");
        assert_eq!(
            find_process_id_in(&root, "   ").expect("a blank name is not an error"),
            None
        );
        assert_eq!(
            find_process_id_in(&root, "VRChat.exe").expect("an empty tree is not an error"),
            None
        );

        let missing = root.join("no-such-procfs");
        let error =
            find_process_id_in(&missing, "VRChat.exe").expect_err("a missing root must fail");
        assert_eq!(error.code(), "audio.unavailable");
    }

    /// `key:'…' value:'…'` → `(key, value)`，跳过 `pw-metadata` 的其它输出行。
    fn metadata_entries(output: &str) -> impl Iterator<Item = (&str, &str)> {
        output.lines().filter_map(|line| {
            let key = line.split("key:'").nth(1)?.split('\'').next()?;
            let value = line.split("value:'").nth(1)?.split('\'').next()?;
            Some((key, value))
        })
    }

    /// 本机 settings metadata 里的图速率：`clock.rate` 优先，其次 `default.clock.rate`。
    fn settings_graph_rate() -> Option<u32> {
        let output = std::process::Command::new("pw-metadata")
            .args(["-n", "settings"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut primary = None;
        let mut legacy = None;
        for (key, value) in metadata_entries(&text) {
            match key {
                "clock.rate" => primary = value.trim().parse().ok(),
                "default.clock.rate" => legacy = value.trim().parse().ok(),
                _ => {}
            }
        }
        primary.or(legacy)
    }

    /// settings metadata 的速率键映射：`clock.rate` 是真实键，`default.clock.rate` 只是兼容回退。
    #[test]
    fn settings_metadata_maps_the_clock_rate_key() {
        // 只有声道/日志键时不该出现"猜出来的"速率，只能是回退值。
        let mut snapshot = Snapshot::default();
        snapshot.apply_metadata(false, "default.clock.channels", "2");
        snapshot.apply_metadata(false, "log.level", "2");
        snapshot.apply_metadata(true, "default.audio.sink", r#"{"name":"hdmi-stereo"}"#);
        assert_eq!(snapshot.sample_rate(), FALLBACK_SAMPLE_RATE);

        // 真实键。
        snapshot.apply_metadata(false, "clock.rate", "44100");
        assert_eq!(snapshot.sample_rate(), 44_100);

        // 少数系统/WirePlumber 版本在 settings 里写配置文件名字。
        let mut legacy = Snapshot::default();
        legacy.apply_metadata(false, "default.clock.rate", "32000");
        assert_eq!(legacy.sample_rate(), 32_000);

        // 两者都在时以 `clock.rate` 为准，与属性到达顺序无关。
        let mut legacy_first = Snapshot::default();
        legacy_first.apply_metadata(false, "default.clock.rate", "32000");
        legacy_first.apply_metadata(false, "clock.rate", "44100");
        assert_eq!(legacy_first.sample_rate(), 44_100);
        let mut primary_first = Snapshot::default();
        primary_first.apply_metadata(false, "clock.rate", "44100");
        primary_first.apply_metadata(false, "default.clock.rate", "32000");
        assert_eq!(primary_first.sample_rate(), 44_100);
    }

    /// 合成默认设备的声道数取自 `default.audio.*` 指到的节点。
    #[test]
    fn fallback_device_channels_come_from_the_default_node() {
        let mut snapshot = Snapshot::default();
        snapshot.apply_metadata(true, "default.audio.sink", r#"{"name":"hdmi-stereo"}"#);
        snapshot.nodes.push(NodeEntry {
            node_name: "hdmi-stereo".to_string(),
            label: "HDMI".to_string(),
            is_sink: true,
            channels: 6,
        });
        assert_eq!(snapshot.default_channels(true), 6);
        // 名字不在快照里（默认设备刚好消失）时回退。
        assert_eq!(snapshot.default_channels(false), FALLBACK_CHANNELS);
    }

    /// 现场对拍：`list_devices()` 报的采样率必须等于 PipeWire 图的实际速率。
    #[test]
    fn enumerated_devices_report_the_live_graph_rate() {
        let Some(expected) = settings_graph_rate() else {
            eprintln!(
                "skipping graph rate test: pw-metadata is missing or settings has no clock rate"
            );
            return;
        };
        eprintln!("settings metadata graph rate: {expected} Hz");
        let devices = crate::audio::list_devices().expect("device enumeration");
        for device in &devices {
            eprintln!(
                "device {:?}: sample_rate={} channels={}",
                device.name, device.sample_rate, device.channels
            );
            assert_eq!(
                device.sample_rate, expected,
                "device {:?} reports {} Hz instead of the graph rate {expected} Hz",
                device.name, device.sample_rate
            );
        }
    }
}
