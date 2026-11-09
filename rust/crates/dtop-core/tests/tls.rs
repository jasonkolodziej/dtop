//! End-to-end TLS through the platform backend (OpenSSL on Linux, Network.framework on macOS,
//! SChannel on Windows) against a real `openssl s_server`. Each run mints a throwaway PKI.
//! Skipped without an openssl tool unless DTOP_REQUIRE_TLS_TESTS=1 (CI sets it, so the tests
//! cannot silently disappear).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dtop_core::host::{DockerHost, TlsConfiguration};
use dtop_core::http::{HttpClient, Method, Request, Response};
use dtop_core::tls::{client_for_host, TlsProtocolVersion, TlsTransport};
use dtop_core::{DockerApi, DockerEndpoint, DockerError};

fn openssl_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("DTOP_OPENSSL") {
        return Some(explicit.into());
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let separator = if cfg!(windows) { ';' } else { ':' };
    let name = if cfg!(windows) {
        "openssl.exe"
    } else {
        "openssl"
    };
    path.split(separator)
        .map(|d| Path::new(d).join(name))
        .chain(
            [
                r"C:\Program Files\Git\usr\bin\openssl.exe",
                r"C:\Program Files\OpenSSL\bin\openssl.exe",
                "/opt/homebrew/bin/openssl",
                "/usr/local/bin/openssl",
                "/usr/bin/openssl",
            ]
            .map(PathBuf::from),
        )
        .find(|p| p.is_file())
}

/// Whether to run; panics when required but unavailable.
fn available() -> bool {
    let required = std::env::var("DTOP_REQUIRE_TLS_TESTS").as_deref() == Ok("1");
    match openssl_path() {
        Some(_) => true,
        None if required => {
            panic!("openssl not found and DTOP_REQUIRE_TLS_TESTS=1; set DTOP_OPENSSL")
        }
        None => {
            eprintln!("skipping TLS tests: no openssl tool");
            false
        }
    }
}

fn openssl(arguments: &[&str], directory: &Path) {
    let output = Command::new(openssl_path().unwrap())
        .args(arguments)
        .current_dir(directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "openssl {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Pki {
    directory: PathBuf,
}

impl Pki {
    fn shared() -> Arc<Pki> {
        static PKI: OnceLock<Mutex<Weak<Pki>>> = OnceLock::new();
        let mut current = PKI.get_or_init(|| Mutex::new(Weak::new())).lock().unwrap();
        if let Some(pki) = current.upgrade() {
            return pki;
        }
        let pki = Arc::new(Pki::create());
        *current = Arc::downgrade(&pki);
        pki
    }

    fn create() -> Pki {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "dtop-rust-tls-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let pki = Pki { directory };
        let directory = &pki.directory;
        let write = |name: &str, text: &str| {
            std::fs::write(directory.join(name), format!("{text}\n")).unwrap()
        };
        write(
            "ca.cnf",
            "[req]\nprompt = no\ndistinguished_name = dn\n[dn]\nCN = dtop test CA\n[v3_ca]\nbasicConstraints = critical,CA:TRUE\nkeyUsage = critical,keyCertSign,cRLSign\nsubjectKeyIdentifier = hash",
        );
        for (name, common_name) in [
            ("server", "localhost"),
            ("client", "dtop client"),
            ("wrongname", "wrong.example"),
        ] {
            write(
                &format!("{name}.cnf"),
                &format!("[req]\nprompt = no\ndistinguished_name = dn\n[dn]\nCN = {common_name}"),
            );
        }
        let server_ext = "basicConstraints = CA:FALSE\nkeyUsage = critical,digitalSignature,keyEncipherment\nextendedKeyUsage = serverAuth";
        write(
            "server.ext",
            &format!("{server_ext}\nsubjectAltName = DNS:localhost,IP:127.0.0.1"),
        );
        write("dind.ext", "subjectAltName = DNS:localhost,IP:127.0.0.1");
        write(
            "wrongname.ext",
            &format!("{server_ext}\nsubjectAltName = DNS:wrong.example"),
        );
        write("client.ext", "basicConstraints = CA:FALSE\nkeyUsage = critical,digitalSignature\nextendedKeyUsage = clientAuth");
        let d = directory.as_path();
        for ca in ["ca", "rogue-ca"] {
            openssl(&["genrsa", "-out", &format!("{ca}.key"), "2048"], d);
            openssl(
                &[
                    "req",
                    "-x509",
                    "-new",
                    "-key",
                    &format!("{ca}.key"),
                    "-sha256",
                    "-days",
                    "30",
                    "-config",
                    "ca.cnf",
                    "-extensions",
                    "v3_ca",
                    "-out",
                    &format!("{ca}.pem"),
                ],
                d,
            );
        }
        let issue = |name: &str,
                     key: &str,
                     config: &str,
                     extensions: &str,
                     ca: &str,
                     serial: &str,
                     days: &str| {
            openssl(
                &[
                    "req",
                    "-new",
                    "-key",
                    key,
                    "-config",
                    config,
                    "-out",
                    &format!("{name}.csr"),
                ],
                d,
            );
            openssl(
                &[
                    "x509",
                    "-req",
                    "-in",
                    &format!("{name}.csr"),
                    "-CA",
                    &format!("{ca}.pem"),
                    "-CAkey",
                    &format!("{ca}.key"),
                    "-set_serial",
                    serial,
                    "-days",
                    days,
                    "-sha256",
                    "-extfile",
                    extensions,
                    "-out",
                    &format!("{name}.pem"),
                ],
                d,
            );
        };
        openssl(&["genrsa", "-out", "server.key", "2048"], d);
        issue(
            "server",
            "server.key",
            "server.cnf",
            "server.ext",
            "ca",
            "2",
            "30",
        );
        issue(
            "rogue-server",
            "server.key",
            "server.cnf",
            "server.ext",
            "rogue-ca",
            "3",
            "30",
        );
        issue(
            "wrongname",
            "server.key",
            "wrongname.cnf",
            "wrongname.ext",
            "ca",
            "4",
            "30",
        );
        issue(
            "dind-short",
            "server.key",
            "server.cnf",
            "dind.ext",
            "ca",
            "7",
            "30",
        );
        issue(
            "dind-long",
            "server.key",
            "server.cnf",
            "dind.ext",
            "ca",
            "8",
            "825",
        );
        issue(
            "server-long",
            "server.key",
            "server.cnf",
            "server.ext",
            "ca",
            "9",
            "825",
        );
        openssl(
            &[
                "ecparam",
                "-name",
                "prime256v1",
                "-genkey",
                "-noout",
                "-out",
                "client-ec.key",
            ],
            d,
        );
        issue(
            "client-ec",
            "client-ec.key",
            "client.cnf",
            "client.ext",
            "ca",
            "5",
            "30",
        );
        openssl(&["genrsa", "-out", "client-rsa.key", "2048"], d);
        issue(
            "client-rsa",
            "client-rsa.key",
            "client.cnf",
            "client.ext",
            "ca",
            "6",
            "30",
        );
        pki
    }

    fn path(&self, name: &str) -> String {
        self.directory.join(name).to_string_lossy().into_owned()
    }

    fn client(&self, key: &str) -> TlsConfiguration {
        TlsConfiguration {
            verify_peer: true,
            ca_file: Some(self.path("ca.pem")),
            certificate_file: Some(self.path(&format!("{key}.pem"))),
            key_file: Some(self.path(&format!("{key}.key"))),
            server_name: None,
        }
    }

    fn ca_only(&self) -> TlsConfiguration {
        TlsConfiguration {
            verify_peer: true,
            ca_file: Some(self.path("ca.pem")),
            ..Default::default()
        }
    }
}

impl Drop for Pki {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.directory) {
            eprintln!(
                "could not remove test PKI {}: {error}",
                self.directory.display()
            );
        }
    }
}

struct Server {
    port: u16,
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        // s_server children can ignore SIGTERM; kill() sends SIGKILL.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    fn start(pki: &Pki, certificate: &str, require_client: bool, extra: &[&str]) -> Server {
        for attempt in 0..6u32 {
            // OpenSSL 3 reports the port it picked for -accept 0; LibreSSL may not, so later
            // attempts use random high ports.
            let requested = if attempt == 0 {
                0
            } else {
                20_000 + (std::process::id() * 7 + attempt * 7919) % 40_000
            };
            let mut arguments = vec![
                "s_server".to_string(),
                "-accept".into(),
                requested.to_string(),
                "-cert".into(),
                pki.path(certificate),
                "-key".into(),
                pki.path("server.key"),
                "-www".into(),
            ];
            if require_client {
                arguments.extend([
                    "-CAfile".into(),
                    pki.path("ca.pem"),
                    "-Verify".into(),
                    "1".into(),
                    "-verify_return_error".into(),
                ]);
            }
            arguments.extend(extra.iter().map(|s| s.to_string()));
            let mut child = Command::new(openssl_path().unwrap())
                .args(&arguments)
                .current_dir(&pki.directory)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let stdout = child.stdout.take().unwrap();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if line.contains("ACCEPT") {
                        let _ = tx.send(line);
                    }
                }
            });
            if let Ok(line) = rx.recv_timeout(Duration::from_secs(10)) {
                let reported = line
                    .rsplit(':')
                    .next()
                    .and_then(|p| p.trim().parse::<u16>().ok());
                if let Some(port) = reported.or((requested != 0).then_some(requested as u16)) {
                    return Server { port, child };
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        panic!("s_server did not start");
    }
}

fn transport(server: &Server, host: &str, configuration: TlsConfiguration) -> TlsTransport {
    TlsTransport::new(host, server.port, configuration, 10)
}

fn get(transport: TlsTransport) -> Result<Response, DockerError> {
    HttpClient::new(std::sync::Arc::new(transport)).send(&Request::new(Method::Get, "/_ping"))
}

fn versioned_get(transport: &TlsTransport) -> (u16, Option<TlsProtocolVersion>) {
    let (connection, version) = transport.connect_tls().unwrap();
    connection
        .write_all(&Request::new(Method::Get, "/_ping").serialized("localhost"))
        .unwrap();
    let head = connection.read(1024).unwrap();
    connection.close();
    let status = String::from_utf8_lossy(&head)
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, version)
}

fn expect_failure(fragments: &[&str], result: Result<Response, DockerError>) {
    match result {
        Ok(response) => panic!("expected a TLS failure, got {}", response.status()),
        Err(error) => {
            let text = error.to_string();
            assert!(
                fragments.iter().any(|f| text.contains(f)),
                "unexpected error: {text}"
            );
        }
    }
}

#[test]
fn mutual_tls_with_ec_and_rsa_client_keys() {
    if !available() {
        return;
    }
    let pki = Pki::shared();
    let pki = &*pki;
    let server = Server::start(pki, "server.pem", true, &[]);
    for key in ["client-ec", "client-rsa"] {
        let response = get(transport(&server, "127.0.0.1", pki.client(key))).unwrap();
        assert_eq!(response.status(), 200, "{key}");
        assert!(
            response.body_text().contains("s_server") || response.body_text().contains("Cipher")
        );
    }
}

#[test]
fn temporary_certificates_are_removed_after_the_last_owner() {
    if !available() {
        return;
    }
    let pki = Pki::create();
    let directory = pki.directory.clone();
    assert!(directory.join("ca.pem").is_file());
    drop(pki);
    assert!(!directory.exists());
}

#[test]
fn docker_style_server_certificate_obeys_platform_usage_policy() {
    if !available() {
        return;
    }
    let pki = Pki::shared();
    for certificate in ["dind-short.pem", "dind-long.pem"] {
        let server = Server::start(&pki, certificate, false, &[]);
        let response = get(transport(&server, "localhost", pki.ca_only()));
        #[cfg(target_os = "macos")]
        expect_failure(&["Extended Key Usage serverAuth"], response);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(response.unwrap().status(), 200, "{certificate}");
    }
}

#[test]
fn server_certificate_with_valid_usage_and_long_validity_is_trusted() {
    if !available() {
        return;
    }
    let pki = Pki::shared();
    let server = Server::start(&pki, "server-long.pem", false, &[]);
    let response = get(transport(&server, "localhost", pki.ca_only())).unwrap();
    assert_eq!(response.status(), 200);
}

#[test]
fn tls13_and_tls12_are_negotiated_as_the_server_allows() {
    if !available() {
        return;
    }
    let pki = Pki::shared();
    let pki = &*pki;
    let tls13 = Server::start(pki, "server.pem", true, &["-tls1_3"]);
    for key in ["client-ec", "client-rsa"] {
        assert_eq!(
            versioned_get(&transport(&tls13, "127.0.0.1", pki.client(key))),
            (200, Some(TlsProtocolVersion::Tls13)),
            "{key}"
        );
    }
    let tls12 = Server::start(pki, "server.pem", true, &["-tls1_2"]);
    assert_eq!(
        versioned_get(&transport(&tls12, "127.0.0.1", pki.client("client-ec"))),
        (200, Some(TlsProtocolVersion::Tls12))
    );
}

#[cfg(all(windows, feature = "tls-test-support"))]
#[test]
fn legacy_schannel_uses_tls12_and_rejects_tls13_only_servers() {
    if !available() {
        return;
    }
    let pki = Pki::shared();
    let pki = &*pki;
    dtop_core::tls::with_legacy_schannel_for_test(|| {
        let tls12 = Server::start(pki, "server.pem", true, &["-tls1_2"]);
        for key in ["client-ec", "client-rsa"] {
            assert_eq!(
                versioned_get(&transport(&tls12, "127.0.0.1", pki.client(key))),
                (200, Some(TlsProtocolVersion::Tls12)),
                "{key}"
            );
        }
        let tls13 = Server::start(pki, "server.pem", true, &["-tls1_3"]);
        assert!(transport(&tls13, "127.0.0.1", pki.client("client-ec"))
            .connect_tls()
            .is_err());
    });
}

#[test]
fn server_name_override_and_repeated_handshakes() {
    if !available() {
        return;
    }
    let pki = Pki::shared();
    let pki = &*pki;
    let server = Server::start(pki, "wrongname.pem", false, &[]);
    let configuration = TlsConfiguration {
        server_name: Some("wrong.example".into()),
        ..pki.ca_only()
    };
    assert_eq!(
        get(transport(&server, "127.0.0.1", configuration))
            .unwrap()
            .status(),
        200
    );
    let mutual = Server::start(pki, "server.pem", true, &[]);
    for _ in 0..5 {
        assert_eq!(
            get(transport(&mutual, "localhost", pki.client("client-ec")))
                .unwrap()
                .status(),
            200
        );
    }
}

#[test]
fn docker_client_pings_over_tls() {
    if !available() {
        return;
    }
    let pki = Pki::shared();
    let pki = &*pki;
    let server = Server::start(pki, "server.pem", true, &[]);
    let mut host = DockerHost::new(
        "tls",
        DockerEndpoint::Tcp {
            host: "localhost".into(),
            port: server.port,
        },
    );
    host.tls = Some(pki.client("client-ec"));
    let client = client_for_host(&host, 10).unwrap();
    // s_server sends no API-Version header, so ping succeeds with None.
    assert_eq!(client.ping().unwrap(), None);
}

#[test]
fn untrusted_and_mismatched_server_certificates_are_rejected() {
    if !available() {
        return;
    }
    let pki = Pki::shared();
    let pki = &*pki;
    let rogue = Server::start(pki, "rogue-server.pem", false, &[]);
    expect_failure(
        &["could not verify the certificate"],
        get(transport(&rogue, "127.0.0.1", pki.ca_only())),
    );
    let wrong = Server::start(pki, "wrongname.pem", false, &[]);
    expect_failure(
        &["is not valid for '127.0.0.1'", "could not verify"],
        get(transport(&wrong, "127.0.0.1", pki.ca_only())),
    );
    // Without verification (--tls) any server certificate is accepted.
    let insecure = TlsConfiguration {
        verify_peer: false,
        ..Default::default()
    };
    assert_eq!(
        get(transport(&rogue, "127.0.0.1", insecure))
            .unwrap()
            .status(),
        200
    );
}

#[test]
fn client_certificate_problems_explain_what_to_do() {
    if !available() {
        return;
    }
    let pki = Pki::shared();
    let pki = &*pki;
    let server = Server::start(pki, "server.pem", true, &[]);
    expect_failure(
        &["client certificate", "--tlscert"],
        get(transport(&server, "127.0.0.1", pki.ca_only())),
    );
    let rejected = TlsConfiguration {
        certificate_file: Some(pki.path("rogue-server.pem")),
        key_file: Some(pki.path("server.key")),
        ..pki.ca_only()
    };
    expect_failure(
        &[
            "rejected the client certificate",
            "client certificate",
            "TLS",
        ],
        get(transport(&server, "127.0.0.1", rejected)),
    );
    let mismatched = TlsConfiguration {
        key_file: Some(pki.path("client-rsa.key")),
        ..pki.client("client-ec")
    };
    expect_failure(
        &["do not belong together", "client-rsa.key", "client"],
        get(transport(&server, "127.0.0.1", mismatched)),
    );
    let missing_ca = TlsConfiguration {
        ca_file: Some(pki.path("nope.pem")),
        ..Default::default()
    };
    expect_failure(
        &["nope.pem"],
        get(transport(
            &server,
            "127.0.0.1",
            TlsConfiguration {
                verify_peer: true,
                ..missing_ca
            },
        )),
    );
}
