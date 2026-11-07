# dtop

`dtop` is a Swift terminal UI inspired by `btop`/`htop`, built around Docker and Docker Compose:
live resource graphs, a compose-aware container tree, per-container processes and logs, and
start/stop/restart/kill/remove/prune workflows. It runs on macOS, Linux and Windows and has no
third-party dependencies.

```text
 dtop docker 29.9.2 · vm · 4 cpu · 16GiB                                                       ● 5 up  ◌ 2 down  12:47:06
╭─ cpu ──────────────── 27.4% of 4 cpu ─╮╭─ mem ──────────────── 351MiB / 16GiB ─╮╭─ net ────────────────── ↓0B/s ↑0B/s ─╮
│                                       ││                                       ││                                      │
│                                       ││                                       ││                                      │
│                                       ││                                       ││                                      │
│                                  ⣀⣀⣄⣀⣀││                                       ││                                      │
│                                  ⣿⣿⣿⣿⣿││                                 ⢀⣀⣀⣀⣀⣀││                                      │
╰───────────────────────────────────────╯╰───────────────────────────────────────╯╰──────────────────────────────────────╯
  1 Containers 7   2 Compose 1   3 Images 3   4 Networks 4   5 Volumes 0                                              all
╭─ compose 1 ────────────────────────────────────────────────────────╮╭─ demoapp-web-1 ──────────────────────────────────╮
│  NAME                STATE        CPU%     MEM  MEM% SERVICE       ││ Overview   Processes   Logs                      │
│▾ demoapp             4/4 up      101.0  8.0MiB       burner, web, …││PID      CPU  COMMAND                             │
│● ├─ demoapp-burner-1 running      99.6  596KiB   0.0 burner        ││3919    0    nginx: master process nginx -g daemon│
│● ├─ demoapp-web-1    running ✓     1.3  5.3MiB   0.0 web           ││3978    0    ├─ nginx: worker process             │
│● ├─ demoapp-worker-1 running       0.1  1.1MiB   0.0 worker        ││3980    0    ├─ nginx: worker process             │
│● └─ demoapp-worker-2 running       0.1  1.0MiB   0.0 worker        ││3981    0    ├─ nginx: worker process             │
│                                                                    ││3982    0    └─ nginx: worker process             │
╰────────────────────────────────────────────────────────────────────╯╰──────────────────────────────────────────────────╯
 q quit  ? help  / filter  ⏎ fold  u start  U up  x stop  r restart  D down  v pane  t theme
```

## Features

- **Real-time monitoring**: braille CPU, memory and network graphs for the whole host, plus CPU,
  memory, network and block I/O rates per container. The numbers use the same formulas as
  `docker stats`, covering cgroup v1, cgroup v2 and Windows containers.
- **Compose-first views**: containers are grouped into projects and services from their Compose
  labels. You can fold projects and see a roll-up status (running, partial, degraded, stopped).
- **Process trees**: the `docker top` output for each container, shown as a PID/PPID tree.
- **Logs**: the stdout/stderr tail with terminal escapes stripped and stderr highlighted.
- **Management**: start, stop, restart, pause, kill and remove containers. Compose stacks get
  start/stop/restart/pause plus `up`/`down` through the Compose CLI. You can remove or prune
  images, networks and volumes. Destructive actions ask for confirmation, and **No** is focused
  by default.
- **Keyboard and mouse**: vim and arrow keys, clickable tabs, rows and panes, and wheel scrolling.
- **Themes**: five built-in themes (`harbor`, `daylight`, `midnight`, `ansi`, `monochrome`) and a
  simple theme-file format. Truecolor is downsampled automatically to 256 or 16 colors, and
  `NO_COLOR` is honoured.
- **Low overhead**: the screen is diffed so only changed cells are redrawn. Metric history lives
  in fixed-size ring buffers, stats requests run with bounded concurrency, and the event stream
  triggers refreshes instead of tighter polling.
- **Resilient**: if the daemon goes away, dtop keeps the last known state on screen and
  reconnects with exponential backoff. Every error message says what to do next.

## Install and run

Swift 6.0 or newer is required: Xcode 16+ on macOS, or a swift.org toolchain on Linux and Windows.

```bash
swift build -c release
```

```bash
.build/release/dtop
```

dtop finds Docker in this order:

1. `--host`
2. `DOCKER_HOST`
3. Well-known sockets:
   - Linux: `/var/run/docker.sock`, then rootless `$XDG_RUNTIME_DIR/docker.sock`
   - macOS: Docker Desktop, OrbStack, Colima and Rancher Desktop sockets
   - Windows: the `\\.\pipe\docker_engine` named pipe

Supported endpoint forms are `unix://`, `npipe://` and plain-HTTP `tcp://`. For `ssh://` or TLS
endpoints, forward the socket locally first.

```text
dtop [options]
  -H, --host <endpoint>     unix:///var/run/docker.sock, npipe:////./pipe/docker_engine, tcp://host:2375
  -i, --interval <seconds>  refresh interval, 0.25-60 (default 2; accepts 500ms)
  -t, --theme <name|file>   built-in theme name or path to a .theme file
      --color <mode>        auto, truecolor, 256, 16 or none
      --running             only list running containers
      --sort <key>          name, cpu, memory, status, created, image
      --no-mouse            disable mouse reporting
      --history <samples>   samples kept per graph (30-3600)
  -c, --config <file>       settings file (default: see below)
      --once                print one plain-text snapshot and exit (scriptable)
      --list-themes / --print-theme <name>
  -h, --help / -v, --version
```

`dtop --once` prints a plain table with CPU% measured over one second. It works without a TTY, so
you can use it in scripts and CI.

## Keys

| Keys | Action |
| --- | --- |
| `↑↓` `j` `k`, `PgUp` `PgDn`, `g` `G` | move the selection |
| `←→` `Tab` `h` `l`, `1`–`5` | switch view (containers, compose, images, networks, volumes) |
| `Enter` / `d` / `v` | toggle details (Enter folds a compose project) / toggle details / cycle the overview, processes and logs panes |
| `/` | filter (`Esc` clears, `Ctrl-U` and `Ctrl-W` edit, paste works) |
| `s` / `S` / `a` | sort key / sort direction / all, running or stopped |
| `u` `x` `r` `p` `K` `D` | start/unpause, stop, restart, pause/unpause, kill, remove |
| `U` / `D` on a project | `docker compose up -d` / `docker compose down` |
| `X` | prune the current view's resource type |
| `t` / `space` / `R` | cycle theme / pause updates / refresh now |
| `?` / `q` `Ctrl-C` | help / quit |

## Configuration

Settings are read from `dtop.conf` in `$XDG_CONFIG_HOME/dtop`, `~/.config/dtop`, or
`%APPDATA%\dtop` on Windows. Command-line flags override the file.

```ini
# dtop.conf
host = unix:///var/run/docker.sock
interval = 1s
theme = midnight
color = auto
mouse = true
show_stopped = true
sort = cpu
history = 600
```

Custom themes go in `themes/<name>.theme` in the same directory. Start from a built-in theme:

```bash
dtop --print-theme harbor > ~/.config/dtop/themes/mine.theme
```

```ini
name = mine
inherits = harbor
accent = #4fb0f5
status_running = green
border_style = rounded   # rounded, single, double, heavy, ascii
```

## Architecture

| Module | Responsibility |
| --- | --- |
| `DtopCore` | Endpoint resolution; transports (Unix socket and TCP on POSIX, named pipe on Windows); an incremental HTTP/1.1 client; the Docker Engine API client and models; stats math; compose grouping and CLI invocation; process trees; ring buffers; backoff |
| `DtopUI` | Colors and styles; a screen buffer with wide-character handling; the diffing renderer; layout; widgets (box, table, gauge, braille graph, sparkline); themes; the VT/mouse/paste input parser; raw-mode terminal I/O |
| `DtopApp` | Pure `AppState` and reducer (input → state + commands); views; the `DataRefresher` actor; configuration; the `--once` report; the `AppController` event loop |
| `dtop` | `main.swift`, which calls `DtopMain.run` |

Views and the reducer are pure functions of state, so they are tested without a terminal or a
daemon. The `DockerAPI` protocol lets the runtime be tested against a fake.

## Development

```bash
swift build
```

```bash
swift test
```

```bash
DTOP_INTEGRATION=1 swift test --filter Integration
```

```bash
swift run dtop
```

`swift test` runs the full unit suite and needs no Docker daemon. The integration suite needs a
reachable daemon. It creates labelled `alpine` containers and removes them afterwards. CI builds
and tests on Linux, macOS and Windows, and runs the integration suite and `dtop --once` against
the Docker daemon on the Linux runner.

## Platform notes

- **Windows**: dtop talks to Docker Desktop over its named pipe and needs a VT-capable console
  (Windows Terminal, or Windows 10 1809 or newer). `tcp://` endpoints are not supported on
  Windows yet.
- **macOS**: when `DOCKER_HOST` is unset, dtop finds the Docker Desktop, OrbStack and Colima
  sockets automatically.
