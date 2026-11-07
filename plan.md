# dtop Plan

## Vision
Build a Swift-based terminal UI inspired by `btop`/`htop`, focused on Docker and Docker Compose observability and management.

## Product Goals
- Intuitive, minimal interface with keyboard + mouse support
- Low-overhead, real-time monitoring with clear visual hierarchy
- Container and compose-centric process tree and colorized status views
- Theme/customization support
- Docker management workflows (container/image/network/volume)

## Implementation Phases

### Phase 0 — Repository bootstrap (this change)
- Initialize Swift package structure
- Add baseline executable target and tests
- Add CI Swift build + test checks
- Document project direction and initial architecture

### Phase 1 — Core runtime + data layer
- Docker Engine API client abstraction
- Polling + event streaming loop for real-time updates
- Domain models for containers, compose projects, images, volumes, networks

### Phase 2 — Terminal UI foundation
- TUI layout grid (overview, lists, details, logs)
- Theme system and color palette abstraction
- Input handling (keyboard first, mouse support)

### Phase 3 — Monitoring views
- Resource charts (CPU, memory, IO, network)
- Container/compose process tree view
- Status-based color coding and filters

### Phase 4 — Management workflows
- Start/stop/restart/kill containers
- Compose stack operations
- Image/network/volume lifecycle operations
- Confirmation flows for destructive actions

### Phase 5 — Hardening
- Performance profiling and memory tuning
- Error handling and reconnect strategies
- Cross-platform validation (macOS/Linux)

## Initial Architecture
- `Sources/dtop/`:
  - `main.swift`: CLI entrypoint + app startup wiring
  - `AppProfile.swift`: project identity + capability model
- `Tests/dtopTests/`: baseline tests for package and capability metadata

## Exit Criteria for Bootstrap
- `swift build` and `swift test` pass in CI
- New contributors can understand scope from `plan.md` and `README.md`
