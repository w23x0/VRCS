# Linux 移植审查报告

分支：`linux-capture`（fork `w23x0/VRCS`）
上游：`Dreaminko/VRCS` `main`，审查时为 `7c407ad`，与本分支的分叉点相同，所以本分支相对上游的全部差异就是 Linux 移植本身。
审查日期：2026-09-24

## 1. 结论

- **Linux 端可以构建、测试并运行。** 两个 Rust crate 的 fmt、clippy（`-D warnings`）和测试全部通过，前端测试、i18n 校验、生产构建通过，`.deb` 和 `.AppImage` 都能打出来。在 Xvfb 下 AppImage 能启动，进程内 Core 能起来，能通过 PipeWire 枚举设备，没有托盘时关闭窗口会真正退出。
- **本轮新发现并修复了 3 个缺陷，另外修了 1 个遗留问题**，每个一个 commit（见第 3 节）。其中两个会直接影响用户：
  - 用户明确选择的采集设备，会在系统默认设备变化时被 WirePlumber 悄悄换成别的设备；
  - 之前的移植把 7 条音频错误提示改成了平台中性的措辞，Windows 用户因此看不到原来的 Windows 专属处理建议（违反了“不改变 Windows 行为”）。
- **遗留问题 #6（按进程采集“时间扭曲”）无法复现。** 我用和 Core 相同的方式驱动采集（按进程名查找、`spawn_blocking` 启动、同时轮询设备列表），分别用原生客户端和 PulseAudio 协议客户端、44.1 kHz 和 48 kHz 测试，音频的频率和电平都正确，合成语音也能通过 Silero VAD。另外找到一个测试工具本身的陷阱，可以原样复现当时报告的症状（见 4.2）。要去掉“实验性”标签，还需要在真实的 VRChat/Proton 上跑一次，这一步我这里做不了。
- 上游 `main` 的 **`Cargo.lock` 和版本号不同步**，所以 `--locked` 会失败，上游 CI 大概率已经是红的。这是给上游的第一条建议（见第 5 节）。

## 2. 验证

### 2.1 环境

| 项目 | 值 |
|---|---|
| 系统 | Ubuntu 24.04.4 LTS，x86_64，4 核，无声卡、无 GPU |
| 音频 | PipeWire 1.0.5、WirePlumber 0.4.17、pipewire-pulse，无头会话（自建 dbus + 空 sink） |
| 工具链 | Rust 1.94.1，Node 24.9.0，CMake 3.28，clang 18 |
| 图形 | Xvfb（X11），WebKitGTK 4.1 |

原有文档是在 Ubuntu 26.04 / PipeWire 1.6.2 上验证的，这次补充了一个更旧的组合。

**环境限制和绕过方式**（都没有改仓库代码）：

- `cdn.pyke.io` 被网络策略拦截，`ort-sys` 下载不到预编译的 ONNX Runtime。我从 PyPI 取了同版本的 `onnxruntime==1.24.2` wheel，用 `ORT_LIB_LOCATION` 加 `ORT_PREFER_DYNAMIC_LINK=1` 动态链接。CI 上是静态链接，所以我这里打出的包**会运行时依赖 `libonnxruntime.so`**，不能代表正式产物。
- Hugging Face 被拦截，下载不到 Whisper 模型。所以“采集 → VAD → Whisper → 字幕库”这条链路**最后的 ASR 一步没有验证**，只验证到 VAD 切出语音段为止。ASR 本身与平台无关。
- 系统自带 Node 22，仓库要求 Node ≥ 24，所以另外下载了 Node 24。

### 2.2 结果

| 检查 | 结果 |
|---|---|
| `cargo fmt --check`（core、src-tauri） | ✅ |
| `cargo clippy --all-targets --locked -D warnings`（core、src-tauri） | ✅ 0 条 |
| `cargo test --lib`（core，连接真实 PipeWire 会话，全部集成测试都实际执行，没有跳过） | ✅ 479 passed，连续 3 次全绿。修复前每次都有 1 个失败，见 3.1 |
| `cargo test --lib`（src-tauri） | ✅ 40 passed，包括私有 dbus 上的托盘探测子进程测试 |
| `npm run check:i18n` | ✅ |
| `npm --workspace apps/desktop test` | ✅ 184 passed（原 182 个加上新增 2 个） |
| `tsc --noEmit`、`npm run build:frontend` | ✅ |
| `npm --workspace apps/desktop run build:linux` | ✅ 产出 `.deb` 和 `.AppImage`；`.deb` 的 Depends 包含 `libpipewire-0.3-0` |
| AppImage 在 Xvfb 中运行 | ✅ 引导页、主界面、设置页都能正常渲染；Core 在进程内启动；数据写到 `~/.local/share/vrcs`，日志写到 `~/.local/state/vrcs/logs`；无 StatusNotifier host 时给出警告，“最小化到托盘”置灰并注明原因；点 × 后进程退出 |
| Core 独立运行 + HTTP API | ✅ 启动、下载 Silero、`/health`、`/api/audio/devices` 正常；`/api/capture/start` 在没有 Whisper 模型时按设计返回 `asr.model.not_downloaded` |
| 系统音频（sink monitor）→ Silero VAD | ✅ 380 块中 283 块判为语音，切出语音段 |
| 按进程（伪 `VRChat.exe`）→ Silero VAD | ✅ 370 块中 324 块判为语音，切出 2 段 |
| 按进程采集的频率和电平（原生 / Pulse、44.1k / 48k、同时轮询设备） | ✅ 440 Hz 输入就采到 440 Hz，电平 0.400，约 16 000 帧/秒 |
| 显式选设备后切换默认设备 | ❌ → ✅ 已修复，见 3.2 |
| 显式选择的设备被拔出 | ✅ 修复后采集以错误结束，不再悄悄改录其他设备 |
| System default 模式下切换默认设备 | ✅ 在 WirePlumber 0.4.17 上会跟随新的默认设备，与 WASAPI 一致 |

**没有验证的：** CUDA（没有 GPU）、Wayland（只测了 X11）、真实声卡（只用空 sink）、真实的 VRChat/Proton、Whisper 识别、WirePlumber 0.5、PipeWire 0.3.x。

临时探针（用来对比 Core 调用方式、测 VAD、测默认设备切换的一次性测试）已经从工作区删除，没有提交。

## 3. 已修复（`ced5671..b90a4b5`，每项一个 commit，都已推送）

### 3.1 `4762743` 测试：让 PipeWire 测试的播放器固定在自己的 sink 上

- **现象**：全量 `cargo test --lib` 每次都失败 1 个（`captures_a_sink_monitor…`，采到的峰值是 0.73–0.77，期望 0.4）；单独跑音频模块却能通过。
- **根因**（用 `pw-dump` 在全量运行时追踪链路确认）：机器没有声卡时，各测试创建的空 sink 会轮流成为默认 sink。麦克风测试的 `pw-play` 当时正好挂在“旧默认 sink”上，WirePlumber 把它挪到了新默认 sink，也就是另一个测试的 sink，结果两个 440 Hz 音调叠在一起。
- **修复**：测试里的 `pw-play` 加上 `node.dont-reconnect = true`。只改测试代码。
- **验证**：修复前 7 次全量运行 7 次失败，修复后 5/5 通过，最终回归又跑了 3/3。

### 3.2 `d85a2ce` 用户显式选择的采集设备，在默认设备变化时保持不变

- **现象**：用户在设置里选了 sink A；系统默认设备切到 sink B 以后（比如插上耳机，或者在系统设置里切换），VRCS 实际在录 B 的 monitor，界面仍然显示 A。
- **复现**：两个空 sink 分别播放 440 Hz（A，被选中）和 220 Hz（B），采集过程中执行 `wpctl set-default B`。修复前，第 5 秒起采到的变成 220 Hz，链路被重连到 `sink-b:monitor_FL`。
- **根因**：流只带了 `target.object`，WirePlumber 0.4 会把这种流迁移到新的默认节点。Windows 端（`wasapi/capture.rs` 的 `follows_default`）只在“跟随系统默认”模式下才跟随，显式选择的 endpoint 不会动。
- **修复**：显式选择设备时，流额外带上 `node.dont-reconnect`。System default 模式不变。
- **验证**：修复后整段都是 440 Hz，链路一直在 A 上；把 A 拔掉（销毁节点）时采集以 “Audio capture has stopped” 结束，管线会把它作为错误上报，这和 Windows 对显式 endpoint 的处理一致。

### 3.3 `b8e5c80` 凭据存储的跨进程写入加锁（遗留问题 #5）

- **复现**：新增测试，8 个写者各写 25 个不同的键，每个写者用自己的文件句柄（`flock` 按打开的文件描述互斥，和两个进程之间的情形相同）。修复前 200 个键只剩 24 个。
- **修复**：写入和删除期间对同目录的 `credentials.lock`（权限 0600）持有独占的 `File::lock`（即 flock），读取仍然不加锁。锁放在单独的文件上，因为凭据文件每次写入都会被 `rename` 成新的 inode。Windows 分支（凭据管理器）没有改动。
- **验证**：200/200，连续 5 次通过；原有的往返测试改为同时期望锁文件存在，并检查它的权限。
- 注意：`File::lock` 需要 Rust 1.89 以上。仓库已经在用 1.88 才稳定的 `as_chunks`，CI 用的是 stable，所以没有问题。

### 3.4 `b90a4b5` 恢复音频错误提示里的 Windows 处理建议

- **问题**：之前的移植（`7b6dfa7`、`12f48c7`）把 7 条音频错误提示在 4 种语言里都改成了中性措辞，Windows 用户因此看不到原来的建议：“需要 Windows 11 或 Windows 10 build 20348”、“关闭独占模式”、“Windows Audio 服务”、“Windows 隐私设置”。这违反了“不改变 Windows 行为”的边界。
- **修复**：`errors.audio.*` 的基础键逐字恢复为上游 `7c407ad` 的文本；移植时写的 Linux 文案保留为 `<key>_linux`，覆盖 PipeWire 后端可能报出的 6 个错误码（`com_initialization_failed` 只有 WASAPI 会报，所以没有 Linux 变体）。`localizedError` 在 webview 的 UA 是 Linux（不含 Android）时传 i18next `context: "linux"`；其他平台，或者某个键没有 Linux 变体时，i18next 会回退到基础键，所以不会出现提示消失的情况。
- **验证**：新增 2 个测试，分别覆盖 UA 判断和真实 i18next 实例上的变体选择与回退；`check:i18n`、184 个前端测试、tsc、vite build 全部通过。

### 3.5 `4b05062` 文档

`docs/Linux.md` 和 `docs/KnownIssues.md` 已按本轮结果更新：补充验证组合、写明“显式设备不跟随 / System default 跟随”的语义、重写 #6 的现状、标记 #5 已修复、新增 #8–#10。

## 4. 未修复的问题（附原因）

编号 1–10 与 `docs/KnownIssues.md` 一致。

| # | 问题 | 为什么没改 |
|---|---|---|
| 1 | VR Overlay 状态调用失败时，控件仍可操作 | 修复会同时改变 Windows 在出错时的表现（违反边界）；而且只有 IPC 失败才会触发。建议交给上游或由你决定（D4） |
| 2 | `app_build_info` 失败时，Linux 无边框窗口没有缩放把手 | 这是同步命令，几乎不会失败；组件与 Windows 共用。收益太低 |
| 3 | build info 加载完成前，更新状态短暂显示“不可用” | 上游本来就有，Windows 也能看到；只影响显示。已列为上游草稿 U5 |
| 4 | `Debug` 分类标签没有走 i18n | 上游本来就有，Windows 也能看到。已列为上游草稿 U6 |
| 6 | 按进程采集“时间扭曲” | **无法复现**，见 4.2。缺少真实 VRChat/Proton 环境，所以“实验性”标签暂时保留（D1） |
| 7 | 挪威语目标语言名是乱码 `Norwegian Bokm姘搇`，会进入 LLM 提示词 | 上游 `1aa70f1` 引入，所有平台都受影响，修复会改变 Windows 发给 LLM 的内容。已列为上游草稿 U2（D5） |
| 8 | System default 模式在启动时仍把当前默认节点写进 `target.object` | 在 WirePlumber 0.4.17 上行为正确（会跟随）；在 0.5 上可能被钉死在旧的默认设备上，但我这里没有 0.5，无法验证，不硬改 |
| 9 | VRChat 重启（pid 变化）后，按进程采集不会跟随新进程，一直静音 | Windows 后端同样按 pid 绑定，属于两端都要改的产品行为。已列为上游草稿 U9 |
| 10 | `xdg-open` 子进程没有回收（僵尸进程） | 通过读代码发现，没有实际观察到；影响可以忽略 |

### 4.1 其他观察（没有列入 KnownIssues）

- **Windows 上还可见的措辞改动**（之前的移植留下的，这次没有恢复）：`securityNotice`、`sourceCredentialManager`、`addApiDescription`（“Windows 凭据管理器”改成了“系统凭据存储”）、`systemOutputDescription`、`defaultMicrophoneDescription`、`autostart_not_applied`、`updates.status.unavailable`。这些只是措辞变中性，内容仍然正确，没有丢失处理建议，所以留给你决定（D2）。
- **`a4ea843` 改变了所有平台的 OSC 行为**：“测试”按钮会绕过静音门，总是发送。这是一个合理的修复，但确实改变了 Windows 行为（D3）。
- 自动检查更新开关被置灰以后，仍然显示为“开”的状态（截图中可见）；因为它是禁用的，功能上没有影响。
- 托盘探测只检查 SNI watcher。只提供 XEmbed 托盘的桌面可能被误判为“没有托盘”，没有验证。
- `.deb` 的 `libpipewire-0.3-0` 依赖没有写版本。程序只链接老牌的 `pw_*` 符号，所以在旧版 PipeWire 上能加载，但 `target.object` 需要 PipeWire/WirePlumber 0.3.64 以上才生效；在 Ubuntu 22.04（PipeWire 0.3.48）上，显式选择设备可能不起作用。没有测试（D6）。
- `config.json` 在两个 Core 共用同一份配置时，也存在和 #5 一样的读-改-写问题（D7）。

### 4.2 关于 #6 的一个测试陷阱

`paplay` 就是 `pacat`，它根据 `argv[0]` 决定工作模式。复制一份改名成 `VRChat.exe` 以后，它会把 WAV 当成 **44.1 kHz 的裸 PCM** 来播放（除非加 `--file-format=wav`），于是 440 Hz 变成约 404 Hz。这发生在 sink monitor 上，也就是在 VRCS 拿到音频之前。用这种方式搭的测试会得到“电平和采样率都对，但期望的频率上没有峰”，和当初的报告完全一致。当初报告的原因是不是这个，已经无从确认。

## 5. 给上游的 issue 草稿

按“收益高、改动小”排序。每条都可以直接复制提交。代码位置指上游 `main`（`7c407ad`）。

---

### U1. `Cargo.lock` 与 0.1.11 版本号不同步，`cargo … --locked` 失败

**现象**：`core/Cargo.toml` 和 `apps/desktop/src-tauri/Cargo.toml` 已经升到 `0.1.11`，但两个 `Cargo.lock` 里自身包的版本仍是 `0.1.10`。CI 里的 clippy 和 test 都带 `--locked`，会直接报错 `cannot update the lock file … because --locked was passed`。

**复现**：
```bash
cargo metadata --manifest-path core/Cargo.toml --locked --format-version 1 >/dev/null
cargo metadata --manifest-path apps/desktop/src-tauri/Cargo.toml --locked --format-version 1 >/dev/null
```

**建议**：运行一次 `cargo update -p vrcs-core -p vrcs-desktop --offline`（或者不带 `--locked` 构建一次），提交 lockfile。差异只有 3 行（`version = "0.1.10"` 改成 `"0.1.11"`）。可以考虑在发版脚本里加一步 `cargo metadata --locked`，防止再次出现。

**规模**：3 行。**收益**：让 CI 恢复可用。

---

### U2. 挪威语目标语言名是乱码，进入 LLM 翻译提示词

**现象**：`core/src/providers.rs` 中 `"nb" => "Norwegian Bokm姘搇",` 是 `Bokmål` 的乱码（`1aa70f1` 引入）。`translation_language_name` 用于拼接每一次 LLM 翻译请求的目标语言行（`core/src/translation/prompt.rs`），所以翻译成挪威语时，模型收到的是 `Target language: Norwegian Bokm姘搇 [nb]`。前端有自己的一份表（`translation-languages.ts`），显示是正常的，界面上看不出问题。

**建议**：
1. 改回 `"Norwegian Bokmål"`；
2. 加一个一致性测试：遍历 `LLM_TRANSLATION_LANGUAGES`，断言 `translation_language_name` 对每个代码都返回 `Some`；断言 `DEEPL_TRANSLATION_LANGUAGES` 是它的子集；断言名字中没有 CJK 字符（或者直接要求全部是 ASCII，`Bokmål` 可以写成 ASCII 转写，由你决定）。

**规模**：1 行加 1 个测试。**收益**：修复一个会进入模型输入的数据错误。

---

### U3. 非 Windows 上有 6 个 `pipeline::dependencies` 测试失败

**现象**：在 Linux 上运行 `cargo test --manifest-path core/Cargo.toml --lib`，结果是 445 passed、6 failed，都在 `pipeline::dependencies::tests::native_*` 和 `reset_fails_only_its_pending_sources…`，报错 `attempt to write a readonly database`。

**根因**：`pipeline::test_dependencies` 返回前就把 `TempDir` 释放了，而 SQLite 连接还开着。Windows 上文件被占用，目录删不掉，所以测试碰巧能过；类 Unix 系统上目录会真的被删掉。

**建议**：让 `TempDir` 的生命周期覆盖整个测试，比如放进返回的结构体里（fork 的 `7b6dfa7` 中 `core/src/pipeline.rs` 有 4 行改动可以参考）。只改测试代码，Windows 行为不变。

**规模**：约 4 行。**收益**：测试在所有平台上都有意义，也为 U12 的 Linux CI 做准备。

---

### U4. 非 Windows 上 `clippy -D warnings` 失败

**现象**：Linux 上运行 `cargo clippy --manifest-path core/Cargo.toml --all-targets -- -D warnings`，报 5 个错误：
- `audio/platform.rs`（非 Windows 的占位实现）里 `wasapi_id`、`direction` 从未读取，`CHUNK_FRAMES`、`retryable_with_code`、`with_default_code` 从未使用，还有一个 `field 0 is never read`，都是 dead_code；
- `asr/streaming/provider/gemini.rs` 的测试触发 `clippy::field_reassign_with_default`。这一条与平台无关，新版 clippy 在 Windows 上同样会报。

**建议**：给只在 Windows 上使用的项加 `#[cfg_attr(not(windows), allow(dead_code))]`，gemini 测试改用结构体初始化语法（参考 fork 的 `7b6dfa7`）。

**规模**：约 10 行。**收益**：跨平台 lint 干净，也能挡住新版 clippy 在 Windows CI 上报错。

---

### U5. 设置 → 软件更新：build info 加载完成前短暂显示“此构建不可用”

**现象**：`apps/desktop/src/updates/SoftwareUpdateSettings.tsx` 的 `updateStatusKey` 第一行是 `if (!updater.buildInfo?.updaterAvailable) return "updates.status.unavailable";`，`buildInfo` 为 `null`（还在加载）时也会命中，所以 Windows 正式版启动时也会闪一下“不可用”。

**建议**：确认不可用以后再这样显示：`if (updater.buildInfo && !updater.buildInfo.updaterAvailable) …`，并加一个 `buildInfo: null` 时状态不是 unavailable 的测试。

**规模**：1 行加 1 个测试。

---

### U6. 设置分类标签 `Debug` 没有本地化

**现象**：`apps/desktop/src/settings/components/SettingsTabBar.tsx` 中 `{ id: "debug", label: "Debug", … }` 是唯一没有经过 `t(...)` 的分类标签。`scripts/check-i18n.mjs` 只比较各语言文件之间的一致性，所以发现不了这种遗漏。

**建议**：在 4 个语言文件里加上 `settings.categories.debug`，改用 `t(...)`。

**规模**：约 5 行。

---

### U7. 错误提示支持按平台区分（为多平台做准备）

**背景**：`errors.audio.*` 中的处理建议是按 Windows 写的（独占模式、Windows Audio 服务、Windows 11 build 20348）。只要上游考虑支持第二个平台，就会遇到两个选择：要么改成中性措辞（Windows 用户会失去具体建议），要么按平台区分。

**建议**：`localizedError`（`apps/desktop/src/app/app-utils.ts`）调用 `t()` 时传入 i18next 的 `context`（比如 `"linux"`）。平台专属的文案放在 `<key>_linux`，基础键保持 Windows 原文；没有变体的键会自动回退到基础键。fork 的 `b90a4b5` 是完整实现（含测试）：新增的 `platform-context.ts` 约 10 行，`app-utils.ts` 改 2 处。对只有 Windows 的上游来说，这个改动不会改变任何可见行为。

**规模**：约 15 行加测试。**收益**：以后加平台时不用在两个平台的文案之间取舍。

---

### U8. 更新器按平台编译，非 Windows 构建不去找 `windows-x86_64-*` 产物

**现象**：`apps/desktop/src-tauri/src/app_updates.rs` 中的 `target()` 固定返回 `windows-x86_64-{variant}`，`UPDATER_PUBLIC_KEY` 在所有平台上都通过 `option_env!` 读取。所以在导出了 `TAURI_UPDATER_PUBLIC_KEY` 的 shell 里构建非 Windows 版本，程序会去发布源里找 Windows 安装包。

**建议**：用 `#[cfg(windows)]` 把 updater 插件注册、`target()` 和 `UpdateState` 限定在 Windows 上；非 Windows 下，两个命令保持相同签名，直接返回 `update.unavailable`（fork 的 `858fb7a` 有实现和测试）。

**规模**：中等偏小（一个文件）。Windows 行为不变。

---

### U9. VRChat 重启后，按进程采集不会跟随新进程

**现象**：`AudioCapture::start` 只解析一次 pid，WASAPI 的进程回环和 Linux 的 tap 都绑定在这个 pid 上。VRChat 退出再启动以后，采集仍在“运行”，但一直没有声音，只能手动停止再开始。

**建议**：采集线程定期（比如每 2 秒）检查 pid 是否还活着；进程不在了，就重新按名字查找，并按“目标进程已变化”重建采集。Windows 端可以复用现有的 `should_recover_default` 重建循环。或者先做一个最小版本：进程退出时让采集以 `audio.vrchat_not_running` 结束，让 UI 能看到。

**规模**：中等。**收益**：VRChat 用户经常重启游戏，现在是静默失效。

---

### U10. npm 脚本 `build`、`dev:*:cuda` 只能在 PowerShell 下运行

**现象**：根目录 `package.json` 的 `build`、`dev:cuda`、`dev:core:cuda`、`dev:desktop:cuda` 直接调用 `powershell -File scripts/*.ps1`。在 Linux 或 macOS 上执行 `npm run build` 会直接失败，而 README 的开发说明没有提到这一点。

**建议**：在 README 里注明这些脚本仅限 Windows；或者给 `package.json` 加上按平台分开的入口（比如 `build:windows`）。

**规模**：很小。

---

### U11. 非 Windows 平台的凭据存储

**现象**：`core/src/credentials.rs` 在非 Windows 上的 `write_stored` 直接返回错误（“只能通过环境变量提供 API Key”），所以在非 Windows 上无法在界面里保存任何密钥。

**建议**（二选一，需要维护者定）：
- A：用 XDG 数据目录下的 0600 JSON 文件，原子替换，写入时用 flock 加锁（fork 的 `7b6dfa7` 加 `b8e5c80` 已实现并有测试）。简单，没有额外依赖。
- B：通过 Secret Service（libsecret / `keyring` crate）存储，安全性和 Windows 凭据管理器相当，但要求桌面上有 keyring 守护进程，无头环境下还需要回退方案。

**规模**：中等。只在支持非 Windows 平台时才需要。

---

### U12. 接受 PipeWire 采集后端，增加 Linux 打包和 CI（大，分阶段）

**背景**：fork 的 `linux-capture` 分支已经实现了完整的 Linux 支持，并在 Ubuntu 24.04 和 26.04 上验证过：PipeWire 设备枚举，sink monitor、麦克风和按进程 tap 采集，XDG 数据和日志目录，托盘探测，无边框窗口的缩放把手，`.deb` 和 `.AppImage` 打包，以及对应的测试。

**建议的 PR 顺序**（每一步都不改变 Windows 行为）：
1. U1、U3、U4（让非 Windows 平台能编译，测试和 lint 能过）；
2. `audio.rs` 增加平台无关的 `CaptureTarget::device()` 构造函数，接入 `audio/linux/` 模块（通过 `cfg(target_os = "linux")` 选择）；
3. U11 凭据存储、XDG 日志和数据目录、U8 更新器按平台编译、托盘可用性上报（`BuildInfo.trayAvailable`、`platform`）；
4. U7 平台文案；
5. `tauri.linux.conf.json`、`build:linux`，以及 CI 的 `linux` job（fmt、clippy、`test --lib`；没有 PipeWire 会话时相关集成测试会自动跳过）。

**规模**：大（约 3 000 行，大部分集中在新增的 `audio/linux/` 目录）。**收益**：Linux（Proton 下的 VRChat 玩家）可以直接使用上游版本。

## 6. 需要你决定的事项

| # | 事项 | 我的建议 |
|---|---|---|
| D1 | 按进程采集是否去掉“实验性”标签 | 先在真实的 VRChat/Proton 上按 `docs/Linux.md` 的端到端流程跑一次，干净的话就去掉（涉及 `AudioSettingsSection.tsx` 和 4 个语言文件） |
| D2 | 4.1 中列出的其余 Windows 可见措辞：保留中性措辞，还是也改成 `_linux` 变体 | 用 3.4 的机制改成变体，让 Windows 端与上游完全一致；工作量小 |
| D3 | `a4ea843`（OSC 测试消息绕过静音门）在所有平台上生效 | 行为本身合理；如果要严格遵守“Windows 不变”，可以回退后改为向上游提 issue |
| D4 | KnownIssues #1–#4（修复会改变 Windows 表现）在 fork 里修，还是只提给上游 | #3、#4 已写成上游草稿；#1、#2 收益很低，建议暂不处理 |
| D5 | 挪威语乱码（#7）是否先在 fork 里修 | 等上游修（U2）；如果你近期要在 Windows 上用这个 fork 翻译挪威语，就先带上这 1 行 |
| D6 | `.deb` 是否声明 `libpipewire-0.3-0 (>= 0.3.65)` | 声明。代价是 Ubuntu 22.04 装不上，好处是不会装上以后才发现设备选择不起作用 |
| D7 | `config.json` 多进程写入：只允许 Core 写，还是也加锁 | 只允许 Core 写，并在文档里说明“不要让两个 Core 共用同一份配置” |
| D8 | 凭据：继续用 0600 文件，还是改用 Secret Service | 暂时继续用文件（已经加锁）；如果要推给上游，按 U11 由上游维护者决定 |
| D9 | 是否有一台 WirePlumber 0.5 或 PipeWire 1.6 的机器可以复验 #8 和 3.2 | 需要你提供环境；验证方法见 `docs/KnownIssues.md` #8 |
| D10 | 是否按 U12 的顺序把本分支整理成上游 PR 系列 | 按你的要求，我没有向上游推送，也没有开 PR |

## 7. 复现本报告的验证

```bash
# 依赖（Ubuntu 24.04）
sudo apt install build-essential cmake pkg-config clang libclang-dev libssl-dev \
  libpipewire-0.3-dev libspa-0.2-dev pipewire pipewire-bin wireplumber pipewire-pulse \
  pulseaudio-utils libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf
export BINDGEN_EXTRA_CLANG_ARGS="-I$(gcc -print-file-name=include)"

cargo fmt --manifest-path core/Cargo.toml -- --check
cargo clippy --manifest-path core/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path core/Cargo.toml --lib --locked          # 需要 PipeWire 会话才会运行采集测试
cargo clippy --manifest-path apps/desktop/src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --lib --locked
npm ci && npm run check:i18n && npm --workspace apps/desktop test && npm run build:frontend
npm --workspace apps/desktop run build:linux
```

显式设备与默认设备切换的复现方法：创建两个空 sink（`pw-cli -m create-node adapter '{ factory.name=support.null-audio-sink node.name=sink-a media.class=Audio/Sink … }'`），分别用 `pw-play --target` 播放不同频率的音调，选中 `sink-a` 开始采集，然后执行 `wpctl set-default <sink-b 的 id>`，观察采到的频率。
