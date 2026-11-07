# Screenshots

The PNGs in `docs/screenshots/` are real captures of dtop running against a live Docker daemon.
No mock-ups are involved. `capture.py` drives the app in a pseudo-terminal and records every cell
(character, color, attributes). `render.py` turns those cells into framed PNGs.

## Regenerate

Requirements: Python 3 with `pyte`, `pillow` and `fonttools`, plus JetBrains Mono and DejaVu
fonts. Set `DTOP_FONT`, `DTOP_FONT_BOLD` and `DTOP_FONT_FALLBACKS` to use other font files, for
example on macOS.

```bash
pip install pyte pillow fonttools
```

```bash
docker compose -p shop -f scripts/screenshots/compose.yaml up -d
```

```bash
docker run -d --name cache alpine:3.20 sh -c 'while true; do sleep 1; done'
```

```bash
docker create --name migrate-db alpine:3.20 true
```

```bash
swift build
```

```bash
python3 scripts/screenshots/capture.py /tmp/dtop-caps -- .build/debug/dtop
```

```bash
python3 scripts/screenshots/render.py /tmp/dtop-caps /tmp/dtop-png
```

```bash
python3 scripts/screenshots/gallery.py /tmp/dtop-png docs/screenshots/themes.png
```

Copy the views you want from `/tmp/dtop-png` into `docs/screenshots/`. The capture takes about a
minute because it lets the graphs build up history first.
