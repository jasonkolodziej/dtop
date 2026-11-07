# 0001 — Zero-dependency, cross-platform architecture

**Status**: Accepted
**Date**: 2026-11-07

## Context

dtop has to build and run on macOS, Linux and Windows. It talks to the Docker Engine API, which
is served over a Unix socket on macOS/Linux, a named pipe on Windows (Docker Desktop), or
optionally plain-HTTP TCP. It also has to drive a full-screen terminal UI with mouse input on all
three platforms. The repository guidelines ask for minimal dependencies, and the project goal
includes a low resource footprint.

## Options Considered

1. **SwiftNIO / AsyncHTTPClient for HTTP, plus a TUI library.** These are mature, but they make
   builds heavier. NIO's Windows named-pipe support is weak, so the Windows path would need
   custom code anyway, and Swift TUI libraries rarely support Windows consoles.
2. **Shell out to the `docker` CLI and parse its output.** This is simple, but it needs the CLI
   installed, costs a process spawn per poll, and has no streaming events and fragile parsing.
3. **Zero third-party dependencies (chosen).** Hand-write a small HTTP/1.1 client over a
   pluggable `DockerTransport` (Unix socket and TCP via POSIX sockets, named pipe via `WinSDK`),
   and a small terminal toolkit (screen buffer, diff renderer, VT input parser, termios/Windows
   console raw mode).

## Decision

Option 3, structured as three libraries plus a thin executable:

- `DtopCore`: transports, an incremental HTTP/1.1 parser (content-length, chunked,
  until-close, streaming with interrupt), the Docker API client behind a `DockerAPI` protocol,
  tolerant models, and metrics math.
- `DtopUI`: colors, a screen buffer, the diff renderer, layout, widgets, themes, the input
  parser, and terminal I/O. It has no dependency on `DtopCore`.
- `DtopApp`: a pure `AppState` and reducer, the `ThemeEditor` state machine, and views as pure
  functions of state. Side effects run in `AppController` (main actor) and `DataRefresher` (an
  actor), via `AppCommand`.
- `dtop`: `main.swift` calls `DtopMain.run`.

The only external process dtop ever spawns is `docker compose`, and only for `up`/`down`, which
need the compose file.

## Consequences

- **Builds and tests on all three OSes from one codebase**, enforced by a 3-OS CI matrix. The
  pure core is tested without a terminal or a daemon (fakes for `DockerAPI`, the transport, the
  command runner and the file system), and a real-daemon integration suite runs on Linux.
- **We own protocol code.** The HTTP parser and input parser are tested at every byte-split
  point. Docker API differences between versions are absorbed by lenient decoding.
- **Platform quirks must be handled explicitly**:
  - libc calls are qualified per module.
  - Win32 `BOOL` imports as `Bool` from Swift 6.2 and as `WindowsBool` before that
    (`win32Succeeded(_:)` handles both).
  - Windows CI uses Swift 6.2, because Swift 6.0.3 cannot build against the runner's Windows SDK
    10.0.26100. The package still declares `swift-tools-version: 6.0`.
- **Debug-build stack size matters.** macOS secondary threads have 512 KB stacks, and large
  value types copied many times in one function can overflow them. CI runs the Linux tests with
  `ulimit -s 512` to catch this.
- **Follow-ups not covered yet**: TLS for `tcp://` endpoints, `ssh://` endpoints, TCP on Windows,
  and live log following. These are tracked in `plan.md`.
