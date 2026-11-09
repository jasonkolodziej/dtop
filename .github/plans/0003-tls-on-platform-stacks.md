# 0003 — TLS on each platform's own stack

**Status**: Accepted
**Date**: 2026-11-07

## Context

Remote Docker daemons listen on `tcp://host:2376` with `--tlsverify`. The Docker CLI connects with
a CA bundle, a client certificate and a key (`DOCKER_TLS_VERIFY`, `DOCKER_CERT_PATH`,
`--tlscacert/--tlscert/--tlskey`), and Docker contexts store the same material. dtop must do the
same on macOS, Linux and Windows. ADR 0001 rules out third-party dependencies, and Swift has no
cross-platform TLS in its standard library.

Facts that shaped the decision:

- `URLSession` is HTTP-level and on Linux/Windows (swift-corelibs-foundation) cannot present a
  client identity, so it cannot talk to a `--tlsverify` daemon.
- Every Linux distribution ships OpenSSL (`libssl.so.3` or `.1.1`). Its API is opaque pointers
  and C functions, so loading it with `dlopen` needs no hand-declared struct layouts — unlike
  NVML, the concern behind ADR 0002's option 2.
- macOS has Network.framework (TLS 1.3, verify blocks for custom trust). It needs a
  `SecIdentity`; the public ways to get one from PEM files go through a keychain.
  `SecIdentityCreate` builds one from an in-memory key and certificate; Security.framework
  exports it but does not declare it in its public headers.
- Windows has SChannel through SSPI. Swift's `WinSDK` module imports `wincrypt.h` but not
  `sspi.h`, `schannel.h` or `ncrypt.h`. SChannel signs client handshakes in LSASS, so it cannot
  use ephemeral CNG keys; the key must be persisted under a name.

## Options Considered

1. **Vendor a TLS library (BoringSSL, swift-nio-ssl)** — one implementation everywhere, but a
   large dependency and C build on every platform. Rejected by ADR 0001.
2. **`URLSession`** — no dependency, but no client certificates on Linux/Windows.
3. **Each platform's own TLS stack behind `DockerConnection` (chosen).**
   - Linux: OpenSSL via `dlopen`, driven through memory BIOs over dtop's own TCP connection, so
     timeouts and `interrupt()` keep working.
   - macOS: Network.framework, with `SecIdentityCreate` looked up at runtime (`dlsym`) and the
     server verified in a verify block anchored only to the configured CA.
   - Windows: SChannel over a new Winsock transport. SSPI and CNG are loaded at runtime and their
     few structs written at explicit byte offsets (`RawStruct`), never through guessed Swift
     layouts. Certificate checks use the typed `wincrypt` API with an exclusive-root chain
     engine. Client keys are imported as named CNG keys and deleted at exit.

## Decision

Option 3. `TLSTransport` picks the backend; everything above it (`HTTPClient`, `DockerClient`,
streaming) is unchanged. Shared, pure code handles PEM/DER parsing and key conversion (PKCS#1,
SEC1, PKCS#8, X9.63) and turns failures into messages that say what to change.

Host resolution follows the Docker CLI: `--host`, `DOCKER_HOST`, the active context, then local
sockets; TLS from `--tls`/`--tlsverify`, `DOCKER_TLS_VERIFY`/`DOCKER_TLS` or `https://`; files
from explicit paths, `DOCKER_CERT_PATH` or `~/.docker`. Contexts are read from
`~/.docker/contexts`, addressed by the SHA-256 of their names (a small SHA-256 is included,
since CryptoKit is Apple-only).

## Consequences

- No new dependencies. Linux needs `libssl` at runtime only when TLS is used.
- TLS 1.3 everywhere the OS supports it. On Windows that needs `SCH_CREDENTIALS` (Windows 10
  1809 and later; 1.3 itself on Windows 11 / Server 2022), with `TLS_PARAMETERS` disabling
  everything below 1.2. Older Windows rejects that structure, and dtop falls back to
  `SCHANNEL_CRED` (TLS 1.2). TLS 1.3 session tickets arrive as `SEC_I_RENEGOTIATE` from
  `DecryptMessage` and are fed back to `InitializeSecurityContext`. Every backend reports the
  negotiated version, and CI asserts 1.3 against a 1.3-only server on all three OSes.
- macOS relies on `SecIdentityCreate`, which is exported but undocumented. If a future macOS drops
  it, dtop reports that client certificates from files are unavailable; server-only TLS keeps
  working.
- Windows leaves a `dtop-<uuid>` key in the user's CNG store if dtop is killed before it can clean
  up.
- dtop opens one connection per request, so each request does a full handshake. If that proves
  costly for busy remote hosts, keep-alive or session resumption can be added inside the backends.
- Verification: every backend runs real handshakes against `openssl s_server` in CI (mutual TLS
  with EC and RSA keys, wrong CA, wrong name, insecure mode, missing and rejected client
  certificates). Linux CI also runs `dtop --once` against a `--tlsverify` docker:dind daemon,
  alone and together with the local socket.
