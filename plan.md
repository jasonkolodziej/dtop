# dtop Plan

## Vision
Build a Swift-based terminal UI inspired by `btop`/`htop`, focused on Docker and Docker Compose
observability and management, that runs on macOS, Linux and Windows with no third-party
dependencies.

## Product Goals
- Intuitive, minimal interface with keyboard and mouse support
- Low-overhead, real-time monitoring with clear visual hierarchy
- Container- and compose-centric process trees and colorized status views
- Theme and customization support
- Docker management workflows (containers, images, networks, volumes)

## Status by Phase

### Phase 0: Repository bootstrap ✅
- Swift package, CI, project direction
- VS Code launch configurations for debug and release builds (`.vscode/launch.json`)

### Phase 1: Core runtime and data layer ✅
- `DockerEndpoint`: `DOCKER_HOST` parsing and per-platform socket discovery
- Transports: Unix socket and TCP (POSIX), named pipe and TCP (Windows, Winsock), and TLS on top
  of TCP (Phase 6)
- Incremental HTTP/1.1 client (content-length, chunked, until-close, streaming with interrupt)
- `DockerClient`: version negotiation; containers, stats, top, logs and inspect; actions;
  images, networks and volumes, including prune; the `/events` stream
- Tolerant models for API differences: cgroup v1/v2 and Windows stats, `null` fields,
  numbers sent as strings
- `StatsCalculator`, `StatsSampler`, `MetricsStore` (ring buffers); `DataRefresher` actor with
  bounded-concurrency stats polling

### Phase 2: Terminal UI foundation ✅
- Screen buffer with wide-character handling, and a diff renderer that only emits changed cells
- Layout solver (fixed, percent and weighted fill); responsive placement of the details panel
- Themes: semantic roles, ten built-ins, a theme-file format, truecolor→256→16 downsampling,
  `NO_COLOR`
- Theme menu (`T`/`F2`): live preview on the running UI; per-role RGB nudging or typed colors;
  explicit save to `themes/<name>.theme` + `dtop.conf`, otherwise revert
- Input parser: keys, xterm modifiers, CSI u, SGR and X10 mouse, bracketed paste, ESC timing;
  raw mode via termios and the Windows console

### Phase 3: Monitoring views ✅
- Host CPU, memory and network braille graphs; per-container sparklines and rates
- Containers, Compose (project → container tree), Images, Networks and Volumes tabs
- Details panel: overview (inspect), processes (`top` as a tree), logs (demuxed, sanitized)
- Status and health color coding; text filter; running/stopped filter; seven sort keys
- GPU monitoring: host GPU graph plus per-container GPU%/memory for NVIDIA (`nvidia-smi`),
  AMD (amdgpu sysfs + DRM fdinfo) and Intel (DRM fdinfo) on Linux; NVIDIA host graph on Windows;
  Apple Silicon host graph on macOS (IOKit). See ADR 0002

### Phase 4: Management workflows ✅
- Start, stop, restart, pause, unpause, kill and remove for containers
- Compose start, stop, restart and pause through the Engine API; up and down through the
  `docker compose` CLI, using the project's labels
- Remove and prune for images, networks and volumes
- Confirmation dialogs for destructive actions (No focused by default, keyboard and mouse)

### Phase 5: Hardening 🚧
- ✅ Reconnect with exponential backoff while keeping the last known state on screen
- ✅ Actionable error messages throughout; a terminal-too-small fallback; signal-safe terminal
  restore
- ✅ 3-OS CI matrix (Linux, macOS, Windows); integration tests against a real daemon on Linux
- ✅ End-to-end checks of mouse input and the theme menu in a pseudo-terminal against a live daemon
- ✅ Real screenshots in `docs/screenshots/` (regenerate with `scripts/screenshots/`)
- ⏳ Interactive testing on real macOS and Windows terminals (CI covers build and unit tests only)
- ⏳ GPU monitoring on real NVIDIA, AMD, Intel and Apple hardware (verified so far with
  unit tests and a stand-in `nvidia-smi`)
- ⏳ Profiling on hosts with hundreds of containers (stats fan-out is capped at 8 concurrent
  requests)

### Phase 6: Remote and multi-host ✅
- ✅ TLS for `tcp://`/`https://` the Docker CLI way (`--tlsverify`, `DOCKER_TLS_VERIFY`,
  `DOCKER_CERT_PATH`, client certificates) on each platform's own stack — OpenSSL (Linux),
  Network.framework (macOS), SChannel (Windows) — see ADR 0003
- ✅ TLS 1.3 wherever the OS supports it, 1.2 otherwise. Windows uses `SCH_CREDENTIALS` (1.3 on
  Windows 11 / Server 2022) and falls back to `SCHANNEL_CRED` (1.2) before Windows 10 1809
- ✅ Plain `tcp://` on Windows (Winsock)
- ✅ Client keys: RSA and ECDSA (P-256, P-384, P-521) in PKCS#1, SEC1 or PKCS#8 PEM, parsed and
  converted in pure Swift; Windows imports them as named CNG keys and deletes them at exit
- ✅ Errors that say what to change: untrusted CA, wrong name, missing or rejected client
  certificate, a port that doesn't speak TLS, plain HTTP sent to a TLS daemon
- ✅ Docker contexts: the active context (`DOCKER_CONTEXT`, `currentContext`), `--context`
  (repeatable), `--all-contexts`; context directories found by SHA-256 of the name (built in);
  `ssh://` contexts are skipped with a notice
- ✅ Several hosts at once: repeatable `--host [NAME=]ENDPOINT`, header chips (● connected,
  ○ connecting, ✕ offline; clickable), `[`/`]`, host list (`H`), `[host NAME]` config sections,
  `--once` sections (exit 1 if any host is unreachable)
- ✅ Routing: details, logs and actions go to the host on screen; compose commands get that host's
  `--context`, or `--host` plus its TLS flags; GPU usage is attributed to the first local daemon
- ✅ Live log streaming (`logs?follow=1`): keeps the last 5,000 lines; ● live, ⏸ paused, ■ ended,
  ✕ stream lost; scrolling back pauses following, `f` or scrolling to the end resumes; pausing
  updates (space) freezes the pane; running containers reconnect, stopped ones aren't polled. The
  rules live in a pure, unit-tested `LogStreamPolicy`
- ✅ SIGPIPE is ignored (a daemon rejecting the client certificate used to kill the process on
  macOS); `DTOP_TLS_TRACE=1` traces the TLS backends
- ✅ Verified with real handshakes against `openssl s_server` on all three OSes (including
  TLS 1.3-only and 1.2-only servers, with the negotiated version asserted), `--once` against a
  `--tlsverify` docker:dind daemon in CI, and pseudo-terminal e2e runs (two hosts, logs over TLS)
- ⏳ The Windows `SCHANNEL_CRED` fallback on a real pre-1809 system (CI forces it on Server 2022)

## Next Up
- GPU: AMD and Intel on Windows via performance counters (`\GPU Engine(*)\Utilization Percentage`);
  optionally use AMD SMI / NVML directly (loaded at runtime) for richer metrics
- `exec` into a container (hand the TTY to `docker exec -it`)
- `ssh://` endpoints (spawn `ssh … docker system dial-stdio` like the Docker CLI)
- Keep-alive / TLS session resumption for remote hosts (one handshake per request today)
- An "all hosts" container view with a HOST column
- Per-container graphs in the details panel; a disk usage view (`/system/df`)
- Image pull with progress; persisting UI preferences (sort, collapsed projects)

## Architecture
Design decisions: [ADR 0001](.github/plans/0001-zero-dependency-cross-platform-architecture.md).
Per-platform support and how each feature was verified: the feature matrix in `README.md`.

Design decisions: also [ADR 0002](.github/plans/0002-gpu-monitoring-sources.md) (GPU sources) and
[ADR 0003](.github/plans/0003-tls-on-platform-stacks.md) (TLS on each platform's own stack).

- `Sources/DtopCore`: Docker transports, TLS backends, hosts and contexts, HTTP, API client,
  models, metrics, GPU collectors, compose
- `Sources/DtopUI`: rendering primitives, widgets, themes, input, terminal
- `Sources/DtopApp`: state and reducer, views, runtime (refresher and controller), configuration
- `Sources/dtop/main.swift`: the entry point
- `Tests/*`: unit suites per module, plus `IntegrationTests` (opt-in with `DTOP_INTEGRATION=1`)
