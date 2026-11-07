import Foundation
@testable import DtopCore

/// Realistic Engine API payloads, trimmed to the fields dtop reads plus some noise.
enum Fixtures {
    static let containerList = """
    [
      {
        "Id": "aaaaaaaaaaaa1111111111111111111111111111111111111111111111111111",
        "Names": ["/web-1"],
        "Image": "nginx:alpine",
        "ImageID": "sha256:img1",
        "Command": "nginx -g 'daemon off;'",
        "Created": 1700000000,
        "Ports": [
          {"IP": "0.0.0.0", "PrivatePort": 80, "PublicPort": 8080, "Type": "tcp"},
          {"IP": "::", "PrivatePort": 80, "PublicPort": 8080, "Type": "tcp"}
        ],
        "Labels": {
          "com.docker.compose.project": "shop",
          "com.docker.compose.service": "web",
          "com.docker.compose.container-number": "1",
          "com.docker.compose.project.working_dir": "/srv/shop",
          "com.docker.compose.project.config_files": "/srv/shop/compose.yaml,/srv/shop/compose.override.yaml"
        },
        "State": "running",
        "Status": "Up 2 hours (healthy)",
        "HostConfig": {"NetworkMode": "shop_default"},
        "Mounts": []
      },
      {
        "Id": "bbbbbbbbbbbb2222222222222222222222222222222222222222222222222222",
        "Names": ["/db"],
        "Image": "postgres:16",
        "ImageID": "sha256:img2",
        "Command": "docker-entrypoint.sh postgres",
        "Created": 1690000000,
        "Ports": null,
        "Labels": null,
        "State": "exited",
        "Status": "Exited (0) 3 days ago"
      }
    ]
    """

    static let containerInspect = """
    {
      "Id": "aaaaaaaaaaaa1111111111111111111111111111111111111111111111111111",
      "Created": "2024-05-01T10:00:00.123456789Z",
      "Path": "/docker-entrypoint.sh",
      "Args": ["nginx", "-g", "daemon off;"],
      "State": {
        "Status": "running", "Running": true, "Paused": false, "Restarting": false, "OOMKilled": false,
        "Dead": false, "Pid": 4242, "ExitCode": 0, "Error": "",
        "StartedAt": "2024-05-01T10:00:01.5Z", "FinishedAt": "0001-01-01T00:00:00Z",
        "Health": {"Status": "healthy", "FailingStreak": 0, "Log": []}
      },
      "Image": "sha256:img1",
      "Name": "/web-1",
      "RestartCount": 2,
      "HostConfig": {"RestartPolicy": {"Name": "unless-stopped", "MaximumRetryCount": 0}, "Memory": 536870912, "NanoCpus": 1500000000},
      "Mounts": [
        {"Type": "volume", "Name": "shop_data", "Source": "/var/lib/docker/volumes/shop_data/_data",
         "Destination": "/data", "Driver": "local", "Mode": "z", "RW": true},
        {"Type": "bind", "Source": "/srv/shop/nginx.conf", "Destination": "/etc/nginx/nginx.conf", "Mode": "ro", "RW": false}
      ],
      "Config": {
        "Hostname": "a1b2c3", "Env": ["PATH=/usr/bin", "NGINX_VERSION=1.25"], "Tty": false, "WorkingDir": "/",
        "Image": "nginx:alpine", "Labels": {"com.docker.compose.project": "shop"}
      },
      "NetworkSettings": {
        "Ports": {"80/tcp": [{"HostIp": "0.0.0.0", "HostPort": "8080"}], "443/tcp": null},
        "Networks": {
          "shop_default": {"IPAddress": "172.20.0.3", "Gateway": "172.20.0.1", "MacAddress": "02:42:ac:14:00:03"},
          "bridge": {"IPAddress": "172.17.0.2", "Gateway": "172.17.0.1", "MacAddress": ""}
        }
      }
    }
    """

    /// cgroup v2 Linux stats from a one-shot request (precpu empty).
    static func linuxStats(read: String, total: UInt64, system: UInt64, rx: UInt64 = 1_000, tx: UInt64 = 2_000,
                           blockRead: UInt64 = 4_096, blockWrite: UInt64 = 8_192) -> String {
        """
        {
          "read": "\(read)",
          "preread": "0001-01-01T00:00:00Z",
          "pids_stats": {"current": 7, "limit": 18446744073709551615},
          "blkio_stats": {"io_service_bytes_recursive": [
            {"major": 8, "minor": 0, "op": "read", "value": \(blockRead)},
            {"major": 8, "minor": 0, "op": "write", "value": \(blockWrite)}
          ]},
          "num_procs": 0,
          "cpu_stats": {
            "cpu_usage": {"total_usage": \(total), "usage_in_kernelmode": 10, "usage_in_usermode": 20},
            "system_cpu_usage": \(system),
            "online_cpus": 4,
            "throttling_data": {"periods": 0, "throttled_periods": 0, "throttled_time": 0}
          },
          "precpu_stats": {"cpu_usage": {"total_usage": 0, "usage_in_kernelmode": 0, "usage_in_usermode": 0}, "throttling_data": {}},
          "memory_stats": {"usage": 104857600, "limit": 1073741824, "stats": {"inactive_file": 4194304, "anon": 90000000}},
          "networks": {
            "eth0": {"rx_bytes": \(rx), "rx_packets": 10, "rx_errors": 0, "rx_dropped": 0, "tx_bytes": \(tx), "tx_packets": 12, "tx_errors": 0, "tx_dropped": 0},
            "eth1": {"rx_bytes": 0, "tx_bytes": 0}
          }
        }
        """
    }

    static let windowsStats = """
    {
      "read": "2024-05-01T10:00:02.000000000Z",
      "preread": "2024-05-01T10:00:01.000000000Z",
      "num_procs": 2,
      "cpu_stats": {"cpu_usage": {"total_usage": 15000000, "usage_in_kernelmode": 0, "usage_in_usermode": 0}},
      "precpu_stats": {"cpu_usage": {"total_usage": 10000000}},
      "memory_stats": {"commitbytes": 50000000, "commitpeakbytes": 60000000, "privateworkingset": 30000000},
      "storage_stats": {"read_count_normalized": 1, "read_size_bytes": 1000, "write_count_normalized": 2, "write_size_bytes": 2000},
      "networks": {"Ethernet": {"rx_bytes": 500, "tx_bytes": 700}}
    }
    """

    static let images = """
    [
      {"Id": "sha256:1111111111111111aaaa", "ParentId": "", "RepoTags": ["nginx:alpine", "nginx:1.25-alpine"],
       "RepoDigests": ["nginx@sha256:abc"], "Created": 1700000000, "Size": 43000000, "SharedSize": -1,
       "Containers": 1, "Labels": {"maintainer": "NGINX"}},
      {"Id": "sha256:2222222222222222bbbb", "ParentId": "", "RepoTags": null, "RepoDigests": ["postgres@sha256:def"],
       "Created": 1600000000, "Size": 400000000, "SharedSize": -1, "Containers": -1, "Labels": null},
      {"Id": "sha256:3333333333333333cccc", "RepoTags": ["<none>:<none>"], "RepoDigests": ["<none>@<none>"],
       "Created": 1500000000, "Size": 1000}
    ]
    """

    static let networks = """
    [
      {"Name": "bridge", "Id": "net1111111111111", "Created": "2024-01-01T00:00:00Z", "Scope": "local", "Driver": "bridge",
       "EnableIPv6": false, "IPAM": {"Driver": "default", "Options": null, "Config": [{"Subnet": "172.17.0.0/16", "Gateway": "172.17.0.1"}]},
       "Internal": false, "Attachable": false, "Containers": {}, "Labels": {}},
      {"Name": "shop_default", "Id": "net2222222222222", "Scope": "local", "Driver": "bridge",
       "IPAM": {"Config": [{"Subnet": "172.20.0.0/16"}]}, "Internal": true, "Attachable": true, "EnableIPv6": true,
       "Containers": {"aaaa": {"Name": "web-1"}, "cccc": {"Name": "worker"}},
       "Labels": {"com.docker.compose.project": "shop"}}
    ]
    """

    static let volumes = """
    {
      "Volumes": [
        {"Name": "shop_data", "Driver": "local", "Mountpoint": "/var/lib/docker/volumes/shop_data/_data",
         "CreatedAt": "2024-01-01T00:00:00Z", "Scope": "local", "Labels": {"com.docker.compose.project": "shop"},
         "UsageData": {"Size": 2048, "RefCount": 1}},
        {"Name": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "Driver": "local",
         "Mountpoint": "", "Scope": "local", "Labels": null, "UsageData": {"Size": -1, "RefCount": -1}}
      ],
      "Warnings": null
    }
    """

    static let version = """
    {"Version": "27.3.1", "ApiVersion": "1.47", "MinAPIVersion": "1.24", "Os": "linux", "Arch": "amd64",
     "KernelVersion": "6.10.0", "GitCommit": "ce12230", "Components": []}
    """

    static let info = """
    {"ID": "x", "Containers": 3, "ContainersRunning": 1, "ContainersPaused": 1, "ContainersStopped": 1, "Images": 12,
     "Name": "docker-host", "ServerVersion": "27.3.1", "OperatingSystem": "Ubuntu 24.04", "OSType": "linux",
     "Architecture": "x86_64", "NCPU": 8, "MemTotal": 16777216000}
    """

    static let top = """
    {"Titles": ["UID", "PID", "PPID", "C", "STIME", "TTY", "TIME", "CMD"],
     "Processes": [
       ["root", "100", "50", "0", "10:00", "?", "00:00:00", "nginx: master process nginx -g daemon off;"],
       ["101", "120", "100", "1", "10:00", "?", "00:00:01", "nginx: worker process"],
       ["101", "121", "100", "0", "10:00", "?", "00:00:00", "nginx: worker process"],
       ["root", "130", "121", "0", "10:01", "?", "00:00:00", "sh -c sleep 1000"]
     ]}
    """

    static func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try JSONDecoder().decode(T.self, from: Data(json.utf8))
    }

    static func container(
        _ name: String,
        id: String? = nil,
        state: ContainerState = .running,
        project: String? = nil,
        service: String? = nil,
        number: Int? = nil,
        status: String = "",
        image: String = "alpine",
        created: Int64 = 0,
        workingDir: String? = nil
    ) -> Container {
        var labels: [String: String] = [:]
        if let project { labels[ComposeLabel.project] = project }
        if let service { labels[ComposeLabel.service] = service }
        if let number { labels[ComposeLabel.containerNumber] = String(number) }
        if let workingDir { labels[ComposeLabel.workingDir] = workingDir }
        return Container(
            id: id ?? (name + String(repeating: "0", count: max(0, 64 - name.count))),
            names: ["/" + name],
            image: image,
            created: created,
            labels: labels,
            state: state,
            status: status
        )
    }
}
