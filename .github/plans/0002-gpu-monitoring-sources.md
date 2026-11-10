# 0002 — GPU monitoring data sources

**Status**: Accepted
**Date**: 2026-11-07

## Context

dtop needs a host GPU utilization graph and per-container GPU usage for NVIDIA, AMD, Intel and
Apple Silicon. The Docker Engine API exposes no GPU metrics, so dtop has to read them from the
machine it runs on. ADR 0001 rules out third-party build dependencies. Facts from primary
sources:

- **Linux DRM client usage stats** (`Documentation/gpu/drm-usage-stats.rst`). Every DRM file
  descriptor's `/proc/<pid>/fdinfo/<fd>` carries `drm-driver` and `drm-client-id`, plus:
  - `drm-engine-<name>` (busy nanoseconds), or `drm-cycles-<name>` / `drm-total-cycles-<name>`;
  - `drm-engine-capacity-<name>`;
  - per-region memory (`drm-resident-<region>`, where amdgpu's legacy `drm-memory-<region>` is
    an alias).

  Clients must be deduplicated by `drm-client-id` (scoped by `drm-pdev`), and counters that go
  backwards keep their previous value. amdgpu, i915 and xe implement this; the i915 and xe
  formats are documented with examples in the kernel tree.
- **amdgpu sysfs**: `gpu_busy_percent` (%, computed by SMU firmware) and
  `mem_info_vram_used` / `mem_info_vram_total` (bytes) under `/sys/class/drm/cardN/device/`.
  They are declared `S_IRUGO`, i.e. world-readable.
- **AMD libraries**: `rocm_smi_lib` receives only critical fixes since ROCm 7.0 and is no longer
  in TheRock builds after ROCm 10.1. AMD directs users to AMD SMI (`amdsmi_get_gpu_activity`,
  `amdsmi_get_gpu_vram_usage`, `amdsmi_get_gpu_process_list`).
- **Intel** has no sysfs busy-percent file. Utilization comes from DRM fdinfo, or from the i915
  PMU via `perf_event_open`, which needs `CAP_PERFMON`.
- **NVIDIA**: `nvidia-smi --query-gpu=… --format=csv,noheader,nounits`, `--query-compute-apps`
  and `pmon` give device, per-process memory and per-process SM utilization, on Linux and
  Windows.
- **Apple Silicon**: the IORegistry `IOAccelerator` service's `PerformanceStatistics` dictionary
  (`Device Utilization %`, `In use system memory`) is readable without root. It is undocumented,
  and its key names vary by GPU generation.
- **Container mapping**: `/proc/<pid>/cgroup` contains the 64-hex container ID for cgroup v1
  (`/docker/<id>`) and for cgroup v2 with systemd (`docker-<id>.scope`), as do Podman and
  Kubernetes paths.

## Options Considered

1. **Link vendor libraries (NVML, AMD SMI / rocm_smi_lib, IOKit).** Rich data, but NVML and AMD
   SMI become build dependencies, `rocm_smi_lib` is end-of-life, and Windows/macOS builds would
   need conditional packaging.
2. **Load NVML / AMD SMI at runtime with `dlopen`.** No build dependency, but we would be
   hand-declaring C struct layouts we can't test without hardware, and a mismatch crashes the
   process.
3. **Read what the kernel and vendor tools already publish (chosen).**
   - NVIDIA: `nvidia-smi` CSV output.
   - AMD: amdgpu sysfs counters (the same data ROCm SMI and AMD SMI read).
   - AMD and Intel per process: DRM fdinfo.
   - Apple: IOKit `IOAccelerator`, a system framework that needs no root.
   - Containers: `/proc/<pid>/cgroup`.

## Decision

Option 3, behind a `GPUCollecting` protocol:

- `SystemGPUCollector`:
  - Linux: NVIDIA via `nvidia-smi`; AMD and Intel via sysfs and DRM fdinfo; per-container
    attribution via cgroups.
  - Windows: NVIDIA host graph via `nvidia-smi`.
- `AppleGPUCollector` (macOS): IOKit, host graph only, because Docker Desktop's VM has no GPU
  access.
- All parsing is pure (`NvidiaSMI`, `DRMClient`, `DRMUsageTracker`, `CgroupContainerID`,
  `AppleGPUStatistics`) and tested with the kernel's documented examples and recorded output.
  File access goes through `GPUFileSystem`, so the collector runs against a fake `/sys` + `/proc`
  in tests.
- GPU collection is on by default; `--no-gpu` / `gpu = false` turns it off.

## Consequences

- No new dependencies, and dtop works without ROCm, CUDA toolkits or vendor SDKs installed. It
  needs only the driver and, for NVIDIA, `nvidia-smi` on `PATH`.
- **Per-container AMD/Intel needs root**, because reading other users' fdinfo requires it.
  Without root, attribution is marked incomplete: those containers show `-`, not `0`, and the
  GPU box says why.
- **Per-container attribution needs dtop on the Docker host.** It relies on the host PID
  namespace, so it does nothing for remote daemons.
- **Process cost**: `nvidia-smi` is started up to three times per refresh. If that proves too
  heavy, NVML via `dlopen` (option 2) can replace it behind the same protocol.
- **A hung driver can make `nvidia-smi` block**, and it runs inside the host refresh. The Rust
  version bounds each query to 5 s, kills it after that, keeps the last known GPUs on screen
  without readings, and skips `nvidia-smi` for 30 s, so a wedged driver cannot stall container
  monitoring. A killed process stuck in the driver (uninterruptible sleep) is reaped on its own
  thread, and no new query starts until it exits (amended 2026-11-09).
- **Intel host utilization depends on the same fdinfo access.** Intel has no busy-percent
  file, so device utilization and memory are sums over readable clients. When any process
  cannot be read, the Rust version reports them as unknown rather than an undercount, and the
  GPU box says root is needed. Group membership (`render`, `video`) does not help: fdinfo is
  guarded by ptrace access checks, not by the device node's permissions (amended 2026-11-09).
- **ROCm compute bypasses fdinfo engine time.** KFD user-mode queues don't go through the DRM
  scheduler, so ROCm work shows in the device's `gpu_busy_percent` but not in per-process
  `drm-engine-*`. The Rust version also reads amdkfd's world-readable
  `/sys/class/kfd/kfd/proc/<pid>/` (`vram_<gpu_id>`, `stats_<gpu_id>/cu_occupancy`), maps
  `gpu_id` to a PCI address through the KFD topology (`domain`, `location_id`), and turns CU
  occupancy into a share of the GPU's compute units. It is a snapshot of occupancy, not busy
  time (amended 2026-11-10).
- **NVIDIA graphics processes**: `--query-compute-apps` lists compute processes only. The Rust
  version runs `pmon -s um` and takes graphics processes' memory from its `fb` column
  (amended 2026-11-10).
- **Remote hosts through exporters**: the Rust version can read a host's GPUs from a DCGM
  Exporter or AMD Device Metrics Exporter (`--gpu-metrics`, `gpu_metrics =`), parsing the
  Prometheus text format itself. It reads device readings only, because the exporters attribute
  usage to Kubernetes pods, not Docker containers. This lifts the "no GPU data for remote
  daemons" limit for hosts that run an exporter (amended 2026-11-10).
- **Not covered yet**: AMD and Intel on Windows (would use `\GPU Engine(*)\Utilization Percentage`
  performance counters), and GPU power and temperature beyond NVIDIA's.
- **Verification so far**: unit tests and an end-to-end run with a stand-in `nvidia-smi`. Real
  hardware is still needed for all four vendors.
