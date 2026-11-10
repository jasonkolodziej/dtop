//! GPU metrics scraped from a Prometheus exporter running on a Docker host: NVIDIA's DCGM
//! Exporter or AMD's Device Metrics Exporter. dtop measures GPUs itself only on the machine it
//! runs on (ADR 0002); an exporter is what lets a *remote* host show its GPUs too. Only device
//! readings are used: the exporters attribute GPU use to Kubernetes pods, not Docker
//! containers, so per-container values stay unknown.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::error::DockerError;
use crate::gpu::{GpuCollecting, GpuDevice, GpuSample, GpuVendor};
use crate::host::TlsConfiguration;
use crate::http::{HttpClient, Method, Request};
use crate::tls::TlsTransport;
use crate::transport::{TcpTransport, Transport};

/// One sample of the Prometheus text format: `name{label="value",…} value [timestamp]`.
#[derive(Debug, Clone, PartialEq)]
pub struct PromSample {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub value: f64,
}

/// Parses the Prometheus text exposition format. Comments, `# HELP`/`# TYPE` lines and
/// malformed lines are skipped; label values are unescaped (`\\`, `\"`, `\n`).
pub fn parse_prometheus(text: &str) -> Vec<PromSample> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<PromSample> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let name_end = line
        .find(|c: char| c == '{' || c.is_whitespace())
        .unwrap_or(line.len());
    let name = &line[..name_end];
    if name.is_empty() {
        return None;
    }
    let mut rest = &line[name_end..];
    let mut labels = BTreeMap::new();
    if let Some(after) = rest.strip_prefix('{') {
        let mut chars = after.char_indices().peekable();
        let mut key = String::new();
        let close;
        loop {
            let (index, c) = chars.next()?;
            match c {
                '}' => {
                    close = index;
                    break;
                }
                ',' | ' ' => {}
                '=' => {
                    if chars.next()?.1 != '"' {
                        return None;
                    }
                    let mut value = String::new();
                    loop {
                        let (_, c) = chars.next()?;
                        match c {
                            '"' => break,
                            '\\' => match chars.next()?.1 {
                                'n' => value.push('\n'),
                                other => value.push(other),
                            },
                            other => value.push(other),
                        }
                    }
                    labels.insert(std::mem::take(&mut key), value);
                }
                other => key.push(other),
            }
        }
        rest = &after[close + 1..];
    }
    let value = rest.split_whitespace().next()?;
    let value = match value {
        "NaN" => return None,
        "+Inf" | "Inf" => f64::INFINITY,
        "-Inf" => f64::NEG_INFINITY,
        v => v.parse().ok()?,
    };
    Some(PromSample {
        name: name.to_string(),
        labels,
        value,
    })
}

const MIB: f64 = 1_048_576.0;

/// The first of `names` that a sample carries.
fn label<'a>(sample: &'a PromSample, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|n| sample.labels.get(*n))
        .map(String::as_str)
}

#[derive(Default)]
struct Readings {
    vendor: Option<GpuVendor>,
    name: Option<String>,
    utilization: Option<f64>,
    used: Option<f64>,
    free: Option<f64>,
    reserved: Option<f64>,
    total: Option<f64>,
    temperature: Option<f64>,
}

/// GPUs described by DCGM Exporter or AMD Device Metrics Exporter samples. Both DCGM's
/// current names (`DCGM_FI_DEV_GPU_UTIL_RATIO`, `…_FB_USED_BYTES`) and its older ones
/// (`DCGM_FI_DEV_GPU_UTIL` in %, `…_FB_USED` in MiB) are understood; AMD reports activity in %
/// and VRAM in MB (MiB).
pub fn devices_from_samples(samples: &[PromSample]) -> Vec<GpuDevice> {
    let mut devices: BTreeMap<String, Readings> = BTreeMap::new();
    for sample in samples {
        if !sample.value.is_finite() {
            continue;
        }
        let name = sample.name.to_ascii_uppercase();
        let v = sample.value;
        if let Some(field) = name.strip_prefix("DCGM_FI_DEV_") {
            let Some(id) = label(sample, &["UUID", "uuid", "gpu_uuid", "gpu"]) else {
                continue;
            };
            let entry = devices.entry(format!("nvidia:{id}")).or_default();
            entry.vendor = Some(GpuVendor::Nvidia);
            if entry.name.is_none() {
                entry.name = label(sample, &["modelName", "model_name"]).map(str::to_string);
            }
            match field {
                "GPU_UTIL" => entry.utilization = Some(v),
                "GPU_UTIL_RATIO" => entry.utilization = Some(v * 100.0),
                "FB_USED" => entry.used = Some(v * MIB),
                "FB_USED_BYTES" => entry.used = Some(v),
                "FB_FREE" => entry.free = Some(v * MIB),
                "FB_FREE_BYTES" => entry.free = Some(v),
                "FB_RESERVED" => entry.reserved = Some(v * MIB),
                "FB_RESERVED_BYTES" => entry.reserved = Some(v),
                "FB_CAPACITY_BYTES" => entry.total = Some(v),
                "GPU_TEMP" | "GPU_TEMP_CELSIUS" => entry.temperature = Some(v),
                _ => {}
            }
        } else if let Some(field) = name.strip_prefix("GPU_") {
            // AMD Device Metrics Exporter. Other exporters also use `gpu_*` names, so a sample
            // counts only with AMD's device labels.
            let Some(id) = label(sample, &["gpu_uuid", "gpu_id"]) else {
                continue;
            };
            if !matches!(
                field,
                "GFX_ACTIVITY"
                    | "USED_VRAM"
                    | "TOTAL_VRAM"
                    | "FREE_VRAM"
                    | "JUNCTION_TEMPERATURE"
                    | "EDGE_TEMPERATURE"
            ) {
                continue;
            }
            let entry = devices.entry(format!("amd:{id}")).or_default();
            entry.vendor = Some(GpuVendor::Amd);
            if entry.name.is_none() {
                entry.name = label(sample, &["card_model", "card_series"]).map(str::to_string);
            }
            match field {
                "GFX_ACTIVITY" => entry.utilization = Some(v),
                "USED_VRAM" => entry.used = Some(v * MIB),
                "TOTAL_VRAM" => entry.total = Some(v * MIB),
                "FREE_VRAM" => entry.free = Some(v * MIB),
                "JUNCTION_TEMPERATURE" => entry.temperature = Some(v),
                "EDGE_TEMPERATURE" => entry.temperature = entry.temperature.or(Some(v)),
                _ => {}
            }
        }
    }
    devices
        .into_iter()
        .map(|(id, r)| {
            let vendor = r.vendor.unwrap_or(GpuVendor::Unknown);
            let short = id.split_once(':').map_or(id.as_str(), |(_, rest)| rest);
            let name = r
                .name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| format!("{} GPU {short}", vendor.display_name()));
            let total = r.total.or_else(|| match (r.used, r.free) {
                (Some(used), Some(free)) => Some(used + free + r.reserved.unwrap_or(0.0)),
                _ => None,
            });
            let mut device = GpuDevice::new(&id, vendor, &name);
            device.utilization = r.utilization.map(|u| u.clamp(0.0, 100.0));
            device.memory_used = r.used.map(|b| b as u64);
            device.memory_total = total.map(|b| b as u64);
            device.temperature = r.temperature;
            device
        })
        .collect()
}

/// `http://host[:port][/path]` or `https://…`; the path defaults to `/metrics`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExporterUrl {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl ExporterUrl {
    pub fn parse(text: &str) -> Result<ExporterUrl, String> {
        let invalid = || {
            format!("'{text}' is not an exporter URL — use http://HOST:PORT/metrics (or https://)")
        };
        let (tls, rest) = if let Some(rest) = text.strip_prefix("http://") {
            (false, rest)
        } else if let Some(rest) = text.strip_prefix("https://") {
            (true, rest)
        } else {
            return Err(invalid());
        };
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, "/metrics"),
        };
        let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
            let (host, after) = bracketed.split_once(']').ok_or_else(invalid)?;
            (host, after.strip_prefix(':'))
        } else {
            match authority.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            }
        };
        if host.is_empty() || host.contains('@') {
            return Err(invalid());
        }
        let port = match port {
            Some(p) => p.parse::<u16>().map_err(|_| invalid())?,
            None if tls => 443,
            None => 80,
        };
        Ok(ExporterUrl {
            tls,
            host: host.to_string(),
            port,
            path: path.to_string(),
        })
    }
}

impl std::fmt::Display for ExporterUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let scheme = if self.tls { "https" } else { "http" };
        write!(f, "{scheme}://{host}:{}{}", self.port, self.path)
    }
}

/// Scrapes one exporter on each sample.
pub struct ExporterGpuCollector {
    pub url: ExporterUrl,
    http: HttpClient,
}

impl ExporterGpuCollector {
    /// Bound on connecting to and reading from the exporter.
    pub const TIMEOUT_SECONDS: u64 = 5;

    pub fn new(url: ExporterUrl) -> Self {
        let transport: Arc<dyn Transport> = if url.tls {
            Arc::new(TlsTransport::new(
                &url.host,
                url.port,
                TlsConfiguration {
                    verify_peer: true,
                    ..TlsConfiguration::default()
                },
                Self::TIMEOUT_SECONDS,
            ))
        } else {
            Arc::new(TcpTransport::new(
                &url.host,
                url.port,
                Self::TIMEOUT_SECONDS,
            ))
        };
        let mut http = HttpClient::new(transport);
        http.max_body_size = 16 * 1024 * 1024;
        ExporterGpuCollector { url, http }
    }

    fn fetch(&self) -> Result<String, String> {
        let request = Request::new(Method::Get, self.url.path.clone());
        match self.http.send(&request) {
            Ok(response) if response.head.is_success() => Ok(response.body_text()),
            Ok(response) => Err(format!(
                "HTTP {} {}",
                response.status(),
                response.head.reason
            )),
            Err(DockerError::ConnectionFailed { reason, .. }) => {
                Err(format!("cannot connect ({reason})"))
            }
            Err(DockerError::TimedOut(_)) => {
                Err(format!("no answer within {} s", Self::TIMEOUT_SECONDS))
            }
            Err(DockerError::Io(reason) | DockerError::Tls(reason)) => Err(reason),
            Err(other) => Err(other.to_string()),
        }
    }
}

impl GpuCollecting for ExporterGpuCollector {
    fn sample(&self, _now: f64) -> GpuSample {
        let mut sample = GpuSample::default();
        match self.fetch() {
            Ok(text) => {
                sample.devices = devices_from_samples(&parse_prometheus(&text));
                if sample.devices.is_empty() {
                    sample
                        .issues
                        .push(format!("no DCGM or AMD GPU metrics at {}", self.url));
                }
            }
            Err(reason) => sample
                .issues
                .push(format!("GPU metrics from {}: {reason}", self.url)),
        }
        // The exporters attribute GPU use to Kubernetes pods, not Docker containers.
        sample.process_attribution_complete = false;
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_prometheus_text_format() {
        let samples = parse_prometheus(
            "# HELP x help\n# TYPE x gauge\nx 1\ny{a=\"1\",b=\"q\\\"uote\\\\\"} 2.5 1700000000\n\
             z{} 3\nbad line here\nnan_metric NaN\n",
        );
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].name, "x");
        assert_eq!(samples[1].labels["a"], "1");
        assert_eq!(samples[1].labels["b"], "q\"uote\\");
        assert_eq!(samples[1].value, 2.5);
        assert_eq!(samples[2].value, 3.0);
    }

    #[test]
    fn reads_dcgm_exporter_with_old_and_new_metric_names() {
        // Older dcgm-exporter: % and MiB.
        let old = parse_prometheus(
            r#"DCGM_FI_DEV_GPU_UTIL{gpu="0",UUID="GPU-aaa",device="nvidia0",modelName="NVIDIA H100 80GB HBM3",Hostname="h"} 37
DCGM_FI_DEV_FB_USED{gpu="0",UUID="GPU-aaa",device="nvidia0",modelName="NVIDIA H100 80GB HBM3",Hostname="h"} 1024
DCGM_FI_DEV_FB_FREE{gpu="0",UUID="GPU-aaa",device="nvidia0",modelName="NVIDIA H100 80GB HBM3",Hostname="h"} 3072
DCGM_FI_DEV_GPU_TEMP{gpu="0",UUID="GPU-aaa",device="nvidia0",modelName="NVIDIA H100 80GB HBM3",Hostname="h"} 41
"#,
        );
        let devices = devices_from_samples(&old);
        assert_eq!(devices.len(), 1);
        let d = &devices[0];
        assert_eq!(d.id, "nvidia:GPU-aaa");
        assert_eq!(d.vendor, GpuVendor::Nvidia);
        assert_eq!(d.name, "NVIDIA H100 80GB HBM3");
        assert_eq!(d.utilization, Some(37.0));
        assert_eq!(d.memory_used, Some(1024 * 1_048_576));
        assert_eq!(d.memory_total, Some(4096 * 1_048_576));
        assert_eq!(d.temperature, Some(41.0));

        // Current dcgm-exporter: a ratio and bytes, two GPUs.
        let new = parse_prometheus(
            r#"DCGM_FI_DEV_GPU_UTIL_RATIO{gpu="0",UUID="GPU-aaa",modelName="RTX 4090"} 0.5
DCGM_FI_DEV_FB_USED_BYTES{gpu="0",UUID="GPU-aaa",modelName="RTX 4090"} 2000000000
DCGM_FI_DEV_FB_CAPACITY_BYTES{gpu="0",UUID="GPU-aaa",modelName="RTX 4090"} 24000000000
DCGM_FI_DEV_GPU_UTIL_RATIO{gpu="1",UUID="GPU-bbb",modelName="RTX 4090"} 0.25
"#,
        );
        let devices = devices_from_samples(&new);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].utilization, Some(50.0));
        assert_eq!(devices[0].memory_used, Some(2_000_000_000));
        assert_eq!(devices[0].memory_total, Some(24_000_000_000));
        assert_eq!(devices[1].utilization, Some(25.0));
        assert_eq!(devices[1].memory_total, None);
    }

    #[test]
    fn reads_amd_device_metrics_exporter() {
        let samples = parse_prometheus(
            r#"gpu_gfx_activity{gpu_id="0",card_model="MI300X",serial_number="s"} 88
gpu_used_vram{gpu_id="0",card_model="MI300X"} 65536
gpu_total_vram{gpu_id="0",card_model="MI300X"} 196608
gpu_junction_temperature{gpu_id="0",card_model="MI300X"} 70
gpu_edge_temperature{gpu_id="0",card_model="MI300X"} 55
gpu_power_usage{gpu_id="0"} 500
gpu_gfx_activity{instance="other exporter without AMD labels"} 3
"#,
        );
        let devices = devices_from_samples(&samples);
        assert_eq!(devices.len(), 1);
        let d = &devices[0];
        assert_eq!(d.vendor, GpuVendor::Amd);
        assert_eq!(d.name, "MI300X");
        assert_eq!(d.utilization, Some(88.0));
        assert_eq!(d.memory_used, Some(65536 * 1_048_576));
        assert_eq!(d.memory_total, Some(196_608 * 1_048_576));
        assert_eq!(d.temperature, Some(70.0));
    }

    #[test]
    fn parses_exporter_urls() {
        let u = ExporterUrl::parse("http://gpu-box:9400/metrics").unwrap();
        assert_eq!(
            (u.tls, u.host.as_str(), u.port, u.path.as_str()),
            (false, "gpu-box", 9400, "/metrics")
        );
        let u = ExporterUrl::parse("https://[::1]:5000").unwrap();
        assert_eq!(
            (u.tls, u.host.as_str(), u.port, u.path.as_str()),
            (true, "::1", 5000, "/metrics")
        );
        assert_eq!(u.to_string(), "https://[::1]:5000/metrics");
        assert_eq!(ExporterUrl::parse("http://h").unwrap().port, 80);
        assert!(ExporterUrl::parse("gpu-box:9400").is_err());
        assert!(ExporterUrl::parse("http://:9400").is_err());
        assert!(ExporterUrl::parse("http://h:x/metrics").is_err());
    }

    /// Serves one HTTP response to one connection.
    fn serve_once(status: &'static str, body: &'static str) -> u16 {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap() == 1 {
                head.push(byte[0]);
            }
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
        });
        port
    }

    #[test]
    fn collects_devices_from_a_live_exporter_and_reports_failures() {
        let port = serve_once(
            "200 OK",
            "DCGM_FI_DEV_GPU_UTIL{gpu=\"0\",UUID=\"GPU-x\",modelName=\"A100\"} 12\n",
        );
        let url = ExporterUrl::parse(&format!("http://127.0.0.1:{port}/metrics")).unwrap();
        let sample = ExporterGpuCollector::new(url).sample(0.0);
        assert_eq!(sample.devices.len(), 1);
        assert_eq!(sample.devices[0].utilization, Some(12.0));
        assert!(!sample.process_attribution_complete);
        assert!(sample.issues.is_empty(), "{:?}", sample.issues);

        let port = serve_once("404 Not Found", "");
        let url = ExporterUrl::parse(&format!("http://127.0.0.1:{port}/nope")).unwrap();
        let sample = ExporterGpuCollector::new(url).sample(0.0);
        assert!(sample.devices.is_empty());
        assert!(sample.issues[0].contains("HTTP 404"), "{:?}", sample.issues);

        let port = serve_once("200 OK", "node_cpu_seconds_total 1\n");
        let url = ExporterUrl::parse(&format!("http://127.0.0.1:{port}/metrics")).unwrap();
        let sample = ExporterGpuCollector::new(url).sample(0.0);
        assert!(
            sample.issues[0].starts_with("no DCGM or AMD GPU metrics"),
            "{:?}",
            sample.issues
        );

        // Nothing listening.
        let unused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = unused.local_addr().unwrap().port();
        drop(unused);
        let url = ExporterUrl::parse(&format!("http://127.0.0.1:{port}/metrics")).unwrap();
        let sample = ExporterGpuCollector::new(url).sample(0.0);
        assert!(
            sample.issues[0].contains("cannot connect"),
            "{:?}",
            sample.issues
        );
    }
}
