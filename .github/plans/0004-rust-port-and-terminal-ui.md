# 0004 — Rust port and terminal UI

**Status**: Accepted
**Date**: 2026-11-09

## Context

dtop's Swift implementation provides Docker monitoring and management on macOS, Linux and
Windows. A Rust implementation is useful for users who prefer Rust while retaining the same
cross-platform behavior, terminal experience, and zero-third-party-dependency constraint.
The port should preserve the existing module boundaries and avoid a GPU/windowing stack that
cannot render in a terminal.

## Options Considered

1. **Use a GPU-rendered native UI framework such as Rune.** It is designed for native windows,
   uses a substantial graphics/windowing stack, and has no terminal backend.
2. **Use a Rust TUI toolkit such as `ratatui` with `crossterm`.** This is a mature terminal UI
   option, but adds third-party dependencies and conflicts with the zero-dependency requirement.
3. **Port the terminal toolkit and application to Rust using only `std` and platform C APIs.**
   This preserves the dependency constraint and keeps the UI architecture under dtop's control.

## Decision

Choose option 3. Keep the Rust workspace in `rust/` with `dtop-core`, `dtop-ui`, `dtop-app`, and
the `dtop` executable. Mirror the Swift layering: `dtop-ui` must not depend on `dtop-core`,
application state and rendering stay separate from side effects, and platform-specific APIs are
declared or loaded at runtime as needed.

## Consequences

- The Rust application has no Cargo dependencies outside the workspace.
- The terminal toolkit, Docker client, and platform integrations require ongoing maintenance.
- Linux, macOS, and Windows builds and tests run in a separate Rust CI matrix.
- The Rust implementation is kept in sync with the Swift feature set; platform-only behavior
  still needs native CI or hardware verification.
