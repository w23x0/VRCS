# Linux (PipeWire)

VRCS Core builds and runs on Linux with a PipeWire capture backend (`core/src/audio/linux/`). This page covers build prerequisites, running the Core and the frontend, capture semantics, the current capability matrix, and the Linux test commands.

Both entry points work on Linux: the Tauri desktop shell (packaged as `.deb` and `.AppImage`) and the standalone Core together with the Vite frontend.

## Build prerequisites

Verified on Ubuntu 26.04 (x64) with PipeWire 1.6.2 and WirePlumber, and on Ubuntu 24.04 (x64) with PipeWire 1.0.5 and WirePlumber 0.4.17 (build, both crates' tests, frontend tests, `.deb`/`.AppImage` bundles). The capture backend is built against the `pipewire` crate's `v0_3_65` API level; older PipeWire releases have not been tested.

- Rust stable
- Node.js 24+ (frontend only)
- `cmake` — required, `whisper-rs` uses it to build whisper.cpp
- `libssl-dev` — required
- `libpipewire-0.3-dev` and `libspa-0.2-dev` — required by the Linux capture backend (`pipewire-rs`)
- `clang`, `libclang-dev`, `pkg-config` — required by bindgen
- PipeWire and WirePlumber at runtime. PulseAudio is not required.

```bash
sudo apt install build-essential cmake pkg-config clang libclang-dev libssl-dev \
  libpipewire-0.3-dev libspa-0.2-dev
```

### CMake version

`sudo apt install cmake` is enough: whisper.cpp builds with the distribution CMake (verified with CMake 4.2.3 on Ubuntu 26.04). If a future CMake release rejects whisper.cpp because its `cmake_minimum_required` is too old, install CMake 3.x into `~/.local` and put it first on `PATH`:

```bash
curl -fsSL -o /tmp/cmake.tar.gz \
  https://github.com/Kitware/CMake/releases/download/v3.31.6/cmake-3.31.6-linux-x86_64.tar.gz
tar -xzf /tmp/cmake.tar.gz -C ~/.local --strip-components=1
export PATH="$HOME/.local/bin:$PATH"
```

### Why `libssl-dev` is needed

`libssl-dev` is a **build-time** requirement only. The `ort-sys` build dependency chain (`ureq` → `native-tls` → `openssl-sys`) uses OpenSSL to download ONNX Runtime while building. It is not a runtime dependency of the resulting binary.

### Known pitfall: bindgen `stdbool.h`

If bindgen aborts with `'stdbool.h' file not found`, clang's default header search path does not include gcc's built-in include directory. Point it there:

```bash
export BINDGEN_EXTRA_CLANG_ARGS="-I$(gcc -print-file-name=include)"
```

## Run the Core

```bash
cargo run --manifest-path core/Cargo.toml
```

The Core listens on `http://127.0.0.1:8766` and serves the subtitle WebSocket at `ws://127.0.0.1:8766/ws`. Its configuration defaults to `config.json` in the working directory.

Environment variables read by the Core:

| Variable | Effect |
|---|---|
| `VRCS_CONFIG` | Configuration file path (default `config.json`). One configuration file belongs to one running Core: never point a standalone Core at the desktop app's `~/.local/share/vrcs/config.json` while the app runs |
| `VRCS_HOST` | Bind address (default `127.0.0.1`) |
| `VRCS_PORT` | Bind port (default `8766`) |
| `VRCS_SESSION_TOKEN` | Session token. When unset, a random token is generated and printed to stderr at startup. Binding to a non-loopback address requires an explicit non-empty token. |
| `VRCS_SILERO_MODEL` | Override the Silero VAD model path |
| `VRCS_ASR_MODEL_DIR` | Override the Whisper model directory |
| `VRCS_LOG_DIR` | Log directory (default: `logs/` next to the configuration file) |

Example with an isolated configuration and a fixed token:

```bash
VRCS_SESSION_TOKEN=devtoken VRCS_CONFIG=/tmp/vrcs-dev/config.json VRCS_PORT=8766 \
  cargo run --manifest-path core/Cargo.toml
```

## Frontend development loop

The full UI runs in a browser against a standalone Core; no Tauri or WebKit is involved.

The Core is the only writer of its configuration file (the desktop shell only passes the path, the UI changes settings through the Core API), and it assumes nobody else writes that file. Give the standalone Core its own `VRCS_CONFIG`, as below, rather than the desktop app's configuration: two Cores on one file would overwrite each other's changes. Credentials are the exception, because `credentials.json` is shared by design and its writes are locked.

```bash
npm install

# Terminal 1
VRCS_SESSION_TOKEN=devtoken cargo run --manifest-path core/Cargo.toml

# Terminal 2
VITE_VRCS_SESSION_TOKEN=devtoken npm --workspace apps/desktop run dev
```

Then open `http://localhost:1420`. The frontend talks to `http://127.0.0.1:8766` and `ws://127.0.0.1:8766/ws` and sends `Authorization: Bearer <token>`, so `VITE_VRCS_SESSION_TOKEN` must match the Core's `VRCS_SESSION_TOKEN`.

## Build packages

Bundling needs the Tauri toolchain on top of the prerequisites above:

```bash
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf
```

```bash
npm --workspace apps/desktop run build:linux
```

This runs the frontend production build and then bundles both targets:

```
apps/desktop/src-tauri/target/release/bundle/deb/VRCS_<version>_amd64.deb
apps/desktop/src-tauri/target/release/bundle/appimage/VRCS_<version>_amd64.AppImage
```

Install the Debian package with `sudo dpkg -i <package>.deb`; it depends on `libwebkit2gtk-4.1-0`, `libgtk-3-0`, `libayatana-appindicator3-1`, and `libpipewire-0.3-0 (>= 0.3.65)` (the desktop binary links the Core in-process, so the PipeWire client library is a hard runtime dependency; the minimum matches the `pipewire` crate's `v0_3_65` API level, and `target.object`, which device selection relies on, needs 0.3.64 or later). Distributions with an older PipeWire, such as Ubuntu 22.04 (0.3.48), refuse the package instead of installing a build whose device selection silently does not work. The AppImage is self-contained, so make it executable and run it directly. Add `-- --bundles deb` to build only one target.

The bundles are not signed and do not include updater artifacts, so they are for local builds; the release pipeline with its signing keys lives in `scripts/build-release.ps1` (Windows).

## Capture semantics

- **System audio** records the **monitor port of the selected sink**: the stream is opened with `stream.capture.sink` and targets that sink node.
- **Microphone** records the selected source node.
- Device ids are a stable hash of the PipeWire `node.name`, so they survive restarts. PipeWire's global node ids change on every reconnect and are never used as device identity; a stored id is resolved back to its `node.name` before capture.
- The requested stream format is fixed: **16 kHz, mono, f32** (`F32LE`). PipeWire inserts converters and resamplers into the graph, so the device's native format does not matter. If the server does not accept the requested format, the stream renegotiates; if that fails the capture reports `audio.unsupported_format`.
- Sample rate comes from the PipeWire graph rate (`clock.rate` in the `settings` metadata, with `default.clock.rate` accepted as a legacy fallback); the channel count of a device is its node's `audio.channels`, and the default device is determined from the `default` metadata.
- Per-process capture taps the target application's audio output streams; see below. If the tap cannot be created the capture reports `audio.process_loopback_unavailable` instead of silently falling back to whole-system audio.
- Device endpoints are PipeWire `node.name` values, not WASAPI endpoint ids, so an audio configuration copied from Windows does not carry over.
- A device chosen explicitly stays chosen, as with WASAPI: the stream carries `node.dont-reconnect`, so the session manager neither moves it to a new default device nor falls back to another device when it disappears. If the selected device goes away the capture stops with an error instead of silently recording something else. **System default** follows the default device when it changes (verified with WirePlumber 0.4.17; see issue 8 in [Known issues](KnownIssues.md) for WirePlumber 0.5).

### Per-process capture

> **Status: experimental.** The mechanism is covered by integration tests (they verify the tapped tone's spectrum and that a second application's audio is excluded). A time-warped capture was reported earlier when the mode was started through the Core; it could not be reproduced when the capture is driven exactly like the Core does (lookup by the process name `VRChat.exe`, `spawn_blocking` start, concurrent device polling), with a native and a PulseAudio-protocol player, 44.1 and 48 kHz streams: the tone arrived at the right frequency and level, and synthesized speech was accepted by the Silero VAD. It has not yet been checked with the real VRChat under Proton, so the option stays marked experimental; if you see no subtitles while VRChat is speaking, use **System output**. Tracked as issue 6 in [Known issues](KnownIssues.md).

Selecting VRChat (or any single application) as the source resolves the process id, then finds the
audio output streams that belong to that process and links their output ports to a private capture
stream. The application keeps playing through its own device, exactly like Windows process loopback:
nothing about its routing is changed, so there is nothing to restore on stop.

Details and current limits:

- The process id comes from `find_process_id`, which matches `/proc/<pid>/comm` and `/proc/<pid>/cmdline`.
- A stream belongs to a process through its PipeWire client. The registry only exposes `pipewire.sec.pid`
  for a client (the process that opened the connection), which for a `pipewire-pulse` proxy is
  `pipewire-pulse` itself; the client is therefore bound and its `application.process.id` is used, which
  is what Wine/Proton applications report. Native clients fall back to `pipewire.sec.pid`.
- Each of the application's output streams is tapped through its first output port, so multi-stream
  applications are all captured, and stereo content is captured as a single channel (voice is
  effectively identical on both channels).
- New output streams are picked up within a quarter second, and a tap is re-created if its link
  disappears, so restarting the application's audio does not require restarting capture.
- Capture starts even when the application is not playing anything yet; the level meter simply stays quiet.
- The desktop UI offers this for VRChat only: the process name (`VRChat.exe`) is fixed in the core, so other applications are not selectable yet.

### CUDA build

```bash
export PATH="/usr/local/cuda/bin:$PATH"          # nvcc must be on PATH
export CMAKE_CUDA_ARCHITECTURES=native           # whisper-rs forwards CMAKE_* to CMake
cargo build --manifest-path core/Cargo.toml --features cuda
```

Install the toolkit from [NVIDIA's CUDA repository](https://developer.nvidia.com/cuda-downloads); the distribution package can be
too old for recent GPUs — Ubuntu 26.04 ships CUDA 12.4, which cannot target compute capability 12.0 (Blackwell). The preflight
loads `libcuda.so.1` from the driver at runtime, so the resulting binary does not link the driver directly; it still links the
toolkit's runtime libraries (`cublas`, `cublasLt`, `cudart`, and `culibos` where whisper.cpp asks for it). Set
`asr.local.device` to `cuda`, or leave it on `auto` to prefer CUDA when it is available.

## Capability matrix

| Capability | Status on Linux |
|---|---|
| System audio capture | ✅ (sink monitor) |
| Microphone capture | ✅ |
| Local Whisper (CPU) | ✅ (subtitles verified end to end) |
| Cloud ASR | ✅ (platform-independent code path) |
| SQLite history + FTS | ✅ |
| Per-process capture (VRChat only) | ⚠️ experimental: taps the application's own output streams; see the per-process capture section above for the observed limitation |
| VR Overlay | ❌ Windows only (GDI rendering + OpenVR) |
| CUDA acceleration for local Whisper | ✅ build with `--features cuda`; requires the NVIDIA driver and a CUDA toolkit recent enough for the GPU (CUDA 13.2 was used here) |
| VRCX-0 integration | ✅ platform-independent: a localhost WebSocket client with a token, no Windows-specific code. It needs a running VRCX-0 on the configured port and otherwise reports an error state, exactly as on Windows. Whether a Linux build of VRCX-0 serves the same API has not been verified here |
| Dictionary import and lookup, learning items, Anki card export | ✅ platform-independent; AnkiConnect is reached over localhost HTTP |
| Credential storage | File at `$XDG_DATA_HOME/vrcs/credentials.json` with mode `0600`, instead of the Windows Credential Manager |
| Tauri desktop shell | ✅ builds and runs (unit tests pass) |
| Installers (deb/AppImage) | ✅ `npm --workspace apps/desktop run build:linux` |
| In-app updater | ❌ not compiled off Windows; the build reports updates as unavailable |

Credential storage details: the path is `$XDG_DATA_HOME/vrcs/credentials.json` (`XDG_DATA_HOME` defaults to `~/.local/share`), the file is written with mode `0600` using a temporary file plus atomic rename, writes from several processes are serialized by an advisory lock on the sibling `credentials.lock`, and environment variable overrides keep their existing precedence over stored values.

Data root: the desktop shell keeps its configuration, model files, subtitle database and credentials in `$XDG_DATA_HOME/vrcs` (`~/.local/share/vrcs`), and writes logs to `$XDG_STATE_HOME/vrcs/logs` (`~/.local/state/vrcs/logs`). An installation created while the shell still used the hidden `$XDG_DATA_HOME/.vrcs` directory is moved to the new location on first start; if that move fails the old directory stays in use (with a warning in the log), so an existing configuration and history are never silently abandoned.

System tray: the tray icon is visible only when the desktop provides a StatusNotifier/AppIndicator host (GNOME needs an extension for this). Because the appindicator library loads whether or not a panel implements the protocol, VRCS asks the session bus for a `StatusNotifierWatcher` at startup: without one it starts normally, keeps the tray icon invisible, disables **Minimize to tray**, and closing the window really closes it instead of hiding into a tray that does not exist. A bus that cannot be reached is treated as "host present", so a probe failure never removes a working tray.

## Testing

```bash
cargo test --manifest-path core/Cargo.toml --lib
npm run check:i18n
npm --workspace apps/desktop test
```

- The PipeWire capture integration test in `core/src/audio/linux/mod.rs` needs a live PipeWire session and skips itself when it cannot connect.
- That test also plays a test tone through `pw-play` (from `pipewire-bin` on Debian/Ubuntu) and skips when the command is missing. It creates its own null sink with a low session priority, so it does not take over the default device on a machine with a sound card. Without one (a VM or CI runner), each test sink becomes the default in turn; the test players set `node.dont-reconnect` so the session manager does not move them between the tests' sinks.
- The PulseAudio-protocol tap test uses `paplay` from `pulseaudio-utils` and `pipewire-pulse`, and skips when `paplay` is missing.

### End-to-end check of the capture path

The integration tests drive `AudioCapture` directly. To exercise the whole chain (capture → VAD → local Whisper → subtitle database) through the Core's HTTP API, use a virtual sink and a speech file:

```bash
# 1. A sink whose monitor always carries audio, and its virtual source.
pw-loopback --capture-props 'media.class=Audio/Sink node.name=vrcs-e2e-sink' \
            --playback-props 'media.class=Audio/Source node.name=vrcs-e2e-source' &

# 2. Point audio.output at that sink. mode = "system", device_id = the same
#    FNV-1a 64-bit hash of the node name that the backend uses for device ids:
python3 -c 'n="vrcs-e2e-sink";h=0xcbf29ce484222325
for b in n.encode(): h=((h^b)*0x100000001b3)&0xFFFFFFFFFFFFFFFF
print(h & 0x7fffffffffffffff)'

# 3. Run the Core with local Whisper and the Silero model, start capture, play
#    speech into the sink, then read the transcripts back.
VRCS_CONFIG=/tmp/vrcs-e2e/config.json VRCS_PORT=8766 VRCS_SESSION_TOKEN=devtoken \
VRCS_SILERO_MODEL=~/.local/share/vrcs/models/silero_vad.onnx \
  cargo run --manifest-path core/Cargo.toml &
curl -s -X POST -H 'Authorization: Bearer devtoken' -H 'Content-Type: application/json' \
     -d '{}' http://127.0.0.1:8766/api/capture/start
pw-play --target vrcs-e2e-sink speech.wav
curl -s -H 'Authorization: Bearer devtoken' 'http://127.0.0.1:8766/api/subtitles?limit=5'
```

A short speech sample (for example `jfk.wav` from the whisper.cpp repository) is enough: with `asr.backend = "local_whisper"` and `asr.local.model = "tiny"` the transcript appears within a few seconds of playback. This is also the way to check the microphone path (`audio.microphone.mode = "device"` pointed at a virtual source).
