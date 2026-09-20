# Linux (PipeWire)

VRCS Core builds and runs on Linux with a PipeWire capture backend (`core/src/audio/linux/`). This page covers build prerequisites, running the Core and the frontend, capture semantics, the current capability matrix, and the Linux test commands.

Both entry points work on Linux: the Tauri desktop shell (packaged as `.deb` and `.AppImage`) and the standalone Core together with the Vite frontend.

## Build prerequisites

Verified on Ubuntu 26.04 (x64) with PipeWire 1.6.2 and WirePlumber.

- Rust stable
- Node.js 24+ (frontend only)
- `cmake` — required, `whisper-rs` uses it to build whisper.cpp
- `libssl-dev` — required
- `libpipewire-0.3-dev` and `libspa-0.2-dev` — required by the Linux capture backend (`pipewire-rs`)
- `clang`, `libclang-dev`, `pkg-config` — required by bindgen
- PipeWire and WirePlumber at runtime. PulseAudio is not required.

```bash
sudo apt install build-essential pkg-config clang libclang-dev libssl-dev \
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
| `VRCS_CONFIG` | Configuration file path (default `config.json`) |
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

```bash
npm install

# Terminal 1
VRCS_SESSION_TOKEN=devtoken cargo run --manifest-path core/Cargo.toml

# Terminal 2
VITE_VRCS_SESSION_TOKEN=devtoken npm --workspace apps/desktop run dev
```

Then open `http://localhost:1420`. The frontend talks to `http://127.0.0.1:8766` and `ws://127.0.0.1:8766/ws` and sends `Authorization: Bearer <token>`, so `VITE_VRCS_SESSION_TOKEN` must match the Core's `VRCS_SESSION_TOKEN`.

## Build packages

```bash
npm --workspace apps/desktop run build:linux
```

This runs the frontend production build and then bundles both targets:

```
apps/desktop/src-tauri/target/release/bundle/deb/VRCS_<version>_amd64.deb
apps/desktop/src-tauri/target/release/bundle/appimage/VRCS_<version>_amd64.AppImage
```

Install the Debian package with `sudo dpkg -i <package>.deb`; it depends on `libwebkit2gtk-4.1-0`, `libgtk-3-0`, and `libayatana-appindicator3-1`. The AppImage is self-contained, so make it executable and run it directly. Add `-- --bundles deb` to build only one target.

The bundles are not signed and do not include updater artifacts, so they are for local builds; the release pipeline with its signing keys lives in `scripts/build-release.ps1` (Windows).

## Capture semantics

- **System audio** records the **monitor port of the selected sink**: the stream is opened with `stream.capture.sink` and targets that sink node.
- **Microphone** records the selected source node.
- Device ids are a stable hash of the PipeWire `node.name`, so they survive restarts. PipeWire's global node ids change on every reconnect and are never used as device identity; a stored id is resolved back to its `node.name` before capture.
- The requested stream format is fixed: **16 kHz, mono, f32** (`F32LE`). PipeWire inserts converters and resamplers into the graph, so the device's native format does not matter. If the server does not accept the requested format, the stream renegotiates; if that fails the capture reports `audio.unsupported_format`.
- Sample rate and channel count come from the PipeWire graph settings (`default.clock.rate`), and the default device is determined from the `default` metadata.
- There is no per-process capture on Linux: requesting it returns the error code `audio.process_loopback_unavailable` instead of silently falling back to whole-system audio.
- Device endpoints are PipeWire `node.name` values, not WASAPI endpoint ids, so an audio configuration copied from Windows does not carry over.

### Per-process capture

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
loads `libcuda.so.1` from the driver at runtime, so the resulting binary only needs `libcudart`/`libcublas` from the toolkit
(CMake records their path as an rpath). Set `asr.local.device` to `cuda`, or leave it on `auto` to prefer CUDA when it is available.

## Capability matrix

| Capability | Status on Linux |
|---|---|
| System audio capture | ✅ (sink monitor) |
| Microphone capture | ✅ |
| Local Whisper (CPU) | ✅ (subtitles verified end to end) |
| Cloud ASR | ✅ (platform-independent code path) |
| SQLite history + FTS | ✅ |
| Per-process capture (VRChat only) | ✅ taps the application's own output streams |
| VR Overlay | ❌ Windows only (GDI rendering + OpenVR) |
| CUDA acceleration for local Whisper | ✅ build with `--features cuda`; requires the NVIDIA driver and a CUDA toolkit recent enough for the GPU (CUDA 13.2 was used here) |
| VRCX-0 integration | ❌ Windows program; on Linux it degrades to an error state |
| Dictionary import and lookup, learning items, Anki card export | ✅ platform-independent; AnkiConnect is reached over localhost HTTP |
| Credential storage | File at `$XDG_DATA_HOME/vrcs/credentials.json` with mode `0600`, instead of the Windows Credential Manager |
| Tauri desktop shell | \u2705 builds and runs (unit tests pass) |
| Installers (deb/AppImage) | \u2705 `npm --workspace apps/desktop run build:linux` |

Credential storage details: the path is `$XDG_DATA_HOME/vrcs/credentials.json` (`XDG_DATA_HOME` defaults to `~/.local/share`), the file is written with mode `0600` using a temporary file plus atomic rename, and environment variable overrides keep their existing precedence over stored values.

## Testing

```bash
cargo test --manifest-path core/Cargo.toml --lib
npm run check:i18n
npm --workspace apps/desktop test
```

- The PipeWire capture integration test in `core/src/audio/linux/mod.rs` needs a live PipeWire session and skips itself when it cannot connect.
- That test also plays a test tone through `pw-play` (from `pipewire-bin` on Debian/Ubuntu) and skips when the command is missing. It creates its own null sink with a low session priority, so it does not take over the default device.
