//! Linux 采集后端：基于 PipeWire 的设备枚举与音频采集。
//!
//! 与 `audio/wasapi/` 的语义保持一致：
//! - `DeviceDirection::Render`（系统音频）→ 录制某个 sink 的 monitor 端口；
//! - `DeviceDirection::Capture`（麦克风）→ 录制某个 source 节点；
//! - `CaptureTarget::Process`（按进程隔离采集）→ 不自动接线，而是把目标进程音频
//!   输出流的端口手动接到本流的输入端口（tap），只录该进程的声音；它不改变目标
//!   进程的路由，因此停止时也不需要恢复任何东西。

mod capture;
mod devices;
mod graph;
mod session;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::{AudioDevice, AudioError, CaptureSource};

/// 采集目标。`endpoint` 是 PipeWire 的 `node.name`，`None` 表示跟随系统默认设备。
#[derive(Clone)]
pub(crate) enum CaptureTarget {
    Process(u32),
    Device {
        endpoint: Option<String>,
        direction: DeviceDirection,
    },
}

impl CaptureTarget {
    /// 各平台统一的构造入口：`endpoint` 是后端自己的设备标识。
    pub(crate) fn device(endpoint: Option<String>, direction: DeviceDirection) -> Self {
        Self::Device {
            endpoint,
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
    devices::list()
}

pub(crate) fn resolve_device_id(id: i64, source: CaptureSource) -> Result<String, AudioError> {
    devices::resolve_node_name(id, source)
}

pub(crate) fn find_process_id(name: &str) -> Result<Option<u32>, AudioError> {
    devices::find_process_id(name)
}

pub(crate) fn capture_main(
    target: CaptureTarget,
    rate: u32,
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Vec<f32>>,
    ready: std::sync::mpsc::Sender<Result<AudioDevice, AudioError>>,
) {
    capture::run(target, rate, stop, tx, ready);
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use pipewire as pw;

    use super::session::Session;
    use super::*;
    use crate::audio::{AudioCapture, CaptureSource, CHUNK_FRAMES};

    const TEST_SINK_NAME: &str = "vrcs-capture-test";
    const TEST_SINK_LABEL: &str = "VRCS capture test sink";
    /// 按进程采集的测试用独立 sink：测试并行跑，共用一个 sink 会互相污染。
    const TAP_SINK_NAME: &str = "vrcs-tap-test";
    const TAP_SINK_LABEL: &str = "VRCS tap test sink";
    /// Pulse 协议测试用独立 sink（同上，避免并行测试互相干扰）。
    const PULSE_SINK_NAME: &str = "vrcs-pulse-test";
    const PULSE_SINK_LABEL: &str = "VRCS pulse test sink";
    const TEST_RATE: u32 = 16_000;
    const TONE_HZ: f32 = 440.0;
    /// 干扰用频率：与 TONE_HZ 相隔足够远，便于用 DFT 幅度区分。
    const OTHER_TONE_HZ: f32 = 220.0;
    const TONE_AMPLITUDE: f32 = 0.4;

    /// 采集通路需要真实的 PipeWire 会话；没有会话时跳过（CI 上的默认行为）。
    fn connect_or_skip() -> Option<Session> {
        match Session::connect() {
            Ok(session) => Some(session),
            Err(error) => {
                eprintln!("skipping PipeWire capture test: {error}");
                None
            }
        }
    }

    fn pw_play_available() -> bool {
        Command::new("pw-play")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    /// 在进程内创建一个空 sink：随本进程存活，测试结束即销毁。
    fn create_test_sink(session: &Session, node_name: &str, label: &str) -> Option<pw::node::Node> {
        let properties = pw::properties::properties! {
            *pw::keys::FACTORY_NAME => "support.null-audio-sink",
            *pw::keys::NODE_NAME => node_name,
            *pw::keys::NODE_DESCRIPTION => label,
            *pw::keys::MEDIA_CLASS => "Audio/Sink",
            "audio.position" => "[ FL, FR ]",
            // 不要抢占系统默认设备。
            "priority.session" => "0",
        };
        let sink = session
            .core
            .create_object::<pw::node::Node>("adapter", &properties)
            .ok()?;
        // `create_object` 只把请求排进客户端缓冲区，必须迭代一次主循环才会真正发出，
        // 节点才会出现在图里。代理必须保留：丢弃代理会一并销毁远端节点。
        session::roundtrip(session).ok()?;
        Some(sink)
    }

    fn write_tone(path: &std::path::Path, seconds: f32, hz: f32) -> std::io::Result<()> {
        let rate = 48_000_u32;
        let frames = (rate as f32 * seconds) as u32;
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        for index in 0..frames {
            let value = (TONE_AMPLITUDE
                * (2.0 * std::f32::consts::PI * hz * index as f32 / rate as f32).sin()
                * f32::from(i16::MAX)) as i16;
            writer
                .write_sample(value)
                .and_then(|()| writer.write_sample(value))
                .map_err(|error| std::io::Error::other(error.to_string()))?;
        }
        writer
            .finalize()
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    fn paplay_available() -> bool {
        Command::new("paplay")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    /// 通过 PulseAudio 协议播放：Wine/Proton 的应用（winepulse）就是这样出声的。
    ///
    /// 新建的 sink 需要一点时间才会被 pipewire-pulse 暴露为 Pulse 设备，在那之前 paplay 会
    /// 立刻报"设备不存在"退出，因此这里确认进程还活着，否则重试。
    fn play_via_pulse_in_background(path: &std::path::Path, device: &str) -> Option<Child> {
        for _ in 0..8 {
            let child = Command::new("paplay")
                .arg(format!("--device={device}"))
                .arg(path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            std::thread::sleep(Duration::from_millis(600));
            let mut child = child;
            match child.try_wait() {
                Ok(Some(_)) => std::thread::sleep(Duration::from_millis(500)),
                _ => return Some(child),
            }
        }
        None
    }

    fn pw_loopback_available() -> bool {
        Command::new("pw-loopback")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    /// 造一对虚拟 sink/source：它是 Linux 上无麦克风时唯一能确定性地测录音路径的办法。
    fn spawn_virtual_microphone(sink_name: &str, source_name: &str, label: &str) -> Option<Child> {
        Command::new("pw-loopback")
            .arg("--capture-props")
            .arg(format!("media.class=Audio/Sink node.name={sink_name}"))
            .arg("--playback-props")
            .arg(format!(
                "media.class=Audio/Source node.name={source_name} node.description=\"{label}\""
            ))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
    }

    fn play_in_background(path: &std::path::Path, target: &str) -> Option<Child> {
        Command::new("pw-play")
            .arg("--target")
            .arg(target)
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
    }

    fn kill(child: Option<Child>) {
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[test]
    fn captures_a_sink_monitor_as_mono_chunks_at_the_requested_rate() {
        let Some(session) = connect_or_skip() else {
            return;
        };
        if !pw_play_available() {
            eprintln!("skipping PipeWire capture test: pw-play is not installed");
            return;
        }

        let Some(sink) = create_test_sink(&session, TEST_SINK_NAME, TEST_SINK_LABEL) else {
            eprintln!("skipping PipeWire capture test: could not create a null sink");
            return;
        };

        let directory = tempfile::tempdir().expect("temp dir");
        let tone = directory.path().join("tone.wav");
        write_tone(&tone, 4.0, TONE_HZ).expect("write tone");

        // 等待空 sink 出现在图里。
        let device_id = {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut last_error = None;
            loop {
                match list_devices() {
                    Ok(devices) => {
                        let found = devices
                            .into_iter()
                            .find(|device| device.name == TEST_SINK_LABEL && device.is_loopback)
                            .map(|device| device.id);
                        if let Some(id) = found {
                            break id;
                        }
                    }
                    Err(error) => last_error = Some(error.to_string()),
                }
                assert!(
                    Instant::now() < deadline,
                    "the test sink never showed up in the device list (last error: {last_error:?})"
                );
                std::thread::sleep(Duration::from_millis(250));
            }
        };

        let mut capture = AudioCapture::new(TEST_RATE, CaptureSource::Speaker);
        let device = capture
            .start(Some(device_id), None)
            .expect("capture should start");
        assert_eq!(device.name, TEST_SINK_LABEL);
        assert!(device.is_loopback);

        let player = {
            std::thread::sleep(Duration::from_millis(500));
            play_in_background(&tone, TEST_SINK_NAME)
        };
        assert!(player.is_some(), "pw-play should start");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("tokio runtime");
        let frames = runtime.block_on(async {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut frames = Vec::new();
            while Instant::now() < deadline {
                match tokio::time::timeout(Duration::from_secs(2), capture.read()).await {
                    Ok(Ok(chunk)) => {
                        assert_eq!(
                            chunk.len(),
                            CHUNK_FRAMES,
                            "capture should emit fixed-size chunks"
                        );
                        frames.extend_from_slice(&chunk);
                    }
                    Ok(Err(error)) => panic!("capture failed: {error}"),
                    Err(_) => break,
                }
            }
            frames
        });

        kill(player);
        capture.stop();
        let _ = session.core.destroy_object(sink);

        assert!(
            !frames.is_empty(),
            "no audio was captured from the sink monitor"
        );
        let peak = frames
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert!(
            (peak - TONE_AMPLITUDE).abs() < 0.1,
            "captured peak {peak} should match the played tone amplitude {TONE_AMPLITUDE}"
        );
        let loud = frames.iter().filter(|sample| sample.abs() > 0.05).count();
        assert!(
            loud >= TEST_RATE as usize / 2,
            "expected at least half a second of tone, got {loud} loud samples"
        );
    }

    /// 某个频率上的 DFT 幅度；纯音 `A·sin(2πft)` 的幅度约为 `A/2`。
    fn tone_magnitude(samples: &[f32], hz: f32, rate: u32) -> f32 {
        let (mut real, mut imaginary) = (0.0_f32, 0.0_f32);
        for (index, sample) in samples.iter().enumerate() {
            let phase = 2.0 * std::f32::consts::PI * hz * index as f32 / rate as f32;
            real += sample * phase.cos();
            imaginary += sample * phase.sin();
        }
        (real * real + imaginary * imaginary).sqrt() / samples.len().max(1) as f32
    }

    /// 直接驱动采集线程（用精确 pid，绕开按名字查找），持续 `seconds` 秒后停止。
    fn capture_process_for(session: &Session, pid: u32, seconds: f32) -> Vec<f32> {
        use std::sync::atomic::Ordering;

        let (tx, mut rx) = tokio::sync::mpsc::channel(128);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            capture_main(
                CaptureTarget::Process(pid),
                TEST_RATE,
                thread_stop,
                tx,
                ready_tx,
            );
        });

        let device = ready_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("capture should report readiness")
            .expect("process capture should start");
        assert_eq!(device.name, "Application audio");
        assert!(device.is_loopback);

        // 接线是异步的（我们的启动可能早于目标进程创建流），因此在有限时间内轮询确认。
        let link_deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let snapshot = graph::snapshot(session).expect("graph snapshot");
            let target_nodes: Vec<u32> = snapshot
                .output_streams_of(pid)
                .iter()
                .map(|node| node.id)
                .collect();
            let tapped = snapshot
                .links()
                .iter()
                .any(|link| target_nodes.contains(&link.output_node));
            if tapped {
                break;
            }
            assert!(
                !target_nodes.is_empty(),
                "the target process has no audio output stream"
            );
            assert!(
                Instant::now() < link_deadline,
                "no link was created from the target process to the capture stream"
            );
            std::thread::sleep(Duration::from_millis(100));
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("tokio runtime");
        let frames = runtime.block_on(async {
            let deadline = Instant::now() + Duration::from_secs_f32(seconds);
            let mut frames = Vec::new();
            while Instant::now() < deadline {
                match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
                    Ok(Some(chunk)) => {
                        assert_eq!(chunk.len(), CHUNK_FRAMES);
                        frames.extend_from_slice(&chunk);
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            frames
        });

        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();
        frames
    }

    #[test]
    fn process_lookup_matches_the_current_process() {
        let executable = std::env::current_exe().expect("current executable");
        let full_name = executable
            .file_name()
            .and_then(|name| name.to_str())
            .expect("executable name");
        // 完整名字超出 `comm` 的 15 字符上限，只有 `cmdline` 那条路径能匹配。
        let by_full_name = find_process_id(full_name).expect("lookup should succeed");
        assert_eq!(by_full_name, Some(std::process::id()));
        let by_prefix = find_process_id("vrcs_core").expect("lookup should succeed");
        assert_eq!(by_prefix, Some(std::process::id()));
        let blank = find_process_id("  ").expect("a blank name is not an error");
        assert_eq!(blank, None);
    }

    #[test]
    fn process_capture_taps_only_the_target_process() {
        let Some(session) = connect_or_skip() else {
            return;
        };
        if !pw_play_available() {
            eprintln!("skipping PipeWire capture test: pw-play is not installed");
            return;
        }
        let Some(sink) = create_test_sink(&session, TAP_SINK_NAME, TAP_SINK_LABEL) else {
            eprintln!("skipping PipeWire capture test: could not create a null sink");
            return;
        };

        let directory = tempfile::tempdir().expect("temp dir");
        let wanted = directory.path().join("wanted.wav");
        let other = directory.path().join("other.wav");
        write_tone(&wanted, 8.0, TONE_HZ).expect("write wanted tone");
        write_tone(&other, 8.0, OTHER_TONE_HZ).expect("write other tone");

        // 两个播放器都进同一个 sink：如果实现错录成 sink monitor，两个频率都会出现。
        let wanted_player = play_in_background(&wanted, TAP_SINK_NAME).expect("wanted player");
        let other_player = play_in_background(&other, TAP_SINK_NAME).expect("other player");
        let wanted_pid = wanted_player.id();

        let frames = capture_process_for(&session, wanted_pid, 2.5);
        kill(Some(wanted_player));
        kill(Some(other_player));
        let _ = session.core.destroy_object(sink);

        assert!(
            frames.len() as u32 > TEST_RATE,
            "captured only {} frames",
            frames.len()
        );
        let wanted_magnitude = tone_magnitude(&frames, TONE_HZ, TEST_RATE);
        let other_magnitude = tone_magnitude(&frames, OTHER_TONE_HZ, TEST_RATE);
        assert!(
            wanted_magnitude > 0.05,
            "the target process tone is missing: {wanted_magnitude}"
        );
        assert!(
            wanted_magnitude > 5.0 * other_magnitude,
            "capture is not isolated to the target process: wanted {wanted_magnitude}, other {other_magnitude}"
        );
    }

    /// Wine/Proton 的应用通过 winepulse 走 Pulse 协议；这类客户端的 registry
    /// `pipewire.sec.pid` 是 pipewire-pulse 自己，只有绑定后的 client info 才带真实 pid。
    /// 这个测试守住那条归属路径（否则 VRChat 在 Linux 上抓不到）。
    #[test]
    fn process_capture_taps_a_pulse_protocol_client() {
        let Some(session) = connect_or_skip() else {
            return;
        };
        if !paplay_available() {
            eprintln!("skipping Pulse capture test: paplay (pulseaudio-utils) is not installed");
            return;
        }
        let Some(sink) = create_test_sink(&session, PULSE_SINK_NAME, PULSE_SINK_LABEL) else {
            eprintln!("skipping Pulse capture test: could not create a null sink");
            return;
        };

        let directory = tempfile::tempdir().expect("temp dir");
        let tone = directory.path().join("tone.wav");
        write_tone(&tone, 8.0, TONE_HZ).expect("write tone");

        let player = play_via_pulse_in_background(&tone, PULSE_SINK_NAME).expect("paplay");
        let pid = player.id();
        let frames = capture_process_for(&session, pid, 2.0);
        kill(Some(player));
        let _ = session.core.destroy_object(sink);

        assert!(
            frames.len() as u32 > TEST_RATE / 2,
            "captured only {} frames from a Pulse client",
            frames.len()
        );
        let magnitude = tone_magnitude(&frames, TONE_HZ, TEST_RATE);
        assert!(
            magnitude > 0.05,
            "the Pulse client's tone is missing: {magnitude}"
        );
    }

    /// 麦克风路径：`CaptureSource::Microphone` 走 `Audio/Source` 节点，与系统音频（sink 的
    /// monitor）是两条不同的代码路径，这里用虚拟麦克风把录音链路端到端测掉。
    #[test]
    fn captures_a_microphone_source() {
        // 只需要"PipeWire 可用吗"这个判断：采集与枚举各自会建自己的会话。
        if connect_or_skip().is_none() {
            return;
        }
        if !pw_play_available() || !pw_loopback_available() {
            eprintln!("skipping microphone test: pw-play or pw-loopback is not installed");
            return;
        }
        // 名字带进程号：与其它测试实例、以及手工残留的 loopback 都不会互相串台。
        let suffix = std::process::id();
        let mic_sink = format!("vrcs-mic-sink-{suffix}");
        let mic_source = format!("vrcs-mic-source-{suffix}");
        let mic_label = format!("VRCS test microphone {suffix}");
        let Some(microphone) = spawn_virtual_microphone(&mic_sink, &mic_source, &mic_label) else {
            eprintln!("skipping microphone test: could not create a virtual microphone");
            return;
        };

        // 等虚拟麦克风出现在枚举里（它以 Audio/Source 出现，因此 is_loopback = false）。
        let device_id = {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let found = list_devices().ok().and_then(|devices| {
                    devices
                        .into_iter()
                        .find(|device| device.name == mic_label && !device.is_loopback)
                        .map(|device| device.id)
                });
                if let Some(id) = found {
                    break id;
                }
                assert!(
                    Instant::now() < deadline,
                    "the virtual microphone never showed up in the device list"
                );
                std::thread::sleep(Duration::from_millis(250));
            }
        };

        let directory = tempfile::tempdir().expect("temp dir");
        let tone = directory.path().join("tone.wav");
        write_tone(&tone, 8.0, TONE_HZ).expect("write tone");

        let mut capture = AudioCapture::new(TEST_RATE, CaptureSource::Microphone);
        let device = capture
            .start(Some(device_id), None)
            .expect("microphone capture should start");
        assert_eq!(device.name, mic_label);
        assert!(!device.is_loopback);

        let player = play_in_background(&tone, &mic_sink);
        assert!(player.is_some(), "pw-play should start");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("tokio runtime");
        let frames = runtime.block_on(async {
            let deadline = Instant::now() + Duration::from_secs(6);
            let mut frames = Vec::new();
            while Instant::now() < deadline {
                match tokio::time::timeout(Duration::from_secs(2), capture.read()).await {
                    Ok(Ok(chunk)) => {
                        assert_eq!(chunk.len(), CHUNK_FRAMES);
                        frames.extend_from_slice(&chunk);
                    }
                    Ok(Err(error)) => panic!("microphone capture failed: {error}"),
                    Err(_) => break,
                }
            }
            frames
        });

        kill(player);
        capture.stop();
        kill(Some(microphone));

        assert!(
            !frames.is_empty(),
            "no audio was captured from the microphone"
        );
        let magnitude = tone_magnitude(&frames, TONE_HZ, TEST_RATE);
        assert!(
            magnitude > 0.05,
            "the tone fed into the virtual microphone is missing: {magnitude}"
        );
    }
}
