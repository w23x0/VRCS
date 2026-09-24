# Linux 移植审查报告

分支：`linux-capture`（fork `w23x0/VRCS`）
上游：`Dreaminko/VRCS` `main`，审查时为 `7c407ad`，与本分支的分叉点相同，所以本分支相对上游的全部差异就是 Linux 移植本身。
审查日期：2026-09-24

## 1. 结论

- **Linux 端可以构建、测试并运行。** 两个 Rust crate 的 fmt、clippy（`-D warnings`）和测试全部通过，前端测试、i18n 校验、生产构建通过，`.deb` 和 `.AppImage` 都能打出来。在 Xvfb 下 AppImage 能启动，进程内 Core 能起来，能通过 PipeWire 枚举设备，没有托盘时关闭窗口会真正退出。
- **本轮新发现并修复了 3 个缺陷，另外修了 1 个遗留问题**，每个一个 commit（见第 3 节）。其中两个会直接影响用户：
  - 用户明确选择的采集设备，会在系统默认设备变化时被 WirePlumber 悄悄换成别的设备；
  - 之前的移植把 15 条文案（7 条音频错误提示和 8 条其他文案）改成了平台中性的措辞，Windows 用户因此看不到原来的 Windows 专属处理建议（违反了“不改变 Windows 行为”）。现在上游已有的每个文案键，在 4 种语言里都与上游逐字一致，Linux 另外显示 `_linux` 变体（第二轮按 D2 完成）。
- **遗留问题 #6（按进程采集“时间扭曲”）无法复现。** 我用和 Core 相同的方式驱动采集（按进程名查找、`spawn_blocking` 启动、同时轮询设备列表），分别用原生客户端和 PulseAudio 协议客户端、44.1 kHz 和 48 kHz 测试，音频的频率和电平都正确，合成语音也能通过 Silero VAD。另外找到一个测试工具本身的陷阱，可以原样复现当时报告的症状（见 4.2）。要去掉“实验性”标签，还需要在真实的 VRChat/Proton 上跑一次，这一步我这里做不了。
- **决定事项已处理到收尾**（见第 6 节）：D2、D6、D7 已完成，D3、D8 保留现状，D4、D5 只提给上游。只剩 D1、D9 两项需要你在真实 VRChat 或更新的 PipeWire/WirePlumber 环境上验证。
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
| `npm --workspace apps/desktop test` | ✅ 186 passed（原 182 个加上新增 4 个） |
| `tsc --noEmit`、`npm run build:frontend` | ✅ |
| `npm --workspace apps/desktop run build:linux` | ✅ 产出 `.deb` 和 `.AppImage`；`.deb` 的 Depends 包含 `libpipewire-0.3-0 (>= 0.3.65)`（第三轮加上版本下限，重新打包验证） |
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

## 3. 已修复（`ced5671..6eca4ed`，每项一个 commit，都已推送）

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

### 3.5 `9b6b2a5` 其余平台文案也改为按平台区分（D2，第二轮）

- **范围**：把 fork 相对上游的 4 个语言文件逐键比对，找出上游已有、但值被改动过的键：除了 3.4 的 7 条以外还有 8 条，即 `updates.status.unavailable`、`settings.apiManagement.securityNotice`、`sourceCredentialManager`、`settings.audio.systemOutputDescription`、`defaultMicrophoneDescription`、`onboarding.recognition.addApiDescription`、`errors.desktop.autostart_not_applied`、`errors.audio.unsupported_platform`。
- **修复**：基础键逐字恢复为上游文本，fork 的文案保留为 `<key>_linux`。`unsupported_platform` 例外，因为 Linux 版不会报这个错，所以只恢复基础值。直接调用 `t()` 的 6 处（软件更新状态、API 管理说明、引导页的识别步骤和音频步骤）传入同一个平台 context；开机启动的错误本来就经过 `localizedError`。`sourceCredentialManager` 在上游代码里也没有被引用，只恢复了它的值。
- **结果**：再次逐键比对，4 种语言里上游已有的键**与上游的差异为 0 条**；fork 只新增了键（`_linux` 变体、托盘不可用提示、VRChat 实验性标签和提示）。
- **验证**：新增 2 个测试，一个覆盖新变体在两个平台下的取值，另一个检查每个 `_linux` 键在每种语言里都有基础键可以回退。`check:i18n`、186 个前端测试、tsc、vite build 全部通过。另外在运行中的前端上验证：Vite 加独立 Core，用 Chromium 分别以 WebView2 UA 和 WebKitGTK UA 打开，每次都用全新配置；引导页和设置页上的 5 条文案各自显示对应平台的版本，没有出现另一平台的文案。为了进入引导页的音频步骤，测试中在浏览器里拦截了 `/api/asr/models`，让它报告模型已下载；这只作用于测试页面，没有改动 Core 和仓库代码。

### 3.6 `4b05062` 文档

`docs/Linux.md` 和 `docs/KnownIssues.md` 已按本轮结果更新：补充验证组合、写明“显式设备不跟随 / System default 跟随”的语义、重写 #6 的现状、标记 #5 已修复、新增 #8–#10。

### 3.7 `6826607` `.deb` 要求 PipeWire 0.3.65 以上（D6）

依赖改为 `libpipewire-0.3-0 (>= 0.3.65)`，与 `pipewire` crate 的 `v0_3_65` 特性一致（设备选择依赖的 `target.object` 需要 0.3.64 以上）。CI 的包检查改为必须带版本号，以后去掉版本号会直接让 CI 失败。验证：重新打包后 `Depends: libpipewire-0.3-0 (>= 0.3.65), …`，CI 的检查脚本在它上面通过；Ubuntu 24.04 上 `apt-get install -s` 能接受（`libpipewire-0.3-0t64` 1.0.5 提供带版本的 `libpipewire-0.3-0`）；Ubuntu 22.04 的 0.3.48 低于下限，会拒绝安装。

### 3.8 `6eca4ed` 一份配置只属于一个 Core（D7）

核对过：桌面壳只把路径传给 Core，界面通过 Core API 修改设置，`save_config` 是唯一的写入方，所以同一个安装里本来就只有一个写入方。缺的是运行第二个 Core 时的规则。`docs/Linux.md` 的 `VRCS_CONFIG` 一行和前端开发流程里写明：独立启动的 Core 要用自己的配置文件，不能在桌面程序运行时共用它的配置；凭据文件不同，它本来就是共享的，写入有锁。`docs/KnownIssues.md` #5 记录了这个决定。

## 4. 未修复的问题（附原因）

编号 1–10 与 `docs/KnownIssues.md` 一致。

| # | 问题 | 为什么没改 |
|---|---|---|
| 1 | VR Overlay 状态调用失败时，控件仍可操作 | 修复会同时改变 Windows 在出错时的表现，而且只有 IPC 失败才会触发。按 D4 不处理 |
| 2 | `app_build_info` 失败时，Linux 无边框窗口没有缩放把手 | 这是同步命令，几乎不会失败；组件与 Windows 共用，收益太低。按 D4 不处理 |
| 3 | build info 加载完成前，更新状态短暂显示“不可用” | 上游本来就有，Windows 也能看到。按 D4 只提给上游（U5） |
| 4 | `Debug` 分类标签没有走 i18n | 上游本来就有，Windows 也能看到。按 D4 只提给上游（U6） |
| 6 | 按进程采集“时间扭曲” | **无法复现**，见 4.2。缺少真实 VRChat/Proton 环境，所以“实验性”标签暂时保留（D1） |
| 7 | 挪威语目标语言名是乱码 `Norwegian Bokm姘搇`，会进入 LLM 提示词 | 上游 `1aa70f1` 引入，所有平台都受影响。按 D5 只提给上游（U2） |
| 8 | System default 模式在启动时仍把当前默认节点写进 `target.object` | 在 WirePlumber 0.4.17 上行为正确（会跟随）；在 0.5 上可能被钉死在旧的默认设备上，但我这里没有 0.5，无法验证，不硬改 |
| 9 | VRChat 重启（pid 变化）后，按进程采集不会跟随新进程，一直静音 | Windows 后端同样按 pid 绑定，属于两端都要改的产品行为。已列为上游草稿 U9 |
| 10 | `xdg-open` 子进程没有回收（僵尸进程） | 通过读代码发现，没有实际观察到；影响可以忽略 |

### 4.1 其他观察（没有列入 KnownIssues）

- **`a4ea843` 改变了所有平台的 OSC 行为**：“测试”按钮会绕过静音门，总是发送。这是一个合理的修复，按 D3 保留。
- 自动检查更新开关被置灰以后，仍然显示为“开”的状态（截图中可见）；因为它是禁用的，功能上没有影响。
- 托盘探测只检查 SNI watcher。只提供 XEmbed 托盘的桌面可能被误判为“没有托盘”，没有验证。

### 4.2 关于 #6 的一个测试陷阱

`paplay` 就是 `pacat`，它根据 `argv[0]` 决定工作模式。复制一份改名成 `VRChat.exe` 以后，它会把 WAV 当成 **44.1 kHz 的裸 PCM** 来播放（除非加 `--file-format=wav`），于是 440 Hz 变成约 404 Hz。这发生在 sink monitor 上，也就是在 VRCS 拿到音频之前。用这种方式搭的测试会得到“电平和采样率都对，但期望的频率上没有峰”，和当初的报告完全一致。当初报告的原因是不是这个，已经无从确认。

## 5. 给上游的 issue 草稿

按“收益高、改动小”排序，写成可以直接贴的口语短句。代码位置指上游 `main`（`7c407ad`）。只作为建议，不推送、不开 PR。

### U1. Cargo.lock 没跟版本号一起更新

升到 0.1.11 以后，两个 Cargo.lock 里还是 0.1.10，CI 带 `--locked` 会直接挂。`cargo metadata --manifest-path core/Cargo.toml --locked` 就能复现。跑一下 `cargo update -p vrcs-core -p vrcs-desktop --offline`，把 lock 提交上去就行，一共 3 行。

### U2. 挪威语的语言名是乱码

`core/src/providers.rs` 里写的是 `"nb" => "Norwegian Bokm姘搇"`，应该是 `Bokmål`。这个名字会直接拼进翻译 prompt，模型收到的是 `Target language: Norwegian Bokm姘搇 [nb]`。界面上看不出来，因为前端自己有一份语言表。改回来就行，顺手加个测试把语言表过一遍会更稳。

### U3. Linux 上 pipeline 有 6 个测试挂

`pipeline::test_dependencies` 里，TempDir 在 SQLite 连接还开着的时候就被释放了。Windows 上目录删不掉，所以碰巧能过；Linux 和 macOS 上会报 `attempt to write a readonly database`。让 TempDir 活到测试结束就行，只动测试代码。

### U4. 非 Windows 下 clippy 过不了

`audio/platform.rs` 的占位实现里有几处 dead_code，另外 gemini 的测试里有个 `field_reassign_with_default`，后面这个新版 clippy 在 Windows 上也会报。加几个 `cfg_attr(not(windows), allow(dead_code))`，gemini 那里改成结构体初始化就好。

### U5. 更新页刚打开会闪一下“不可用”

`updateStatusKey` 第一行在 `buildInfo` 还是 null 的时候也会返回 unavailable，所以正式版启动时也会闪一下。改成 `updater.buildInfo && !updater.buildInfo.updaterAvailable` 就行。

### U6. 设置里 Debug 这个标签没翻译

`SettingsTabBar.tsx` 里 `label: "Debug"` 是写死的，其他标签都走了 `t()`。check-i18n 只比较语言文件之间的键，查不出这种问题。

### U7. 错误提示可以按平台分开写

现在音频错误的提示都是按 Windows 写的。以后要支持别的平台，要么全改成笼统的说法，要么分平台写。i18next 自带 context：`t(key, { context: "linux" })` 找不到 `key_linux` 时会自动回退到原来的 key。只要在 `localizedError` 这一个地方加上，Windows 上看到的内容完全不变。我在 fork 里就是这么做的，可以参考。

### U8. 更新器只在 Windows 上编译

`target()` 写死了 `windows-x86_64-…`，公钥也是所有平台都会读。在别的平台上构建时，如果环境里有 `TAURI_UPDATER_PUBLIC_KEY`，它会去 release 里找 Windows 安装包。用 `cfg(windows)` 包一下，非 Windows 下两个命令直接返回 `update.unavailable` 就行。

### U9. VRChat 重启以后，按进程采集就没声了

pid 只在开始采集时查一次。VRChat 重开以后 pid 变了，采集还显示在运行，但一直没有声音，只能手动停掉再开。可以定期检查一下进程还在不在，不在了就重新按名字找一次；最起码进程退出时报一个 `audio.vrchat_not_running`，让用户知道。

### U10. `npm run build` 只能在 Windows 上跑

`build` 和几个 cuda 脚本直接调用 powershell，在其他系统上一跑就报错。在 README 里写一句只支持 Windows，或者改名叫 `build:windows`，都可以。

### U11. 非 Windows 上存不了 API Key

`credentials.rs` 在非 Windows 上直接返回错误，只能靠环境变量提供 Key。要支持的话有两条路：在数据目录放一个 0600 的文件，写的时候加 flock；或者走 Secret Service。前者简单，后者更安全但依赖桌面环境。fork 里用的是前者，带测试，可以参考。

### U12. 要不要考虑合入 Linux 支持

我在 fork 里做了一版 PipeWire 采集，支持系统声音、麦克风和只录 VRChat，还加了 deb/AppImage 打包和 Linux CI，在 Ubuntu 24.04 和 26.04 上都测过，Windows 的行为没有动。如果有兴趣，可以先合上面几个小的（U1、U3、U4），再按采集后端、凭据和目录、文案、打包和 CI 这几块分开提 PR，每一块都不影响 Windows。

## 6. 决定事项

| # | 事项 | 结果 |
|---|---|---|
| D1 | 按进程采集是否去掉“实验性”标签 | **待定**：需要你在真实的 VRChat/Proton 上按 `docs/Linux.md` 的端到端流程跑一次，干净的话就去掉（涉及 `AudioSettingsSection.tsx` 和 4 个语言文件） |
| D2 | 其余 Windows 可见措辞改成 `_linux` 变体 | **已完成**，见 3.5 |
| D3 | `a4ea843`（OSC 测试消息绕过静音门）在所有平台上生效 | **保留** |
| D4 | KnownIssues #1–#4 | #3、#4 只提给上游（U5、U6）；#1、#2 不处理 |
| D5 | 挪威语乱码（#7） | 只提给上游（U2），fork 里不改 |
| D6 | `.deb` 声明 `libpipewire-0.3-0 (>= 0.3.65)` | **已完成**，见 3.7 |
| D7 | `config.json` 多进程写入 | **已完成**：Core 是唯一写入方，不加锁，文档里写明规则，见 3.8 |
| D8 | 凭据存储 | **继续用** 0600 文件加锁 |
| D9 | 在 WirePlumber 0.5 / PipeWire 1.6 上复验 #8 和 3.2 | **待定**：需要你提供环境，验证方法见 `docs/KnownIssues.md` #8 |
| D10 | 与上游的关系 | 本分支不推送上游 `main`，第 5 节只作为建议 |

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
