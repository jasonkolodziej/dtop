//! TLS on Windows through SChannel (SSPI) over a Winsock TCP connection.
//!
//! - Credentials use `SCH_CREDENTIALS` with everything below TLS 1.2 disabled, so Windows 11 and
//!   Server 2022 negotiate TLS 1.3; Windows before 10 1809 rejects that structure and dtop falls
//!   back to `SCHANNEL_CRED` (TLS 1.2).
//! - Validation is manual, so exactly the configured CA is trusted (an exclusive-root chain
//!   engine), with the SSL policy checking the server name.
//! - Client keys are imported as named CNG keys (SChannel cannot use ephemeral ones) and deleted
//!   when dtop exits.
//! - TLS 1.3 session tickets arrive as SEC_I_RENEGOTIATE from DecryptMessage and are fed back to
//!   InitializeSecurityContext; close_notify (SEC_I_CONTEXT_EXPIRED) ends the stream.
//!
//! The structs below follow the 64-bit Windows SDK layouts; their sizes are asserted at compile
//! time.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::pem::{certificates, PrivateKey};
use super::{
    closed_during_handshake_hint, handshake_message, name_mismatch_message, read_file, trace,
    untrusted_message, TlsProtocolVersion,
};
use crate::error::{DockerError, Result};
use crate::host::TlsConfiguration;
use crate::transport::Connection;

type Ptr = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Handle {
    lower: usize,
    upper: usize,
}

impl Handle {
    fn ptr(&mut self) -> *mut Handle {
        self
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SecBuffer {
    length: u32,
    kind: u32,
    data: Ptr,
}

#[repr(C)]
struct SecBufferDesc {
    version: u32,
    count: u32,
    buffers: *mut SecBuffer,
}

impl SecBuffer {
    const fn empty(kind: u32) -> SecBuffer {
        SecBuffer {
            length: 0,
            kind,
            data: std::ptr::null_mut(),
        }
    }
}

#[repr(C)]
struct SchCredentials {
    version: u32,
    cred_format: u32,
    creds: u32,
    certificates: *mut Ptr,
    root_store: Ptr,
    mappers: u32,
    mapper_list: Ptr,
    session_lifespan: u32,
    flags: u32,
    tls_parameters_count: u32,
    tls_parameters: *mut TlsParameters,
}

#[repr(C)]
struct TlsParameters {
    alpn_count: u32,
    alpn_ids: Ptr,
    disabled_protocols: u32,
    disabled_crypto_count: u32,
    disabled_crypto: Ptr,
    flags: u32,
}

#[repr(C)]
struct SchannelCred {
    version: u32,
    creds: u32,
    certificates: *mut Ptr,
    root_store: Ptr,
    mappers: u32,
    mapper_list: Ptr,
    supported_algorithms: u32,
    algorithms: Ptr,
    enabled_protocols: u32,
    minimum_cipher_strength: u32,
    maximum_cipher_strength: u32,
    session_lifespan: u32,
    flags: u32,
    cred_format: u32,
}

#[repr(C)]
#[derive(Default)]
struct StreamSizes {
    header: u32,
    trailer: u32,
    maximum_message: u32,
    buffers: u32,
    block_size: u32,
}

#[repr(C)]
struct CertContext {
    encoding: u32,
    encoded: *mut u8,
    encoded_length: u32,
    info: Ptr,
    store: Ptr,
}

#[repr(C)]
struct KeyProvInfo {
    container: *mut u16,
    provider: *mut u16,
    provider_type: u32,
    flags: u32,
    parameter_count: u32,
    parameters: Ptr,
    key_spec: u32,
}

#[repr(C)]
struct ChainEngineConfig {
    size: u32,
    restricted_root: Ptr,
    restricted_trust: Ptr,
    restricted_other: Ptr,
    additional_store_count: u32,
    additional_stores: Ptr,
    flags: u32,
    url_retrieval_timeout: u32,
    maximum_cached_certificates: u32,
    cycle_detection_modulus: u32,
    exclusive_root: Ptr,
    exclusive_trusted_people: Ptr,
    exclusive_flags: u32,
}

#[repr(C)]
struct ChainPara {
    size: u32,
    usage_type: u32,
    usage_count: u32,
    usage_identifiers: Ptr,
}

#[repr(C)]
struct PolicyPara {
    size: u32,
    flags: u32,
    extra: Ptr,
}

#[repr(C)]
struct PolicyStatus {
    size: u32,
    error: u32,
    chain_index: i32,
    element_index: i32,
    extra: Ptr,
}

#[repr(C)]
struct SslPolicyPara {
    size: u32,
    auth_type: u32,
    checks: u32,
    server_name: *mut u16,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<SecBuffer>() == 16);
    assert!(std::mem::size_of::<SecBufferDesc>() == 16);
    assert!(std::mem::size_of::<SchCredentials>() == 72);
    assert!(std::mem::size_of::<TlsParameters>() == 40);
    assert!(std::mem::size_of::<SchannelCred>() == 80);
    assert!(std::mem::size_of::<CertContext>() == 40);
    assert!(std::mem::size_of::<KeyProvInfo>() == 48);
    assert!(std::mem::size_of::<ChainEngineConfig>() == 88);
    assert!(std::mem::size_of::<ChainPara>() == 24);
    assert!(std::mem::size_of::<PolicyPara>() == 16);
    assert!(std::mem::size_of::<PolicyStatus>() == 24);
    assert!(std::mem::size_of::<SslPolicyPara>() == 24);
};

#[link(name = "secur32")]
extern "system" {
    fn AcquireCredentialsHandleW(
        principal: *mut u16,
        package: *mut u16,
        usage: u32,
        logon_id: Ptr,
        auth_data: Ptr,
        get_key: Ptr,
        get_key_argument: Ptr,
        credential: *mut Handle,
        expiry: Ptr,
    ) -> i32;
    fn InitializeSecurityContextW(
        credential: *mut Handle,
        context: *mut Handle,
        target: *mut u16,
        requirements: u32,
        reserved1: u32,
        representation: u32,
        input: *mut SecBufferDesc,
        reserved2: u32,
        new_context: *mut Handle,
        output: *mut SecBufferDesc,
        attributes: *mut u32,
        expiry: Ptr,
    ) -> i32;
    fn QueryContextAttributesW(context: *mut Handle, attribute: u32, buffer: Ptr) -> i32;
    fn EncryptMessage(
        context: *mut Handle,
        qop: u32,
        message: *mut SecBufferDesc,
        sequence: u32,
    ) -> i32;
    fn DecryptMessage(
        context: *mut Handle,
        message: *mut SecBufferDesc,
        sequence: u32,
        qop: *mut u32,
    ) -> i32;
    fn FreeContextBuffer(buffer: Ptr) -> i32;
    fn DeleteSecurityContext(context: *mut Handle) -> i32;
    fn FreeCredentialsHandle(credential: *mut Handle) -> i32;
}

#[link(name = "ncrypt")]
extern "system" {
    fn NCryptOpenStorageProvider(provider: *mut usize, name: *const u16, flags: u32) -> i32;
    fn NCryptImportKey(
        provider: usize,
        import_key: usize,
        blob_type: *const u16,
        parameters: *mut SecBufferDesc,
        key: *mut usize,
        data: *const u8,
        length: u32,
        flags: u32,
    ) -> i32;
    fn NCryptDeleteKey(key: usize, flags: u32) -> i32;
    fn NCryptFreeObject(object: usize) -> i32;
}

#[link(name = "crypt32")]
extern "system" {
    fn CertCreateCertificateContext(
        encoding: u32,
        encoded: *const u8,
        length: u32,
    ) -> *mut CertContext;
    fn CertFreeCertificateContext(context: *const CertContext) -> i32;
    fn CertSetCertificateContextProperty(
        context: *const CertContext,
        property: u32,
        flags: u32,
        data: *const c_void,
    ) -> i32;
    fn CertOpenStore(
        provider: usize,
        encoding: u32,
        crypt_provider: usize,
        flags: u32,
        parameter: *const c_void,
    ) -> Ptr;
    fn CertCloseStore(store: Ptr, flags: u32) -> i32;
    fn CertAddEncodedCertificateToStore(
        store: Ptr,
        encoding: u32,
        encoded: *const u8,
        length: u32,
        disposition: u32,
        added: *mut *mut CertContext,
    ) -> i32;
    fn CertCreateCertificateChainEngine(config: *const ChainEngineConfig, engine: *mut Ptr) -> i32;
    fn CertFreeCertificateChainEngine(engine: Ptr);
    fn CertGetCertificateChain(
        engine: Ptr,
        certificate: *const CertContext,
        time: Ptr,
        additional_store: Ptr,
        parameters: *const ChainPara,
        flags: u32,
        reserved: Ptr,
        chain: *mut Ptr,
    ) -> i32;
    fn CertFreeCertificateChain(chain: Ptr);
    fn CertVerifyCertificateChainPolicy(
        policy: usize,
        chain: Ptr,
        parameters: *const PolicyPara,
        status: *mut PolicyStatus,
    ) -> i32;
}

extern "C" {
    fn atexit(callback: extern "C" fn()) -> i32;
}

const CREDENTIALS_OUTBOUND: u32 = 2;
const SCHANNEL_CRED_VERSION: u32 = 4;
const SCH_CREDENTIALS_VERSION: u32 = 5;
const PROTOCOL_TLS12_CLIENT: u32 = 0x0000_0800;
const PROTOCOL_TLS13_CLIENT: u32 = 0x0000_2000;
/// SSL 2, SSL 3, TLS 1.0 and TLS 1.1 (client): what SCH_CREDENTIALS disables.
const PROTOCOLS_BELOW_TLS12_CLIENT: u32 = 0x0000_0008 | 0x0000_0020 | 0x0000_0080 | 0x0000_0200;
const CRED_MANUAL_VALIDATION: u32 = 0x0000_0008;
const CRED_NO_DEFAULT_CREDS: u32 = 0x0000_0010;
const USE_STRONG_CRYPTO: u32 = 0x0040_0000;
const CONTEXT_REQUIREMENTS: u32 = 0x8 | 0x4 | 0x10 | 0x100 | 0x8000 | 0x4000 | 0x80 | 0x80000;
const NATIVE_DATA_REPRESENTATION: u32 = 0x10;
const BUFFER_EMPTY: u32 = 0;
const BUFFER_DATA: u32 = 1;
const BUFFER_TOKEN: u32 = 2;
const BUFFER_EXTRA: u32 = 5;
const BUFFER_STREAM_TRAILER: u32 = 6;
const BUFFER_STREAM_HEADER: u32 = 7;
const ATTRIBUTE_STREAM_SIZES: u32 = 4;
const ATTRIBUTE_REMOTE_CERTIFICATE: u32 = 0x53;
const ATTRIBUTE_CONNECTION_INFO: u32 = 0x5A;
const SEC_E_OK: i32 = 0;
const SEC_I_CONTINUE_NEEDED: i32 = 0x0009_0312;
const SEC_I_INCOMPLETE_CREDENTIALS: i32 = 0x0009_0320;
const SEC_I_CONTEXT_EXPIRED: i32 = 0x0009_0317;
const SEC_I_RENEGOTIATE: i32 = 0x0009_0321;
const SEC_E_INCOMPLETE_MESSAGE: i32 = 0x8009_0318_u32 as i32;
const X509_PKCS7_ENCODING: u32 = 0x0000_0001 | 0x0001_0000;
const CERT_KEY_PROV_INFO_PROP_ID: u32 = 2;
const CERT_STORE_PROV_MEMORY: usize = 2;
const CERT_STORE_ADD_ALWAYS: u32 = 4;
const CERT_CHAIN_POLICY_SSL: usize = 4;
const AUTHTYPE_SERVER: u32 = 2;
const NCRYPTBUFFER_PKCS_KEY_NAME: u32 = 45;
const NCRYPT_OVERWRITE_KEY_FLAG: u32 = 0x80;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A readable name for an SSPI status, worded so `handshake_message` can classify it.
fn describe(status: i32) -> String {
    let code = status as u32;
    let hex = format!("0x{code:X}");
    match code {
        0x8009_0308 => format!("wrong version number: the server did not answer with TLS (SEC_E_INVALID_TOKEN, {hex})"),
        0x8009_0326 => format!("the server sent a handshake failure alert (SEC_E_ILLEGAL_MESSAGE, {hex})"),
        0x8009_0327 => format!("certificate unknown (SEC_E_CERT_UNKNOWN, {hex})"),
        0x8009_0325 => format!("untrusted root (SEC_E_UNTRUSTED_ROOT, {hex})"),
        0x8009_0322 => format!("wrong principal (SEC_E_WRONG_PRINCIPAL, {hex})"),
        0x8009_0331 => format!("no common protocol or cipher (SEC_E_ALGORITHM_MISMATCH, {hex})"),
        0x8009_030E => format!("no credentials (SEC_E_NO_CREDENTIALS, {hex})"),
        0x8009_030D => format!("unknown credentials (SEC_E_UNKNOWN_CREDENTIALS, {hex})"),
        0x8009_0304 => format!("internal error (SEC_E_INTERNAL_ERROR, {hex})"),
        0x8009_0027 => format!("the client key could not be used with the client certificate — do they belong together? ({hex})"),
        _ => format!("SSPI status {hex}"),
    }
}

/// How credentials were acquired: `SCH_CREDENTIALS` (TLS 1.2 and 1.3) or `SCHANNEL_CRED` (1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Modern,
    Legacy,
}

#[cfg(any(test, feature = "tls-test-support"))]
std::thread_local! {
    static FORCE_LEGACY_CREDENTIALS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn force_legacy_credentials() -> bool {
    #[cfg(any(test, feature = "tls-test-support"))]
    {
        FORCE_LEGACY_CREDENTIALS.with(std::cell::Cell::get)
    }
    #[cfg(not(any(test, feature = "tls-test-support")))]
    {
        false
    }
}

#[cfg(any(test, feature = "tls-test-support"))]
pub(super) fn with_legacy_credentials<T>(test: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            FORCE_LEGACY_CREDENTIALS.with(|value| value.set(self.0));
        }
    }
    let previous = FORCE_LEGACY_CREDENTIALS.with(|value| value.replace(true));
    let _reset = Reset(previous);
    test()
}

/// Credentials for one configuration, shared by every connection (dtop opens one per request).
struct Credentials {
    handle: Mutex<Handle>,
    style: Style,
    certificate: usize,
    key: usize,
}

static CREDENTIALS: OnceLock<Mutex<HashMap<String, Arc<Credentials>>>> = OnceLock::new();

extern "C" fn release_all_credentials() {
    let Some(cache) = CREDENTIALS.get() else {
        return;
    };
    let all: Vec<Arc<Credentials>> = cache
        .lock()
        .map(|mut c| c.drain().map(|(_, v)| v).collect())
        .unwrap_or_default();
    for credentials in all {
        credentials.release();
    }
}

impl Credentials {
    fn shared(configuration: &TlsConfiguration) -> Result<Arc<Credentials>> {
        let legacy = force_legacy_credentials();
        let key = format!(
            "{}\0{}\0{}",
            configuration.certificate_file.as_deref().unwrap_or(""),
            configuration.key_file.as_deref().unwrap_or(""),
            if legacy { "legacy" } else { "modern" }
        );
        let cache = CREDENTIALS.get_or_init(|| {
            // Imported client keys are persisted; remove them when dtop exits.
            // SAFETY: registering a plain extern "C" function.
            unsafe { atexit(release_all_credentials) };
            Mutex::new(HashMap::new())
        });
        let mut cache = cache
            .lock()
            .map_err(|_| DockerError::Tls("credential cache poisoned".into()))?;
        if let Some(found) = cache.get(&key) {
            return Ok(Arc::clone(found));
        }
        let credentials = Arc::new(Credentials::create(configuration, legacy)?);
        cache.insert(key, Arc::clone(&credentials));
        Ok(credentials)
    }

    fn create(configuration: &TlsConfiguration, legacy: bool) -> Result<Credentials> {
        let (certificate, key) = match (&configuration.certificate_file, &configuration.key_file) {
            (Some(c), Some(k)) => load_client_certificate(c, k)?,
            _ => (std::ptr::null_mut(), 0),
        };
        let flags = CRED_MANUAL_VALIDATION | CRED_NO_DEFAULT_CREDS | USE_STRONG_CRYPTO;
        let mut chain: [Ptr; 1] = [certificate.cast()];
        let chain_pointer = if certificate.is_null() {
            std::ptr::null_mut()
        } else {
            chain.as_mut_ptr()
        };
        let mut handle = Handle::default();
        let mut style = Style::Legacy;
        if !legacy {
            let status = acquire_modern(&mut handle, chain_pointer, flags);
            if status == SEC_E_OK {
                trace(|| "credentials: SCH_CREDENTIALS (TLS 1.2 and 1.3)".into());
                style = Style::Modern;
            } else {
                trace(|| {
                    format!("credentials: SCH_CREDENTIALS refused ({}); falling back to SCHANNEL_CRED (TLS 1.2)", describe(status))
                });
            }
        }

        if style == Style::Legacy {
            let status = acquire_legacy(&mut handle, chain_pointer, flags);
            if status != SEC_E_OK {
                return Err(DockerError::Tls(format!(
                    "Windows could not set up TLS credentials: {}",
                    describe(status)
                )));
            }
            trace(|| "credentials: SCHANNEL_CRED (TLS 1.2)".into());
        }
        Ok(Credentials {
            handle: Mutex::new(handle),
            style,
            certificate: certificate as usize,
            key,
        })
    }

    fn release(&self) {
        // SAFETY: handles we created; each is released once (the cache is drained first).
        unsafe {
            if let Ok(mut handle) = self.handle.lock() {
                FreeCredentialsHandle(handle.ptr());
            }
            if self.key != 0 {
                NCryptDeleteKey(self.key, 0);
            }
            if self.certificate != 0 {
                CertFreeCertificateContext(self.certificate as *const CertContext);
            }
        }
    }
}

fn acquire(handle: &mut Handle, credential: Ptr) -> i32 {
    let mut package = wide("Microsoft Unified Security Protocol Provider");
    // SAFETY: valid credential structure and output handle.
    unsafe {
        AcquireCredentialsHandleW(
            std::ptr::null_mut(),
            package.as_mut_ptr(),
            CREDENTIALS_OUTBOUND,
            std::ptr::null_mut(),
            credential,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            handle,
            std::ptr::null_mut(),
        )
    }
}

fn acquire_modern(handle: &mut Handle, certificates: *mut Ptr, flags: u32) -> i32 {
    let mut parameters = TlsParameters {
        alpn_count: 0,
        alpn_ids: std::ptr::null_mut(),
        disabled_protocols: PROTOCOLS_BELOW_TLS12_CLIENT,
        disabled_crypto_count: 0,
        disabled_crypto: std::ptr::null_mut(),
        flags: 0,
    };
    let mut credential = SchCredentials {
        version: SCH_CREDENTIALS_VERSION,
        cred_format: 0,
        creds: u32::from(!certificates.is_null()),
        certificates,
        root_store: std::ptr::null_mut(),
        mappers: 0,
        mapper_list: std::ptr::null_mut(),
        session_lifespan: 0,
        flags,
        tls_parameters_count: 1,
        tls_parameters: &mut parameters,
    };
    acquire(handle, (&mut credential as *mut SchCredentials).cast())
}

fn acquire_legacy(handle: &mut Handle, certificates: *mut Ptr, flags: u32) -> i32 {
    let mut credential = SchannelCred {
        version: SCHANNEL_CRED_VERSION,
        creds: u32::from(!certificates.is_null()),
        certificates,
        root_store: std::ptr::null_mut(),
        mappers: 0,
        mapper_list: std::ptr::null_mut(),
        supported_algorithms: 0,
        algorithms: std::ptr::null_mut(),
        enabled_protocols: PROTOCOL_TLS12_CLIENT,
        minimum_cipher_strength: 0,
        maximum_cipher_strength: 0,
        session_lifespan: 0,
        flags,
        cred_format: 0,
    };
    acquire(handle, (&mut credential as *mut SchannelCred).cast())
}

/// Parses the certificate, imports the key as a named CNG key and links the two.
fn load_client_certificate(
    certificate_file: &str,
    key_file: &str,
) -> Result<(*mut CertContext, usize)> {
    let der = certificates(
        &read_file(certificate_file, "client certificate")?,
        certificate_file,
    )?
    .remove(0);
    let key = PrivateKey::parse(&read_file(key_file, "client key")?, key_file)?;
    // SAFETY: crypt32/ncrypt calls with valid buffers; the context and key are released by
    // `Credentials::release` (or here on failure).
    unsafe {
        let context =
            CertCreateCertificateContext(X509_PKCS7_ENCODING, der.as_ptr(), der.len() as u32);
        if context.is_null() {
            return Err(DockerError::Tls(format!(
                "Windows could not parse the client certificate {certificate_file}"
            )));
        }
        let mut provider = 0usize;
        let mut provider_name = wide("Microsoft Software Key Storage Provider");
        let status = NCryptOpenStorageProvider(&mut provider, provider_name.as_ptr(), 0);
        if status != 0 {
            CertFreeCertificateContext(context);
            return Err(DockerError::Tls(format!(
                "could not open the Windows key storage provider ({status})"
            )));
        }
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mut key_name = wide(&format!(
            "dtop-{}-{unique}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let blob = key.pkcs8_der();
        let mut buffer = SecBuffer {
            length: (key_name.len() * 2) as u32,
            kind: NCRYPTBUFFER_PKCS_KEY_NAME,
            data: key_name.as_mut_ptr().cast(),
        };
        let mut parameters = SecBufferDesc {
            version: 0,
            count: 1,
            buffers: &mut buffer,
        };
        let blob_type = wide("PKCS8_PRIVATEKEY");
        let mut key_handle = 0usize;
        let status = NCryptImportKey(
            provider,
            0,
            blob_type.as_ptr(),
            &mut parameters,
            &mut key_handle,
            blob.as_ptr(),
            blob.len() as u32,
            NCRYPT_OVERWRITE_KEY_FLAG,
        );
        NCryptFreeObject(provider);
        if status != 0 {
            CertFreeCertificateContext(context);
            return Err(DockerError::Tls(format!(
                "Windows could not import the client key {key_file} (status 0x{:x})",
                status as u32
            )));
        }
        let info = KeyProvInfo {
            container: key_name.as_mut_ptr(),
            provider: provider_name.as_mut_ptr(),
            provider_type: 0,
            flags: 0,
            parameter_count: 0,
            parameters: std::ptr::null_mut(),
            key_spec: 0,
        };
        if CertSetCertificateContextProperty(
            context,
            CERT_KEY_PROV_INFO_PROP_ID,
            0,
            (&info as *const KeyProvInfo).cast(),
        ) == 0
        {
            NCryptDeleteKey(key_handle, 0);
            CertFreeCertificateContext(context);
            return Err(DockerError::Tls(
                "Windows could not attach the key to the client certificate".into(),
            ));
        }
        Ok((context, key_handle))
    }
}

struct Session {
    context: Handle,
    has_context: bool,
    closed: bool,
    /// The server sent close_notify: the stream is over, whatever the socket does.
    peer_closed: bool,
    encrypted: Vec<u8>,
    plaintext: Vec<u8>,
    header: usize,
    trailer: usize,
    maximum_message: usize,
}

pub struct SChannelConnection {
    underlying: Arc<dyn Connection>,
    credentials: Arc<Credentials>,
    configuration: TlsConfiguration,
    server_name: String,
    endpoint: String,
    session: Mutex<Session>,
    pub negotiated: Option<TlsProtocolVersion>,
}

impl Drop for SChannelConnection {
    fn drop(&mut self) {
        if let Ok(mut session) = self.session.lock() {
            if session.has_context {
                // SAFETY: a context we created.
                unsafe { DeleteSecurityContext(session.context.ptr()) };
            }
        }
    }
}

impl SChannelConnection {
    pub fn connect(
        underlying: Arc<dyn Connection>,
        configuration: &TlsConfiguration,
        server_name: &str,
        endpoint: &str,
    ) -> Result<SChannelConnection> {
        let result = (|| {
            // Read the CA before connecting so a bad path fails first.
            let ca = match &configuration.ca_file {
                Some(file) => Some(certificates(&read_file(file, "CA certificate")?, file)?),
                None => None,
            };
            let credentials = Credentials::shared(configuration)?;
            let mut connection = SChannelConnection {
                underlying: Arc::clone(&underlying),
                credentials,
                configuration: configuration.clone(),
                server_name: server_name.into(),
                endpoint: endpoint.into(),
                session: Mutex::new(Session {
                    context: Handle::default(),
                    has_context: false,
                    closed: false,
                    peer_closed: false,
                    encrypted: Vec::new(),
                    plaintext: Vec::new(),
                    header: 0,
                    trailer: 0,
                    maximum_message: 16 * 1024,
                }),
                negotiated: None,
            };
            connection.handshake()?;
            if configuration.verify_peer {
                connection.verify_server(ca.as_deref())?;
            }
            connection.read_stream_sizes()?;
            Ok(connection)
        })();
        if result.is_err() {
            underlying.close();
        }
        result
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Session> {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn handshake(&mut self) -> Result<()> {
        let mut target = wide(&self.server_name);
        let mut first = true;
        let mut needs_read = false;
        loop {
            if needs_read {
                let bytes = self.underlying.read(16 * 1024)?;
                if bytes.is_empty() {
                    return Err(DockerError::Tls(format!(
                        "{} closed the connection during the TLS handshake — {}",
                        self.endpoint,
                        closed_during_handshake_hint(&self.configuration)
                    )));
                }
                self.lock().encrypted.extend(bytes);
            }
            let (status, token, extra) = {
                let mut session = self.lock();
                let session = &mut *session;
                let mut output_buffer = SecBuffer::empty(BUFFER_TOKEN);
                let mut output = SecBufferDesc {
                    version: 0,
                    count: 1,
                    buffers: &mut output_buffer,
                };
                let mut input_buffers = [
                    SecBuffer {
                        length: session.encrypted.len() as u32,
                        kind: BUFFER_TOKEN,
                        data: session.encrypted.as_mut_ptr().cast(),
                    },
                    SecBuffer::empty(BUFFER_EMPTY),
                ];
                let mut input = SecBufferDesc {
                    version: 0,
                    count: 2,
                    buffers: input_buffers.as_mut_ptr(),
                };
                let mut attributes = 0u32;
                let mut credential = self
                    .credentials
                    .handle
                    .lock()
                    .map(|h| *h)
                    .unwrap_or_default();
                let context_pointer: *mut Handle = &mut session.context;
                // SAFETY: valid handles and buffer descriptors for the duration of the call.
                let status = unsafe {
                    InitializeSecurityContextW(
                        &mut credential,
                        if first {
                            std::ptr::null_mut()
                        } else {
                            context_pointer
                        },
                        target.as_mut_ptr(),
                        CONTEXT_REQUIREMENTS,
                        0,
                        NATIVE_DATA_REPRESENTATION,
                        if first {
                            std::ptr::null_mut()
                        } else {
                            &mut input
                        },
                        0,
                        context_pointer,
                        &mut output,
                        &mut attributes,
                        std::ptr::null_mut(),
                    )
                };
                if first {
                    session.has_context = true;
                }
                let token = take_token(&output_buffer);
                let extra = (input_buffers[1].kind == BUFFER_EXTRA)
                    .then_some(input_buffers[1].length as usize);
                trace(|| {
                    format!(
                        "handshake: {} input={} token={} extra={}",
                        describe(status),
                        session.encrypted.len(),
                        token.len(),
                        extra.unwrap_or(0)
                    )
                });
                (status, token, extra)
            };
            first = false;
            if !token.is_empty() {
                self.underlying.write_all(&token)?;
            }
            let mut session = self.lock();
            match status {
                SEC_E_OK => {
                    let keep = extra.unwrap_or(0);
                    let start = session.encrypted.len() - keep;
                    session.encrypted.drain(..start);
                    return Ok(());
                }
                SEC_I_CONTINUE_NEEDED => match extra {
                    Some(keep) => {
                        let start = session.encrypted.len() - keep;
                        session.encrypted.drain(..start);
                        needs_read = false;
                    }
                    None => {
                        session.encrypted.clear();
                        needs_read = true;
                    }
                },
                // The server asked for a client certificate SChannel could not pick; the input
                // was not consumed, so go again with the same bytes.
                SEC_I_INCOMPLETE_CREDENTIALS => needs_read = false,
                SEC_E_INCOMPLETE_MESSAGE => needs_read = true,
                _ => {
                    return Err(DockerError::Tls(handshake_message(
                        &describe(status),
                        &self.endpoint,
                        &self.configuration,
                    )))
                }
            }
        }
    }

    fn read_stream_sizes(&mut self) -> Result<()> {
        let mut session = self.lock();
        let mut sizes = StreamSizes::default();
        // SAFETY: valid context; the buffers match the attributes' structures.
        let status = unsafe {
            QueryContextAttributesW(
                &mut session.context,
                ATTRIBUTE_STREAM_SIZES,
                (&mut sizes as *mut StreamSizes).cast(),
            )
        };
        if status != SEC_E_OK {
            return Err(DockerError::Tls(format!(
                "could not read TLS stream sizes: {}",
                describe(status)
            )));
        }
        session.header = sizes.header as usize;
        session.trailer = sizes.trailer as usize;
        session.maximum_message = (sizes.maximum_message as usize).max(1);
        // SecPkgContext_ConnectionInfo: dwProtocol is its first DWORD.
        let mut info = [0u32; 8];
        let status = unsafe {
            QueryContextAttributesW(
                &mut session.context,
                ATTRIBUTE_CONNECTION_INFO,
                info.as_mut_ptr().cast(),
            )
        };
        drop(session);
        if status == SEC_E_OK {
            let bits = info[0];
            self.negotiated = if bits & PROTOCOL_TLS13_CLIENT != 0 {
                Some(TlsProtocolVersion::Tls13)
            } else if bits & PROTOCOL_TLS12_CLIENT != 0 {
                Some(TlsProtocolVersion::Tls12)
            } else {
                None
            };
            let style = self.credentials.style;
            trace(|| format!("negotiated protocol bits 0x{bits:x} ({style:?})"));
        }
        Ok(())
    }

    /// Manual validation: trust exactly the configured CA (or the system roots), then check the
    /// SSL policy with the server name.
    fn verify_server(&self, ca: Option<&[Vec<u8>]>) -> Result<()> {
        let mut session = self.lock();
        let mut remote: *mut CertContext = std::ptr::null_mut();
        // SAFETY: crypt32 calls with valid structures; every object is freed before returning.
        unsafe {
            let status = QueryContextAttributesW(
                &mut session.context,
                ATTRIBUTE_REMOTE_CERTIFICATE,
                (&mut remote as *mut *mut CertContext).cast(),
            );
            drop(session);
            if status != SEC_E_OK || remote.is_null() {
                return Err(DockerError::Tls(format!(
                    "{} presented no certificate",
                    self.endpoint
                )));
            }
            let mut root_store: Ptr = std::ptr::null_mut();
            let mut engine: Ptr = std::ptr::null_mut();
            let cleanup = |engine: Ptr, store: Ptr, remote: *mut CertContext| {
                if !engine.is_null() {
                    CertFreeCertificateChainEngine(engine);
                }
                if !store.is_null() {
                    CertCloseStore(store, 0);
                }
                CertFreeCertificateContext(remote);
            };
            if let (Some(ca), Some(file)) = (ca, &self.configuration.ca_file) {
                root_store = CertOpenStore(CERT_STORE_PROV_MEMORY, 0, 0, 0, std::ptr::null());
                if root_store.is_null() {
                    cleanup(engine, root_store, remote);
                    return Err(DockerError::Tls(
                        "could not create a certificate store".into(),
                    ));
                }
                for der in ca {
                    if CertAddEncodedCertificateToStore(
                        root_store,
                        X509_PKCS7_ENCODING,
                        der.as_ptr(),
                        der.len() as u32,
                        CERT_STORE_ADD_ALWAYS,
                        std::ptr::null_mut(),
                    ) == 0
                    {
                        cleanup(engine, root_store, remote);
                        return Err(DockerError::Tls(format!(
                            "{file} contains a certificate Windows cannot parse"
                        )));
                    }
                }
                let config = ChainEngineConfig {
                    size: std::mem::size_of::<ChainEngineConfig>() as u32,
                    restricted_root: std::ptr::null_mut(),
                    restricted_trust: std::ptr::null_mut(),
                    restricted_other: std::ptr::null_mut(),
                    additional_store_count: 0,
                    additional_stores: std::ptr::null_mut(),
                    flags: 0,
                    url_retrieval_timeout: 0,
                    maximum_cached_certificates: 0,
                    cycle_detection_modulus: 0,
                    exclusive_root: root_store,
                    exclusive_trusted_people: std::ptr::null_mut(),
                    exclusive_flags: 0,
                };
                if CertCreateCertificateChainEngine(&config, &mut engine) == 0 {
                    cleanup(engine, root_store, remote);
                    return Err(DockerError::Tls(format!(
                        "could not create a certificate chain engine for {file}"
                    )));
                }
            }
            let parameters = ChainPara {
                size: std::mem::size_of::<ChainPara>() as u32,
                usage_type: 0,
                usage_count: 0,
                usage_identifiers: std::ptr::null_mut(),
            };
            let mut chain: Ptr = std::ptr::null_mut();
            if CertGetCertificateChain(
                engine,
                remote,
                std::ptr::null_mut(),
                (*remote).store,
                &parameters,
                0,
                std::ptr::null_mut(),
                &mut chain,
            ) == 0
                || chain.is_null()
            {
                cleanup(engine, root_store, remote);
                return Err(DockerError::Tls(untrusted_message(
                    "Windows could not build its chain",
                    &self.endpoint,
                    &self.configuration,
                )));
            }
            let mut name = wide(&self.server_name);
            let mut extra = SslPolicyPara {
                size: std::mem::size_of::<SslPolicyPara>() as u32,
                auth_type: AUTHTYPE_SERVER,
                checks: 0,
                server_name: name.as_mut_ptr(),
            };
            let policy = PolicyPara {
                size: std::mem::size_of::<PolicyPara>() as u32,
                flags: 0,
                extra: (&mut extra as *mut SslPolicyPara).cast(),
            };
            let mut result = PolicyStatus {
                size: std::mem::size_of::<PolicyStatus>() as u32,
                error: 0,
                chain_index: 0,
                element_index: 0,
                extra: std::ptr::null_mut(),
            };
            let checked = CertVerifyCertificateChainPolicy(
                CERT_CHAIN_POLICY_SSL,
                chain,
                &policy,
                &mut result,
            ) != 0;
            CertFreeCertificateChain(chain);
            cleanup(engine, root_store, remote);
            let error = if checked { result.error } else { u32::MAX };
            let untrusted = |reason: &str| {
                DockerError::Tls(untrusted_message(
                    reason,
                    &self.endpoint,
                    &self.configuration,
                ))
            };
            match error {
                0 => Ok(()),
                0x800B_010F => Err(DockerError::Tls(name_mismatch_message(
                    &self.server_name,
                    &self.endpoint,
                ))),
                0x800B_0109 => Err(untrusted("untrusted root")),
                0x800B_010A => Err(untrusted("no chain to a trusted root")),
                0x800B_0101 => Err(untrusted("the certificate has expired")),
                other => Err(untrusted(&format!("policy error 0x{other:x}"))),
            }
        }
    }

    /// Resumes the handshake for messages DecryptMessage hands back (SEC_I_RENEGOTIATE).
    fn handshake_continuation(&self, session: &mut Session) -> Result<()> {
        let mut target = wide(&self.server_name);
        loop {
            let mut output_buffer = SecBuffer::empty(BUFFER_TOKEN);
            let mut output = SecBufferDesc {
                version: 0,
                count: 1,
                buffers: &mut output_buffer,
            };
            let mut input_buffers = [
                SecBuffer {
                    length: session.encrypted.len() as u32,
                    kind: BUFFER_TOKEN,
                    data: session.encrypted.as_mut_ptr().cast(),
                },
                SecBuffer::empty(BUFFER_EMPTY),
            ];
            let mut input = SecBufferDesc {
                version: 0,
                count: 2,
                buffers: input_buffers.as_mut_ptr(),
            };
            let mut attributes = 0u32;
            let mut credential = self
                .credentials
                .handle
                .lock()
                .map(|h| *h)
                .unwrap_or_default();
            let context_pointer: *mut Handle = &mut session.context;
            // SAFETY: valid handles and buffer descriptors for the duration of the call.
            let status = unsafe {
                InitializeSecurityContextW(
                    &mut credential,
                    context_pointer,
                    target.as_mut_ptr(),
                    CONTEXT_REQUIREMENTS,
                    0,
                    NATIVE_DATA_REPRESENTATION,
                    &mut input,
                    0,
                    context_pointer,
                    &mut output,
                    &mut attributes,
                    std::ptr::null_mut(),
                )
            };
            let token = take_token(&output_buffer);
            if !token.is_empty() {
                self.underlying.write_all(&token)?;
            }
            let extra: Vec<u8> = if input_buffers[1].kind == BUFFER_EXTRA {
                session.encrypted[session.encrypted.len() - input_buffers[1].length as usize..]
                    .to_vec()
            } else {
                Vec::new()
            };
            match status {
                SEC_E_OK => {
                    session.encrypted = extra;
                    return Ok(());
                }
                SEC_I_CONTINUE_NEEDED | SEC_E_INCOMPLETE_MESSAGE => {
                    let bytes = self.underlying.read(16 * 1024)?;
                    if bytes.is_empty() {
                        return Ok(());
                    }
                    if status != SEC_E_INCOMPLETE_MESSAGE {
                        session.encrypted = extra;
                    }
                    session.encrypted.extend(bytes);
                }
                _ => {
                    return Err(DockerError::Tls(handshake_message(
                        &describe(status),
                        &self.endpoint,
                        &self.configuration,
                    )))
                }
            }
        }
    }
}

/// Copies and frees an output token SSPI allocated.
fn take_token(buffer: &SecBuffer) -> Vec<u8> {
    if buffer.data.is_null() {
        return Vec::new();
    }
    // SAFETY: SSPI allocated `length` bytes at `data`; we free it after copying.
    unsafe {
        let bytes =
            std::slice::from_raw_parts(buffer.data.cast::<u8>(), buffer.length as usize).to_vec();
        FreeContextBuffer(buffer.data);
        bytes
    }
}

impl Connection for SChannelConnection {
    fn write_all(&self, bytes: &[u8]) -> Result<()> {
        let mut session = self.lock();
        if session.closed {
            return Err(DockerError::Cancelled);
        }
        let (header, trailer) = (session.header, session.trailer);
        let mut offset = 0;
        while offset < bytes.len() {
            let chunk = session.maximum_message.min(bytes.len() - offset);
            let mut message = vec![0u8; header + chunk + trailer];
            message[header..header + chunk].copy_from_slice(&bytes[offset..offset + chunk]);
            let base = message.as_mut_ptr();
            // SAFETY: the buffers point into `message`, sized as the stream sizes require.
            let (status, total) = unsafe {
                let mut buffers = [
                    SecBuffer {
                        length: header as u32,
                        kind: BUFFER_STREAM_HEADER,
                        data: base.cast(),
                    },
                    SecBuffer {
                        length: chunk as u32,
                        kind: BUFFER_DATA,
                        data: base.add(header).cast(),
                    },
                    SecBuffer {
                        length: trailer as u32,
                        kind: BUFFER_STREAM_TRAILER,
                        data: base.add(header + chunk).cast(),
                    },
                    SecBuffer::empty(BUFFER_EMPTY),
                ];
                let mut descriptor = SecBufferDesc {
                    version: 0,
                    count: 4,
                    buffers: buffers.as_mut_ptr(),
                };
                let status = EncryptMessage(&mut session.context, 0, &mut descriptor, 0);
                (
                    status,
                    (buffers[0].length + buffers[1].length + buffers[2].length) as usize,
                )
            };
            trace(|| {
                format!(
                    "encrypt: {chunk} bytes -> {} total={total}",
                    describe(status)
                )
            });
            if status != SEC_E_OK {
                return Err(DockerError::Io(format!(
                    "TLS encryption failed: {}",
                    describe(status)
                )));
            }
            self.underlying.write_all(&message[..total])?;
            offset += chunk;
        }
        Ok(())
    }

    fn read(&self, max_len: usize) -> Result<Vec<u8>> {
        let mut guard = self.lock();
        let session = &mut *guard;
        if session.closed {
            return Err(DockerError::Cancelled);
        }
        while session.plaintext.is_empty() {
            if session.peer_closed {
                return Ok(Vec::new());
            }
            if !session.encrypted.is_empty() {
                let mut extra = Vec::new();
                let mut buffers = [
                    SecBuffer {
                        length: session.encrypted.len() as u32,
                        kind: BUFFER_DATA,
                        data: session.encrypted.as_mut_ptr().cast(),
                    },
                    SecBuffer::empty(BUFFER_EMPTY),
                    SecBuffer::empty(BUFFER_EMPTY),
                    SecBuffer::empty(BUFFER_EMPTY),
                ];
                let mut descriptor = SecBufferDesc {
                    version: 0,
                    count: 4,
                    buffers: buffers.as_mut_ptr(),
                };
                // SAFETY: the buffer covers `encrypted`; output buffers point into it.
                let status = unsafe {
                    DecryptMessage(
                        &mut session.context,
                        &mut descriptor,
                        0,
                        std::ptr::null_mut(),
                    )
                };
                // Only a successful decrypt yields plaintext; on close_notify (CONTEXT_EXPIRED)
                // the buffers still describe the alert record, which is not data.
                if status == SEC_E_OK || status == SEC_I_RENEGOTIATE {
                    for buffer in &buffers {
                        if buffer.data.is_null() || buffer.length == 0 {
                            continue;
                        }
                        // SAFETY: SChannel points these into the `encrypted` buffer.
                        let bytes = unsafe {
                            std::slice::from_raw_parts(
                                buffer.data.cast::<u8>(),
                                buffer.length as usize,
                            )
                        };
                        match buffer.kind {
                            BUFFER_DATA => session.plaintext.extend_from_slice(bytes),
                            BUFFER_EXTRA => extra = bytes.to_vec(),
                            _ => {}
                        }
                    }
                }
                trace(|| {
                    format!(
                        "decrypt: {} input={} plaintext={} extra={}",
                        describe(status),
                        session.encrypted.len(),
                        session.plaintext.len(),
                        extra.len()
                    )
                });
                match status {
                    SEC_E_OK => {
                        session.encrypted = extra;
                        continue;
                    }
                    SEC_I_CONTEXT_EXPIRED => {
                        // close_notify: the server finished the stream.
                        session.encrypted.clear();
                        session.peer_closed = true;
                        continue;
                    }
                    SEC_I_RENEGOTIATE => {
                        // TLS 1.3 post-handshake messages (session tickets, key updates) come back
                        // as SEC_I_RENEGOTIATE; SChannel wants them fed to InitializeSecurityContext.
                        session.encrypted = extra;
                        if !session.encrypted.is_empty() {
                            self.handshake_continuation(session)?;
                        }
                        continue;
                    }
                    SEC_E_INCOMPLETE_MESSAGE => {}
                    _ => {
                        return Err(DockerError::Tls(handshake_message(
                            &describe(status),
                            &self.endpoint,
                            &self.configuration,
                        )))
                    }
                }
            }
            let bytes = self.underlying.read(16 * 1024)?;
            trace(|| format!("read: {} bytes", bytes.len()));
            if bytes.is_empty() {
                return Ok(Vec::new());
            }
            session.encrypted.extend(bytes);
        }
        let count = max_len.max(1).min(session.plaintext.len());
        Ok(session.plaintext.drain(..count).collect())
    }

    fn disable_read_timeout(&self) -> Result<()> {
        // Reads wait on the TCP socket underneath.
        self.underlying.disable_read_timeout()
    }

    fn interrupt(&self) {
        self.underlying.interrupt();
    }

    fn close(&self) {
        self.underlying.close();
        self.lock().closed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_legacy_credentials_use_schannel_cred() {
        let credentials = with_legacy_credentials(|| {
            Credentials::shared(&TlsConfiguration::default())
                .expect("legacy SChannel credentials should be available on supported Windows")
        });
        assert_eq!(credentials.style, Style::Legacy);
    }
}
