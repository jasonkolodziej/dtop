# dtop

`dtop` is a Swift-based terminal application inspired by `btop`/`htop`, with a Docker and Docker Compose-first focus.

## Current status

This repository is in bootstrap mode.

- Project direction and roadmap: [`plan.md`](./plan.md)
- Initial Swift package scaffold is in place
- CI validates Swift build and tests

## Planned standout capabilities

- Intuitive, minimalistic interface
- Mouse interaction support
- Low resource footprint
- Real-time monitoring with visual flair
- Color-coded process/resource views centered on containers and compose projects
- Tree view for process relationships
- Extensive customization support (themes)
- Robust Docker management (containers, images, networks, volumes)

## Local development

```bash
swift build
swift test
swift run dtop
```
