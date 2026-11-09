//! TLS on Linux through OpenSSL 1.1.1 or 3.x, loaded with `dlopen` (no build dependency). TLS
//! runs through memory BIOs on top of dtop's own TCP connection, so timeouts and interrupts keep
//! working.

use std::ffi::{c_char, c_int, c_long, c_void, CStr, CString};
use std::sync::{Arc, Mutex, OnceLock};

use super::{
    closed_during_handshake_hint, handshake_message, is_ip_address, name_mismatch_message, trace,
    untrusted_message, TlsProtocolVersion,
};
use crate::error::{DockerError, Result};
use crate::host::TlsConfiguration;
use crate::transport::Connection;

type Ptr = *mut c_void;

#[cfg_attr(all(target_os = "linux", target_env = "gnu"), link(name = "dl"))]
extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> Ptr;
    fn dlsym(handle: Ptr, symbol: *const c_char) -> Ptr;
}

const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;

// Constants from ssl.h / bio.h / x509_vfy.h (stable across 1.1 and 3.x).
const VERIFY_NONE: c_int = 0;
const VERIFY_PEER: c_int = 1;
const FILETYPE_PEM: c_int = 1;
const CTRL_SET_MIN_PROTO_VERSION: c_int = 123;
const TLS1_2_VERSION: c_long = 0x0303;
const CTRL_SET_TLSEXT_HOSTNAME: c_int = 55;
const BIO_CTRL_PENDING: c_int = 10;
const BIO_C_SET_BUF_MEM_EOF_RETURN: c_int = 130;
const ERROR_WANT_READ: c_int = 2;
const ERROR_WANT_WRITE: c_int = 3;
const ERROR_ZERO_RETURN: c_int = 6;
const X509_V_OK: c_long = 0;
const X509_V_ERR_HOSTNAME_MISMATCH: c_long = 62;
const X509_V_ERR_IP_ADDRESS_MISMATCH: c_long = 64;

struct Library {
    client_method: unsafe extern "C" fn() -> Ptr,
    ctx_new: unsafe extern "C" fn(Ptr) -> Ptr,
    ctx_free: unsafe extern "C" fn(Ptr),
    ctx_ctrl: unsafe extern "C" fn(Ptr, c_int, c_long, Ptr) -> c_long,
    ctx_set_verify: unsafe extern "C" fn(Ptr, c_int, Ptr),
    ctx_load_verify_locations: unsafe extern "C" fn(Ptr, *const c_char, *const c_char) -> c_int,
    ctx_set_default_verify_paths: unsafe extern "C" fn(Ptr) -> c_int,
    ctx_use_certificate_chain_file: unsafe extern "C" fn(Ptr, *const c_char) -> c_int,
    ctx_use_private_key_file: unsafe extern "C" fn(Ptr, *const c_char, c_int) -> c_int,
    ctx_check_private_key: unsafe extern "C" fn(Ptr) -> c_int,
    ssl_new: unsafe extern "C" fn(Ptr) -> Ptr,
    ssl_free: unsafe extern "C" fn(Ptr),
    ssl_set_bio: unsafe extern "C" fn(Ptr, Ptr, Ptr),
    ssl_set_connect_state: unsafe extern "C" fn(Ptr),
    ssl_do_handshake: unsafe extern "C" fn(Ptr) -> c_int,
    ssl_read: unsafe extern "C" fn(Ptr, *mut c_void, c_int) -> c_int,
    ssl_write: unsafe extern "C" fn(Ptr, *const c_void, c_int) -> c_int,
    ssl_get_error: unsafe extern "C" fn(Ptr, c_int) -> c_int,
    ssl_ctrl: unsafe extern "C" fn(Ptr, c_int, c_long, Ptr) -> c_long,
    ssl_set1_host: unsafe extern "C" fn(Ptr, *const c_char) -> c_int,
    ssl_get0_param: unsafe extern "C" fn(Ptr) -> Ptr,
    param_set1_ip_asc: unsafe extern "C" fn(Ptr, *const c_char) -> c_int,
    ssl_get_verify_result: unsafe extern "C" fn(Ptr) -> c_long,
    verify_error_string: unsafe extern "C" fn(c_long) -> *const c_char,
    bio_s_mem: unsafe extern "C" fn() -> Ptr,
    bio_new: unsafe extern "C" fn(Ptr) -> Ptr,
    bio_read: unsafe extern "C" fn(Ptr, *mut c_void, c_int) -> c_int,
    bio_write: unsafe extern "C" fn(Ptr, *const c_void, c_int) -> c_int,
    bio_ctrl: unsafe extern "C" fn(Ptr, c_int, c_long, Ptr) -> c_long,
    err_get_error: unsafe extern "C" fn() -> std::ffi::c_ulong,
    err_error_string_n: unsafe extern "C" fn(std::ffi::c_ulong, *mut c_char, usize),
    err_clear_error: unsafe extern "C" fn(),
    ssl_version: unsafe extern "C" fn(Ptr) -> c_int,
}

// SAFETY: the library holds only function pointers into a loaded shared object.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

fn open(names: &[&str]) -> Ptr {
    for name in names {
        let Ok(c) = CString::new(*name) else { continue };
        // SAFETY: dlopen with a valid C string.
        let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
        if !handle.is_null() {
            return handle;
        }
    }
    std::ptr::null_mut()
}

impl Library {
    fn shared() -> Result<&'static Library> {
        static SHARED: OnceLock<std::result::Result<Library, String>> = OnceLock::new();
        SHARED
            .get_or_init(Library::load)
            .as_ref()
            .map_err(|e| DockerError::Tls(e.clone()))
    }

    fn load() -> std::result::Result<Library, String> {
        let ssl = open(&["libssl.so.3", "libssl.so.1.1", "libssl.so"]);
        let crypto = open(&["libcrypto.so.3", "libcrypto.so.1.1", "libcrypto.so"]);
        if ssl.is_null() || crypto.is_null() {
            return Err("OpenSSL (libssl.so.3 or libssl.so.1.1) is not installed; install it \
                        (apt install libssl3, dnf install openssl-libs, apk add libssl3) to use TLS endpoints"
                .into());
        }
        macro_rules! symbol {
            ($name:literal) => {{
                let c = concat!($name, "\0");
                // SAFETY: NUL-terminated names; handles from dlopen.
                let mut pointer = unsafe { dlsym(ssl, c.as_ptr().cast()) };
                if pointer.is_null() {
                    pointer = unsafe { dlsym(crypto, c.as_ptr().cast()) };
                }
                if pointer.is_null() {
                    return Err(format!(
                        "the installed OpenSSL has no {}; dtop needs OpenSSL 1.1.1 or newer",
                        $name
                    ));
                }
                // SAFETY: the symbol has the C signature declared for the field it fills.
                #[expect(
                    clippy::missing_transmute_annotations,
                    reason = "the destination signature is declared on the Library field"
                )]
                let function = unsafe { std::mem::transmute::<Ptr, _>(pointer) };
                function
            }};
        }
        Ok(Library {
            client_method: symbol!("TLS_client_method"),
            ctx_new: symbol!("SSL_CTX_new"),
            ctx_free: symbol!("SSL_CTX_free"),
            ctx_ctrl: symbol!("SSL_CTX_ctrl"),
            ctx_set_verify: symbol!("SSL_CTX_set_verify"),
            ctx_load_verify_locations: symbol!("SSL_CTX_load_verify_locations"),
            ctx_set_default_verify_paths: symbol!("SSL_CTX_set_default_verify_paths"),
            ctx_use_certificate_chain_file: symbol!("SSL_CTX_use_certificate_chain_file"),
            ctx_use_private_key_file: symbol!("SSL_CTX_use_PrivateKey_file"),
            ctx_check_private_key: symbol!("SSL_CTX_check_private_key"),
            ssl_new: symbol!("SSL_new"),
            ssl_free: symbol!("SSL_free"),
            ssl_set_bio: symbol!("SSL_set_bio"),
            ssl_set_connect_state: symbol!("SSL_set_connect_state"),
            ssl_do_handshake: symbol!("SSL_do_handshake"),
            ssl_read: symbol!("SSL_read"),
            ssl_write: symbol!("SSL_write"),
            ssl_get_error: symbol!("SSL_get_error"),
            ssl_ctrl: symbol!("SSL_ctrl"),
            ssl_set1_host: symbol!("SSL_set1_host"),
            ssl_get0_param: symbol!("SSL_get0_param"),
            param_set1_ip_asc: symbol!("X509_VERIFY_PARAM_set1_ip_asc"),
            ssl_get_verify_result: symbol!("SSL_get_verify_result"),
            verify_error_string: symbol!("X509_verify_cert_error_string"),
            bio_s_mem: symbol!("BIO_s_mem"),
            bio_new: symbol!("BIO_new"),
            bio_read: symbol!("BIO_read"),
            bio_write: symbol!("BIO_write"),
            bio_ctrl: symbol!("BIO_ctrl"),
            err_get_error: symbol!("ERR_get_error"),
            err_error_string_n: symbol!("ERR_error_string_n"),
            err_clear_error: symbol!("ERR_clear_error"),
            ssl_version: symbol!("SSL_version"),
        })
    }

    fn drain_errors(&self) -> Option<String> {
        let mut messages = Vec::new();
        loop {
            // SAFETY: OpenSSL error-queue calls; the buffer is 256 bytes.
            let code = unsafe { (self.err_get_error)() };
            if code == 0 {
                break;
            }
            let mut buffer = [0 as c_char; 256];
            unsafe { (self.err_error_string_n)(code, buffer.as_mut_ptr(), buffer.len()) };
            messages.push(
                unsafe { CStr::from_ptr(buffer.as_ptr()) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        (!messages.is_empty()).then(|| messages.join("; "))
    }
}

fn c_string(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| DockerError::Tls(format!("'{value}' contains a NUL byte")))
}

struct Session {
    context: Ptr,
    ssl: Ptr,
    input: Ptr,
    output: Ptr,
    closed: bool,
}

// SAFETY: the SSL objects are only touched under the connection's mutex.
unsafe impl Send for Session {}

pub struct OpenSslConnection {
    library: &'static Library,
    underlying: Arc<dyn Connection>,
    session: Mutex<Session>,
    configuration: TlsConfiguration,
    endpoint: String,
    pub negotiated: Option<TlsProtocolVersion>,
}

impl Drop for OpenSslConnection {
    fn drop(&mut self) {
        if let Ok(session) = self.session.lock() {
            // SAFETY: frees objects this connection created (the session owns its BIOs).
            unsafe {
                (self.library.ssl_free)(session.ssl);
                (self.library.ctx_free)(session.context);
            }
        }
    }
}

impl OpenSslConnection {
    pub fn connect(
        underlying: Arc<dyn Connection>,
        configuration: &TlsConfiguration,
        server_name: &str,
        endpoint: &str,
    ) -> Result<OpenSslConnection> {
        let library = Library::shared()?;
        let lib = library;
        // SAFETY: all calls below follow OpenSSL's documented ownership rules; every object
        // created here is freed by `Drop` or on the error paths.
        unsafe {
            (lib.err_clear_error)();
            let context = (lib.ctx_new)((lib.client_method)());
            if context.is_null() {
                return Err(DockerError::Tls(format!(
                    "could not create a TLS context: {}",
                    lib.drain_errors().unwrap_or_else(|| "unknown error".into())
                )));
            }
            // The context must be complete before SSL_new, which copies its certificate and key.
            if let Err(error) = configure_context(lib, context, configuration) {
                (lib.ctx_free)(context);
                underlying.close();
                return Err(error);
            }
            let ssl = (lib.ssl_new)(context);
            if ssl.is_null() {
                (lib.ctx_free)(context);
                return Err(DockerError::Tls(format!(
                    "could not create a TLS session: {}",
                    lib.drain_errors().unwrap_or_else(|| "unknown error".into())
                )));
            }
            let input = (lib.bio_new)((lib.bio_s_mem)());
            let output = (lib.bio_new)((lib.bio_s_mem)());
            if input.is_null() || output.is_null() {
                (lib.ssl_free)(ssl);
                (lib.ctx_free)(context);
                return Err(DockerError::Tls("could not allocate TLS buffers".into()));
            }
            // An empty input BIO means "wait for more bytes", not end of stream.
            (lib.bio_ctrl)(
                input,
                BIO_C_SET_BUF_MEM_EOF_RETURN,
                -1,
                std::ptr::null_mut(),
            );
            (lib.ssl_set_bio)(ssl, input, output);
            let mut connection = OpenSslConnection {
                library,
                underlying: Arc::clone(&underlying),
                session: Mutex::new(Session {
                    context,
                    ssl,
                    input,
                    output,
                    closed: false,
                }),
                configuration: configuration.clone(),
                endpoint: endpoint.into(),
                negotiated: None,
            };
            let result = connection
                .configure_session(server_name)
                .and_then(|()| connection.handshake(server_name));
            if let Err(error) = result {
                underlying.close();
                return Err(error);
            }
            Ok(connection)
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Session> {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn configure_session(&self, server_name: &str) -> Result<()> {
        let lib = self.library;
        let session = self.lock();
        let name = c_string(server_name)?;
        let is_address = is_ip_address(server_name);
        // SAFETY: valid SSL object and C string.
        unsafe {
            if self.configuration.verify_peer {
                let accepted = if is_address {
                    (lib.param_set1_ip_asc)((lib.ssl_get0_param)(session.ssl), name.as_ptr())
                } else {
                    (lib.ssl_set1_host)(session.ssl, name.as_ptr())
                };
                if accepted != 1 {
                    return Err(DockerError::Tls(format!(
                        "'{server_name}' is not a valid name to verify"
                    )));
                }
            }
            if !is_address {
                // Server Name Indication, so name-based virtual hosts present the right certificate.
                (lib.ssl_ctrl)(
                    session.ssl,
                    CTRL_SET_TLSEXT_HOSTNAME,
                    0,
                    name.as_ptr() as Ptr,
                );
            }
        }
        Ok(())
    }

    fn handshake(&mut self, server_name: &str) -> Result<()> {
        let lib = self.library;
        let ssl = self.lock().ssl;
        // SAFETY: valid SSL object; reads and writes go through the memory BIOs.
        unsafe { (lib.ssl_set_connect_state)(ssl) };
        loop {
            let result = unsafe { (lib.ssl_do_handshake)(ssl) };
            self.flush_output(&self.lock())?;
            if result == 1 {
                self.negotiated = match unsafe { (lib.ssl_version)(ssl) } {
                    0x0304 => Some(TlsProtocolVersion::Tls13),
                    0x0303 => Some(TlsProtocolVersion::Tls12),
                    _ => None,
                };
                trace(|| {
                    format!(
                        "openssl: negotiated {}",
                        self.negotiated
                            .map_or("an older protocol", TlsProtocolVersion::as_str)
                    )
                });
                return Ok(());
            }
            match unsafe { (lib.ssl_get_error)(ssl, result) } {
                ERROR_WANT_READ => {
                    let bytes = self.underlying.read(16 * 1024)?;
                    if bytes.is_empty() {
                        return Err(DockerError::Tls(format!(
                            "{} closed the connection during the TLS handshake — {}",
                            self.endpoint,
                            closed_during_handshake_hint(&self.configuration)
                        )));
                    }
                    self.feed(&self.lock(), &bytes);
                }
                ERROR_WANT_WRITE => continue,
                _ => return Err(self.handshake_error(server_name)),
            }
        }
    }

    fn handshake_error(&self, server_name: &str) -> DockerError {
        let lib = self.library;
        let ssl = self.lock().ssl;
        // SAFETY: valid SSL object; the returned string is static.
        let verify = unsafe { (lib.ssl_get_verify_result)(ssl) };
        if self.configuration.verify_peer && verify != X509_V_OK {
            if verify == X509_V_ERR_HOSTNAME_MISMATCH || verify == X509_V_ERR_IP_ADDRESS_MISMATCH {
                return DockerError::Tls(name_mismatch_message(server_name, &self.endpoint));
            }
            let text = unsafe { (lib.verify_error_string)(verify) };
            let reason = if text.is_null() {
                format!("code {verify}")
            } else {
                unsafe { CStr::from_ptr(text) }
                    .to_string_lossy()
                    .into_owned()
            };
            return DockerError::Tls(untrusted_message(
                &reason,
                &self.endpoint,
                &self.configuration,
            ));
        }
        let detail = lib
            .drain_errors()
            .unwrap_or_else(|| "handshake failed".into());
        DockerError::Tls(handshake_message(
            &detail,
            &self.endpoint,
            &self.configuration,
        ))
    }

    fn feed(&self, session: &Session, bytes: &[u8]) {
        // SAFETY: valid BIO; the slice is valid for its length.
        unsafe {
            (self.library.bio_write)(session.input, bytes.as_ptr().cast(), bytes.len() as c_int)
        };
    }

    fn flush_output(&self, session: &Session) -> Result<()> {
        let lib = self.library;
        // SAFETY: valid BIO; the buffer is valid for its length.
        while unsafe { (lib.bio_ctrl)(session.output, BIO_CTRL_PENDING, 0, std::ptr::null_mut()) }
            > 0
        {
            let mut buffer = vec![0u8; 16 * 1024];
            let count = unsafe {
                (lib.bio_read)(
                    session.output,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as c_int,
                )
            };
            if count <= 0 {
                break;
            }
            self.underlying.write_all(&buffer[..count as usize])?;
        }
        Ok(())
    }
}

unsafe fn configure_context(
    lib: &Library,
    context: Ptr,
    configuration: &TlsConfiguration,
) -> Result<()> {
    // SAFETY: the caller passes a fresh context; strings outlive the calls.
    unsafe {
        (lib.ctx_ctrl)(
            context,
            CTRL_SET_MIN_PROTO_VERSION,
            TLS1_2_VERSION,
            std::ptr::null_mut(),
        );
        if configuration.verify_peer {
            (lib.ctx_set_verify)(context, VERIFY_PEER, std::ptr::null_mut());
            if let Some(ca) = &configuration.ca_file {
                let path = c_string(ca)?;
                if (lib.ctx_load_verify_locations)(context, path.as_ptr(), std::ptr::null()) != 1 {
                    return Err(DockerError::Tls(format!(
                        "could not load the CA certificate {ca}: {}",
                        lib.drain_errors()
                            .unwrap_or_else(|| "not a PEM certificate".into())
                    )));
                }
            } else {
                (lib.ctx_set_default_verify_paths)(context);
            }
        } else {
            (lib.ctx_set_verify)(context, VERIFY_NONE, std::ptr::null_mut());
        }
        if let (Some(certificate), Some(key)) =
            (&configuration.certificate_file, &configuration.key_file)
        {
            let certificate_path = c_string(certificate)?;
            let key_path = c_string(key)?;
            if (lib.ctx_use_certificate_chain_file)(context, certificate_path.as_ptr()) != 1 {
                return Err(DockerError::Tls(format!(
                    "could not load the client certificate {certificate}: {}",
                    lib.drain_errors()
                        .unwrap_or_else(|| "not a PEM certificate".into())
                )));
            }
            if (lib.ctx_use_private_key_file)(context, key_path.as_ptr(), FILETYPE_PEM) != 1 {
                return Err(DockerError::Tls(format!(
                    "could not load the client key {key}: {} (encrypted keys are not supported)",
                    lib.drain_errors()
                        .unwrap_or_else(|| "not a PEM private key".into())
                )));
            }
            if (lib.ctx_check_private_key)(context) != 1 {
                (lib.err_clear_error)();
                return Err(DockerError::Tls(format!(
                    "the client certificate {certificate} and key {key} do not belong together"
                )));
            }
        }
    }
    Ok(())
}

impl Connection for OpenSslConnection {
    fn write_all(&self, bytes: &[u8]) -> Result<()> {
        let lib = self.library;
        let session = self.lock();
        if session.closed {
            return Err(DockerError::Cancelled);
        }
        let mut offset = 0;
        while offset < bytes.len() {
            let chunk = (bytes.len() - offset).min(1 << 20);
            // SAFETY: valid SSL object; the slice is valid for `chunk` bytes from `offset`.
            let written = unsafe {
                (lib.ssl_write)(session.ssl, bytes[offset..].as_ptr().cast(), chunk as c_int)
            };
            self.flush_output(&session)?;
            if written > 0 {
                offset += written as usize;
                continue;
            }
            match unsafe { (lib.ssl_get_error)(session.ssl, written) } {
                ERROR_WANT_READ => {
                    let incoming = self.underlying.read(16 * 1024)?;
                    if incoming.is_empty() {
                        return Err(DockerError::Io(
                            "the TLS connection closed while writing".into(),
                        ));
                    }
                    self.feed(&session, &incoming);
                }
                ERROR_WANT_WRITE => {}
                error => {
                    return Err(DockerError::Io(format!(
                        "TLS write failed: {}",
                        lib.drain_errors()
                            .unwrap_or_else(|| format!("error {error}"))
                    )))
                }
            }
        }
        Ok(())
    }

    fn read(&self, max_len: usize) -> Result<Vec<u8>> {
        let lib = self.library;
        let session = self.lock();
        if session.closed {
            return Err(DockerError::Cancelled);
        }
        let mut buffer = vec![0u8; max_len.max(1)];
        loop {
            let length = buffer.len().min(c_int::MAX as usize) as c_int;
            // SAFETY: valid SSL object; the buffer is valid for `length` bytes.
            let count = unsafe { (lib.ssl_read)(session.ssl, buffer.as_mut_ptr().cast(), length) };
            if count > 0 {
                self.flush_output(&session)?;
                buffer.truncate(count as usize);
                return Ok(buffer);
            }
            match unsafe { (lib.ssl_get_error)(session.ssl, count) } {
                ERROR_WANT_READ => {
                    self.flush_output(&session)?;
                    let incoming = self.underlying.read(16 * 1024)?;
                    // A peer that closes without close_notify still ends the stream (HTTP
                    // framing tells whether the body was complete).
                    if incoming.is_empty() {
                        return Ok(Vec::new());
                    }
                    self.feed(&session, &incoming);
                }
                ERROR_WANT_WRITE => self.flush_output(&session)?,
                ERROR_ZERO_RETURN => return Ok(Vec::new()),
                _ => {
                    // With TLS 1.3 the server checks our certificate after the client finished
                    // its side, so a rejection surfaces here, on the first read.
                    return match lib.drain_errors() {
                        Some(message) => Err(DockerError::Tls(handshake_message(
                            &message,
                            &self.endpoint,
                            &self.configuration,
                        ))),
                        None => Ok(Vec::new()),
                    };
                }
            }
        }
    }

    fn interrupt(&self) {
        self.underlying.interrupt();
    }

    fn close(&self) {
        // Closing the socket first wakes a reader blocked in `underlying.read` (holding the lock).
        self.underlying.close();
        self.lock().closed = true;
    }
}
