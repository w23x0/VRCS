//! PipeWire 主循环会话与错误映射。

use std::time::Duration;

use pipewire as pw;
use pw::context::ContextRc;
use pw::core::CoreRc;
use pw::main_loop::MainLoopRc;

use crate::audio::AudioError;

/// 单次 `iterate` 的最长等待：把"等待事件"变成有界轮询，避免无限阻塞。
pub(crate) const ITERATE_TIMEOUT: Duration = Duration::from_millis(20);

/// 等待一轮 roundtrip 或流进入可采集状态的总预算。
pub(crate) const ROUNDTRIP_BUDGET: Duration = Duration::from_secs(5);

pub(crate) struct Session {
    pub(crate) main_loop: MainLoopRc,
    pub(crate) core: CoreRc,
    /// `pw_core` 引用 context，必须保持存活。
    _context: ContextRc,
}

impl Session {
    /// 连接本机 PipeWire 守护进程。
    pub(crate) fn connect() -> Result<Self, AudioError> {
        pw::init();
        let main_loop = MainLoopRc::new(None).map_err(|error| init_failed(&error))?;
        let context = ContextRc::new(&main_loop, None).map_err(|error| init_failed(&error))?;
        let core = context
            .connect_rc(None)
            .map_err(|error| service_not_running(&error))?;
        Ok(Self {
            main_loop,
            core,
            _context: context,
        })
    }
}

pub(crate) fn init_failed(error: &pw::Error) -> AudioError {
    AudioError::with_code(
        "audio.unavailable",
        format!("Failed to initialize PipeWire: {error}"),
    )
}

pub(crate) fn service_not_running(error: &pw::Error) -> AudioError {
    AudioError::with_code(
        "audio.service_not_running",
        format!("PipeWire is not running: {error}"),
    )
}

pub(crate) fn call_failed(operation: &str, error: &pw::Error) -> AudioError {
    AudioError::with_code(
        "audio.unavailable",
        format!("PipeWire {operation} failed: {error}"),
    )
}

/// 跑完一轮 `sync`：把当前排队的图事件全部处理掉再返回。
pub(crate) fn roundtrip(session: &Session) -> Result<(), AudioError> {
    let done = std::rc::Rc::new(std::cell::Cell::new(false));
    let pending = session
        .core
        .sync(0)
        .map_err(|error| call_failed("sync", &error))?;
    let _listener = session
        .core
        .add_listener_local()
        .done({
            let done = std::rc::Rc::clone(&done);
            move |_id, sequence| {
                if sequence == pending {
                    done.set(true);
                }
            }
        })
        .register();

    let loop_ = session.main_loop.loop_();
    let deadline = std::time::Instant::now() + ROUNDTRIP_BUDGET;
    while !done.get() && std::time::Instant::now() < deadline {
        loop_.iterate(pw::loop_::Timeout::Finite(ITERATE_TIMEOUT));
    }
    if done.get() {
        Ok(())
    } else {
        Err(AudioError::with_code(
            "audio.unavailable",
            "Timed out while reading the PipeWire graph",
        ))
    }
}
