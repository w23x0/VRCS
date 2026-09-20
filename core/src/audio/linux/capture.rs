//! PipeWire 采集流：把设备（sink 的 monitor / source）或指定进程的音频转成
//! 16 kHz 单声道 f32 分块。
//!
//! 设备路径用 `AUTOCONNECT` 让 WirePlumber 接线；按进程采集则**不自动接线**，
//! 而是把目标进程输出流的端口手动连到我们的输入端口（tap）。这样做不改动目标
//! 进程的路由，因此也就不需要在停止时恢复任何东西，语义与 Windows 的进程回环一致。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender as SyncSender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::loop_::Timeout;
use pw::properties::{properties, PropertiesBox};
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::param::format::{MediaSubtype, MediaType};
use pw::spa::param::{format_utils, ParamType};
use pw::spa::pod::Pod;
use pw::spa::utils::Direction;
use pw::stream::{Stream, StreamFlags, StreamRc, StreamState};
use tokio::sync::mpsc;

use super::devices::{self, ResolvedDevice};
use super::graph;
use super::session::{self, Session, ITERATE_TIMEOUT, ROUNDTRIP_BUDGET};
use super::{AudioDevice, AudioError, CaptureTarget, DeviceDirection};
use crate::audio::CHUNK_FRAMES;

const STREAM_NAME: &str = "vrcs-capture";
/// 协商失败时最多重新提交几次固定格式（PipeWire 需要一次往返来插入转换器）。
const MAX_FORMAT_ATTEMPTS: u32 = 3;
/// 按进程采集时重新扫描目标进程新出现的输出流的间隔。
const TAP_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
/// 按进程采集时上报的合成设备 id：不占用 0（"跟随系统默认"）也不与真实设备哈希冲突。
const APPLICATION_DEVICE_ID: i64 = -1;
const APPLICATION_DEVICE_LABEL: &str = "Application audio";

type DeviceReport = Result<AudioDevice, AudioError>;
type ReadySlot = Rc<RefCell<Option<SyncSender<DeviceReport>>>>;

/// `ready` 只在启动阶段使用一次；take-and-send 保证既不会重复上报，也不会漏报。
#[derive(Clone)]
struct ReadySignal(ReadySlot);

impl ReadySignal {
    fn new(sender: SyncSender<DeviceReport>) -> Self {
        Self(Rc::new(RefCell::new(Some(sender))))
    }

    fn succeed(&self, device: &AudioDevice) {
        if let Some(sender) = self.0.borrow_mut().take() {
            let _ = sender.send(Ok(device.clone()));
        }
    }

    fn fail(&self, error: AudioError) {
        if let Some(sender) = self.0.borrow_mut().take() {
            let _ = sender.send(Err(error));
        }
    }
}

/// 启动握手：格式已被接受 + 流已进入可接线状态 → 上报设备。
#[derive(Clone)]
struct Startup {
    format_checked: Rc<Cell<bool>>,
    format_accepted: Rc<Cell<bool>>,
    paused: Rc<Cell<bool>>,
}

/// 采集目标（已从平台无关的 `CaptureTarget` 拆开）。
enum CapturePlan {
    Device {
        endpoint: Option<String>,
        direction: DeviceDirection,
    },
    Process {
        pid: u32,
    },
}

/// 建流所需的一切。
struct Prepared {
    device: AudioDevice,
    props: PropertiesBox,
    autoconnect: bool,
    tap_pid: Option<u32>,
}

/// 采集回调的共享可变状态；所有回调都在同一个主循环线程上，无需加锁。
struct CaptureState {
    format: AudioInfoRaw,
    frames: Vec<f32>,
    tx: mpsc::Sender<Vec<f32>>,
}

/// 采集线程入口。
pub(crate) fn run(
    target: CaptureTarget,
    rate: u32,
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Vec<f32>>,
    ready: SyncSender<Result<AudioDevice, AudioError>>,
) {
    let ready = ReadySignal::new(ready);
    let plan = match target {
        CaptureTarget::Process(pid) => CapturePlan::Process { pid },
        CaptureTarget::Device {
            endpoint,
            direction,
        } => CapturePlan::Device {
            endpoint,
            direction,
        },
    };
    if let Err(error) = capture(&plan, rate, &stop, &tx, &ready) {
        ready.fail(error);
    }
}

fn base_props() -> PropertiesBox {
    properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::NODE_NAME => STREAM_NAME,
    }
}

fn prepare(plan: &CapturePlan, rate: u32) -> Result<Prepared, AudioError> {
    match plan {
        CapturePlan::Device {
            endpoint,
            direction,
        } => {
            let ResolvedDevice { node_name, device } =
                devices::resolve_target(endpoint.as_deref(), *direction)?;
            let mut props = base_props();
            if *direction == DeviceDirection::Render {
                // 录制 sink 的 monitor 端口，而不是默认的 source 端口。
                props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
            }
            if let Some(node_name) = node_name.as_deref() {
                props.insert(*pw::keys::TARGET_OBJECT, node_name);
            }
            Ok(Prepared {
                device,
                props,
                autoconnect: true,
                tap_pid: None,
            })
        }
        CapturePlan::Process { pid } => Ok(Prepared {
            device: AudioDevice {
                id: APPLICATION_DEVICE_ID,
                name: APPLICATION_DEVICE_LABEL.to_string(),
                is_default: false,
                is_loopback: true,
                sample_rate: rate,
                channels: 1,
            },
            props: base_props(),
            autoconnect: false,
            tap_pid: Some(*pid),
        }),
    }
}

fn capture(
    plan: &CapturePlan,
    rate: u32,
    stop: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Vec<f32>>,
    ready: &ReadySignal,
) -> Result<(), AudioError> {
    let Prepared {
        device,
        props,
        autoconnect,
        tap_pid,
    } = prepare(plan, rate)?;
    let session = Session::connect()?;

    let stream = StreamRc::new(session.core.clone(), STREAM_NAME, props)
        .map_err(|error| session::call_failed("stream creation", &error))?;

    // 固定格式：PipeWire 会在图里插入转换器/重采样器，设备原生格式无需关心。
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    audio_info.set_rate(rate);
    audio_info.set_channels(1);
    let format_object = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let format_bytes: Rc<Vec<u8>> = Rc::new(
        pw::spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &pw::spa::pod::Value::Object(format_object),
        )
        .map_err(|error| {
            AudioError::with_code(
                "audio.unavailable",
                format!("Failed to serialize the PipeWire format parameter: {error}"),
            )
        })?
        .0
        .into_inner(),
    );

    let startup = Startup {
        format_checked: Rc::new(Cell::new(false)),
        format_accepted: Rc::new(Cell::new(false)),
        paused: Rc::new(Cell::new(false)),
    };
    let attempts = Rc::new(Cell::new(0_u32));
    let failure: Rc<RefCell<Option<AudioError>>> = Rc::new(RefCell::new(None));

    let state = CaptureState {
        format: AudioInfoRaw::new(),
        frames: Vec::with_capacity(CHUNK_FRAMES * 4),
        tx: tx.clone(),
    };

    let _listener = stream
        .add_local_listener_with_user_data(state)
        .param_changed({
            let format_bytes = Rc::clone(&format_bytes);
            let startup = startup.clone();
            let attempts = Rc::clone(&attempts);
            let failure = Rc::clone(&failure);
            move |stream, state, id, param| {
                let Some(param) = param else {
                    return;
                };
                if id != ParamType::Format.as_raw() {
                    return;
                }
                let Ok((media_type, media_subtype)) = format_utils::parse_format(param) else {
                    return;
                };
                if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                    *failure.borrow_mut() = Some(AudioError::with_code(
                        "audio.unsupported_format",
                        "The selected audio source does not provide raw audio",
                    ));
                    return;
                }
                if state.format.parse(param).is_err() {
                    return;
                }
                startup.format_checked.set(true);
                if state.format.format() == AudioFormat::F32LE
                    && state.format.rate() == rate
                    && state.format.channels() == 1
                {
                    startup.format_accepted.set(true);
                    return;
                }
                // 服务器没接受固定格式：重新提交一次，让它插入转换器。
                if attempts.get() >= MAX_FORMAT_ATTEMPTS {
                    *failure.borrow_mut() = Some(AudioError::with_code(
                        "audio.unsupported_format",
                        format!(
                            "PipeWire negotiated an unsupported capture format: {} Hz, {} channel(s)",
                            state.format.rate(),
                            state.format.channels()
                        ),
                    ));
                    return;
                }
                attempts.set(attempts.get() + 1);
                if let Some(pod) = Pod::from_bytes(&format_bytes) {
                    let _ = stream.update_params(&mut [pod]);
                }
            }
        })
        .state_changed({
            let startup = startup.clone();
            let failure = Rc::clone(&failure);
            move |_stream, _state, _old, new| match new {
                StreamState::Paused => startup.paused.set(true),
                StreamState::Streaming => {
                    // 解析到过格式、却没被接受：如实失败，避免按错误的采样率喂给管线
                    // （从未协商过格式的流不在此列，例如尚未接线的按进程采集）。
                    if startup.format_checked.get()
                        && !startup.format_accepted.get()
                        && failure.borrow().is_none()
                    {
                        *failure.borrow_mut() = Some(AudioError::with_code(
                            "audio.unsupported_format",
                            "PipeWire did not accept the requested capture format",
                        ));
                    }
                }
                StreamState::Error(message) => {
                    *failure.borrow_mut() = Some(AudioError::with_code(
                        "audio.device_in_use",
                        format!("PipeWire capture stopped: {message}"),
                    ));
                }
                _ => {}
            }
        })
        .process(capture_frames)
        .register()
        .map_err(|error| session::call_failed("stream listener", &error))?;

    let mut params = [Pod::from_bytes(&format_bytes)
        .ok_or_else(|| AudioError::with_code("audio.unavailable", "Invalid capture format"))?];
    let mut flags = StreamFlags::MAP_BUFFERS;
    if autoconnect {
        flags |= StreamFlags::AUTOCONNECT;
    }
    stream
        .connect(Direction::Input, None, flags, &mut params)
        .map_err(|error| session::call_failed("stream connect", &error))?;
    // 节点 id 只有在 server 绑定之后才有效，必须等 connect 之后再读。
    let mut own_node = valid_node_id(&stream);

    let loop_ = session.main_loop.loop_();
    let deadline = Instant::now() + ROUNDTRIP_BUDGET;
    let mut activated = false;
    // 目标流节点 id → 我们创建的链路代理；替换条目即断开旧链路。
    let mut taps: HashMap<u32, pw::link::Link> = HashMap::new();
    let mut own_input: Option<u32> = None;
    // 初值取"刚刚过期"，让第一次接线立刻发生；之后按间隔扫描新出现的流。
    let mut last_tap = Instant::now() - TAP_REFRESH_INTERVAL;

    while !stop.load(Ordering::Relaxed) {
        if let Some(error) = failure.borrow_mut().take() {
            let _ = stream.disconnect();
            return Err(error);
        }

        if let Some(pid) = tap_pid {
            // 先等自己的输入端口出现，接上目标进程的输出流，再激活。
            if own_input.is_none() {
                if own_node.is_none() {
                    own_node = valid_node_id(&stream);
                }
                own_input = own_node.and_then(|node| input_port_of(&session, node));
            }
            if let (Some(node), Some(destination)) = (own_node, own_input) {
                if last_tap.elapsed() >= TAP_REFRESH_INTERVAL {
                    tap_target_streams(&session, pid, node, destination, &mut taps)?;
                    last_tap = Instant::now();
                }
            }
        }

        if !activated && startup_ready(tap_pid, &startup, own_input) {
            stream
                .set_active(true)
                .map_err(|error| session::call_failed("stream activation", &error))?;
            activated = true;
            ready.succeed(&device);
        }

        if !activated && Instant::now() >= deadline {
            let _ = stream.disconnect();
            return Err(AudioError::with_code(
                "audio.start_timeout",
                "Timed out while starting PipeWire capture",
            ));
        }

        loop_.iterate(Timeout::Finite(ITERATE_TIMEOUT));
    }

    // 链路随本函数结束一起释放：丢弃代理即断开 tap，目标进程的路由从未被改动过。
    drop(taps);
    let _ = stream.disconnect();
    Ok(())
}

/// 设备路径沿用"流已暂停且格式约定完成"；按进程采集只要端口就绪即可（此时可能还没有对端）。
fn startup_ready(tap_pid: Option<u32>, startup: &Startup, own_input: Option<u32>) -> bool {
    if tap_pid.is_some() {
        return own_input.is_some();
    }
    startup.paused.get() && startup.format_accepted.get()
}

/// 流节点 id 在 server 绑定前是无效值（0 / SPA_ID_INVALID）。
fn valid_node_id(stream: &StreamRc) -> Option<u32> {
    let id = stream.node_id();
    (id != 0 && id != u32::MAX).then_some(id)
}

fn input_port_of(session: &Session, node_id: u32) -> Option<u32> {
    let snapshot = graph::snapshot(session).ok()?;
    snapshot.ports_of(node_id, "in").first().map(|port| port.id)
}

/// 把 `pid` 名下所有音频输出流的首个输出端口接到我们的输入端口上。
///
/// 每个输出流只取第一个端口（`port.name` 排序后即 FL）：目标流的立体声内容在
/// 语音场景下左右一致，取单声道既避免了混音带来的电平差异，也复用了设备路径
/// 已验证的单声道链路。新出现的流会在后续刷新里被接上，因此 VRChat 重启音频
/// 或新建流时无需重开采集。
fn tap_target_streams(
    session: &Session,
    pid: u32,
    own_node: u32,
    destination_port: u32,
    taps: &mut HashMap<u32, pw::link::Link>,
) -> Result<(), AudioError> {
    let snapshot = graph::snapshot(session)?;
    let mut created = false;
    for node in snapshot.output_streams_of(pid) {
        // 接好了且链路仍在就跳过；链路掉了（源节点重建、session manager 重排）则重接。
        let linked = snapshot
            .links()
            .iter()
            .any(|link| link.output_node == node.id && link.input_node == own_node);
        if linked && taps.contains_key(&node.id) {
            continue;
        }
        let Some(source_port) = snapshot
            .ports_of(node.id, "out")
            .first()
            .map(|port| port.id)
        else {
            continue;
        };
        let props = properties! {
            "link.output.port" => source_port.to_string(),
            "link.input.port" => destination_port.to_string(),
        };
        let link = session
            .core
            .create_object::<pw::link::Link>("link-factory", &props)
            .map_err(|error| {
                AudioError::with_code(
                    "audio.process_loopback_unavailable",
                    format!("Failed to tap the application audio stream: {error}"),
                )
            })?;
        // 覆盖旧代理会释放上一条链路。
        taps.insert(node.id, link);
        created = true;
    }
    if created {
        // `create_object` 只是把请求排进客户端缓冲区，必须迭代主循环才会真正生效。
        session::roundtrip(session)?;
    }
    Ok(())
}

/// 把一块 PipeWire buffer 转成单声道 f32，并切成 `CHUNK_FRAMES` 的分块。
fn capture_frames(stream: &Stream, state: &mut CaptureState) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let channels = state.format.channels().max(1) as usize;
    let mono = {
        let datas = buffer.datas_mut();
        let Some(data) = datas.first_mut() else {
            return;
        };
        let expected = data.chunk().size() as usize;
        let Some(raw) = data.data() else {
            return;
        };
        let usable = expected.min(raw.len());
        let mut mono = Vec::with_capacity(usable / (channels * 4).max(1) + 1);
        for frame in raw[..usable].chunks_exact(channels * 4) {
            let mut sum = 0.0_f32;
            for sample in frame.as_chunks::<4>().0 {
                sum += f32::from_le_bytes(*sample);
            }
            mono.push(sum / channels as f32);
        }
        mono
    };

    state.frames.extend_from_slice(&mono);
    while state.frames.len() >= CHUNK_FRAMES {
        let chunk: Vec<f32> = state.frames.drain(..CHUNK_FRAMES).collect();
        if state.tx.try_send(chunk).is_err() {
            tracing::debug!("dropping a captured audio chunk because the pipeline is behind");
        }
    }
}
