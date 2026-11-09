//! `dtop --once`: a plain-text snapshot with no escape codes and a stable layout, safe to pipe.
//! Everything that comes from the daemon (names, labels, versions, errors) passes through
//! `text::sanitize`, so a crafted label cannot emit escape sequences or forge report lines.

use dtop_core::format;
use dtop_ui::text::{self, Alignment};

use crate::list::sort_containers;
use crate::state::{ConnectionState, DockerSnapshot, SortKey, APP_VERSION};

/// One section per host, headed `== name ==`. A single host renders without a heading.
pub fn render_hosts(hosts: &[(String, DockerSnapshot)], width: usize) -> String {
    if hosts.len() <= 1 {
        return hosts
            .first()
            .map(|(_, s)| render(s, width))
            .unwrap_or_default();
    }
    hosts
        .iter()
        .map(|(name, s)| format!("== {} ==\n{}", text::sanitize(name), render(s, width)))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render(snapshot: &DockerSnapshot, width: usize) -> String {
    if let ConnectionState::Disconnected { message, .. } = &snapshot.connection {
        return format!("dtop: {}\n", text::sanitize(message));
    }
    let mut lines = Vec::new();
    let mut header = format!("dtop {APP_VERSION} · {}", snapshot.endpoint);
    if let Some(version) = &snapshot.version {
        header.push_str(&format!(
            " · docker {} (api {})",
            version.version, version.api_version
        ));
    }
    if let Some(info) = &snapshot.info {
        header.push_str(&format!(
            " · {} cpu · {}",
            info.cpu_count,
            format::bytes(info.memory_total as f64)
        ));
    }
    lines.push(header);
    let total = &snapshot.metrics.total_latest;
    lines.push(format!(
        "containers: {} running, {} stopped · cpu {} · mem {} · net ↓{} ↑{}",
        snapshot.running_count(),
        snapshot.stopped_count(),
        format::percent(total.cpu_percent, 1),
        format::bytes_u64(total.memory_usage),
        format::rate(total.network_rx_rate),
        format::rate(total.network_tx_rate)
    ));
    let gpu = &snapshot.metrics.gpu;
    for device in &gpu.devices {
        let mut line = format!("gpu: {}", device.name);
        if let Some(u) = device.utilization {
            line.push_str(&format!(" · {}", format::percent(u, 1)));
        }
        if let Some(used) = device.memory_used {
            line.push_str(&format!(" · {}", format::bytes_u64(used)));
            if let Some(t) = device.memory_total {
                line.push_str(&format!(" / {}", format::bytes_u64(t)));
            }
        }
        lines.push(line);
    }
    for issue in &gpu.issues {
        lines.push(format!("gpu: {issue}"));
    }
    lines.push(String::new());
    let show_gpu = !gpu.devices.is_empty();
    let mut columns: Vec<(&str, usize, Alignment)> = vec![
        ("NAME", 28, Alignment::Left),
        ("STATE", 10, Alignment::Left),
        ("CPU%", 7, Alignment::Right),
        ("MEM", 9, Alignment::Right),
        ("MEM%", 6, Alignment::Right),
        ("PROJECT", 16, Alignment::Left),
        ("IMAGE", 30, Alignment::Left),
    ];
    if show_gpu {
        columns.insert(5, ("GPU%", 6, Alignment::Right));
    }
    let row = |cells: &[String]| -> String {
        let text: Vec<String> = columns
            .iter()
            .zip(cells)
            .map(|((_, w, a), c)| text::fit(&text::sanitize(c), *w, *a))
            .collect();
        text::prefix(&text.join(" "), width)
            .trim_end_matches(' ')
            .to_string()
    };
    lines.push(row(&columns
        .iter()
        .map(|c| c.0.to_string())
        .collect::<Vec<_>>()));
    for container in sort_containers(snapshot.containers.clone(), snapshot, SortKey::Cpu, true) {
        let sample = snapshot
            .metrics
            .latest
            .get(&container.id)
            .filter(|_| container.state.is_active());
        let mut cells = vec![
            container.name(),
            container.state.raw_value().to_string(),
            sample.map_or_else(|| "-".into(), |s| format::fixed(s.cpu_percent, 1)),
            sample.map_or_else(|| "-".into(), |s| format::bytes_u64(s.memory_usage)),
            sample
                .filter(|s| s.memory_limit > 0)
                .map_or_else(|| "-".into(), |s| format::fixed(s.memory_percent(), 1)),
            container.compose_project().unwrap_or("").to_string(),
            container.image.clone(),
        ];
        if show_gpu {
            cells.insert(
                5,
                sample
                    .and_then(|s| s.gpu_percent)
                    .map_or_else(|| "-".into(), |g| format::fixed(g, 1)),
            );
        }
        lines.push(row(&cells));
    }
    if snapshot.containers.is_empty() {
        lines.push("(no containers)".into());
    }
    // Header, GPU names and issues come from the daemon and the GPU tools too.
    let lines: Vec<String> = lines.iter().map(|l| text::sanitize(l)).collect();
    lines.join("\n") + "\n"
}
