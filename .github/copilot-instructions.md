# GitHub Copilot Instructions

This file provides repository-wide custom instructions for GitHub Copilot across all agents (VS Code inline, Copilot Chat, CLI, and cloud agents).

## Repository Purpose

This repository is **dtop**, a Swift terminal UI (in the spirit of `btop`/`htop`) for monitoring and managing Docker and Docker Compose. It runs on macOS, Linux and Windows. `README.md` covers features, keys, configuration and a per-platform feature matrix. `plan.md` covers status and next steps. `.github/plans/` holds architecture decisions.

## Project Conventions (dtop)

- **Zero third-party dependencies.** Use Foundation and the platform C libraries only (`Darwin`, `Glibc`/`Musl`, `WinSDK`/`ucrt`). Don't add Swift packages.
- **Module layout**:
  - `DtopCore`: Docker transports, HTTP client, API client and models, metrics math, GPU collectors.
  - `DtopUI`: rendering, widgets, themes, input parsing, terminal I/O.
  - `DtopApp`: state, reducer, views and runtime.
  - `dtop`: a one-line `main.swift`.
  - Keep that layering: `DtopUI` must not import `DtopCore`.
- **Pure core, thin runtime.**
  - `AppState.handle(_:)`, `ThemeEditor` and the views are pure functions of their inputs.
  - Side effects (Docker calls, file writes) go through `AppCommand`, which `AppController`/`DataRefresher` execute.
  - Docker access goes through the `DockerAPI` protocol so it can be faked in tests.
- **Cross-platform code**:
  - Guard platform APIs with `#if os(Windows)` / `#if canImport(Darwin)` / `#if canImport(Glibc)`.
  - Call libc functions through the specific module (`Darwin.read`, `Glibc.connect`), not `Foundation.`.
  - Win32 `BOOL` imports as `Bool` from Swift 6.2 and as `WindowsBool` before that; use the `win32Succeeded(_:)` helper.
  - C APIs that Swift's SDK modules don't import (OpenSSL on Linux; SSPI/SChannel and CNG on Windows) are loaded at runtime. Write their structs at explicit byte offsets (`RawStruct` in `SChannelTLS.swift`), never through assumed Swift layouts. See ADR 0003.
- **TLS and hosts**: everything above `DockerConnection` is TLS-agnostic; `TLSTransport` picks the platform backend. Host resolution (`DockerHostResolver`) follows the Docker CLI's precedence and must stay testable through `HostEnvironment`.
- **Stack size**: macOS secondary threads get 512 KB of stack. Avoid functions that copy large state structs many times in debug builds (CI runs the Linux tests with `ulimit -s 512` to catch this).
- **Swift 6 language mode** with strict concurrency. Prefer `Sendable` value types. Use `@unchecked Sendable` only with real internal locking.
- **Build and test**:
  - `swift build`
  - `swift test`: unit tests, no daemon needed. TLS tests run a real `openssl s_server` and are skipped without the `openssl` tool unless `DTOP_REQUIRE_TLS_TESTS=1` (CI sets it). `DTOP_TLS_TRACE=1` traces the TLS backends.
  - `DTOP_INTEGRATION=1 swift test --filter Integration`: needs a real daemon.

## Coding Guidelines

- Prefer clear, self-documenting code over excessive inline comments.
- Follow existing file and directory naming conventions in the repository.
- Write small, focused functions with a single responsibility.
- Add error handling for all I/O and network operations.
- Keep dependencies minimal — prefer standard library solutions when possible.

## Commit & Pull Request Guidelines

- Use [Conventional Commits](https://www.conventionalcommits.org/) format: `type(scope): description`
  - Common types: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `ci`
- Keep commits atomic — one logical change per commit.
- Reference issue numbers in commit messages and PR descriptions when applicable.
- Every PR should include a description of _what_ changed and _why_.

## Testing Guidelines

- Write tests alongside feature code in the matching `Tests/<Module>Tests` target, using Swift Testing (`import Testing`, `@Test`, `#expect`).
- Aim for meaningful test coverage of public APIs and critical paths.
- Tests should be deterministic and independent of each other.
- Use descriptive test names that explain the expected behavior.

## Documentation Guidelines

- Update `README.md` when adding new features or changing configuration.
- Document public APIs, configuration options, and non-obvious decisions.
- Place architectural decisions in `.github/plans/` as ADR (Architecture Decision Record) files.

## Security Guidelines

- Never commit secrets, credentials, or API keys — use GitHub Secrets or environment variables.
- Validate and sanitize all external inputs.
- Keep dependencies up to date; review security advisories before upgrading.

## Workflow & CI Guidelines

- All workflows live in `.github/workflows/`.
- The `copilot-setup-steps.yml` workflow configures the cloud agent environment.
- CI should run on every push to `main` and on all pull requests.
