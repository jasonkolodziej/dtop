//! TLS on macOS through Network.framework, which owns the socket. Reads and writes block on
//! condition variables over Network.framework's callbacks to fit dtop's `Connection` model.
//!
//! - Verification: a verify block evaluates the server's trust with an SSL policy for the server
//!   name, anchored to `--tlscacert` when given (only that CA is trusted), else system roots.
//! - Client certificates: the PEM certificate and key become a `SecIdentity` in memory; nothing
//!   is written to the keychain.
//!
//! The C APIs take Objective-C blocks. Without crates (ADR 0001), blocks are built by hand
//! following the Block ABI: a literal with copy/dispose helpers that manage a reference-counted
//! Rust closure, so the framework can copy and release them as it likes.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_long, c_void, CStr, CString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::pem::{certificates, KeyKind, PrivateKey};
use super::{
    closed_during_handshake_hint, handshake_message, is_ip_address, name_mismatch_message,
    read_file, trace, untrusted_message, TlsProtocolVersion,
};
use crate::error::{DockerError, Result};
use crate::host::TlsConfiguration;
use crate::transport::Connection;

type Object = *mut c_void;
type CFTypeRef = *const c_void;

// MARK: - Blocks

#[repr(C)]
struct BlockDescriptor {
    reserved: usize,
    size: usize,
    copy: unsafe extern "C" fn(*mut c_void, *const c_void),
    dispose: unsafe extern "C" fn(*const c_void),
}

const BLOCK_HAS_COPY_DISPOSE: i32 = 1 << 25;

extern "C" {
    static _NSConcreteStackBlock: [*const c_void; 32];
}

macro_rules! block_type {
    ($name:ident, ($($arg:ident: $ty:ty),*)) => {
        #[repr(C)]
        struct $name {
            isa: *const c_void,
            flags: i32,
            reserved: i32,
            invoke: unsafe extern "C" fn(*mut $name, $($ty),*),
            descriptor: *const BlockDescriptor,
            closure: *mut Arc<dyn Fn($($ty),*) + Send + Sync>,
        }

        impl $name {
            /// A stack-style block literal; the framework copies it to the heap when it keeps it.
            fn new(closure: impl Fn($($ty),*) + Send + Sync + 'static) -> Box<$name> {
                unsafe extern "C" fn invoke(block: *mut $name, $($arg: $ty),*) {
                    // SAFETY: `closure` points to a live Arc for as long as the block exists.
                    let closure = unsafe { &*(*block).closure };
                    closure($($arg),*);
                }
                unsafe extern "C" fn copy(destination: *mut c_void, source: *const c_void) {
                    // SAFETY: Block_copy passes two literals of this type.
                    unsafe {
                        let source = &*(source as *const $name);
                        let destination = &mut *(destination as *mut $name);
                        destination.closure = Box::into_raw(Box::new(Arc::clone(&*source.closure)));
                    }
                }
                unsafe extern "C" fn dispose(block: *const c_void) {
                    // SAFETY: each literal owns exactly one boxed Arc.
                    unsafe { drop(Box::from_raw((*(block as *const $name)).closure)) };
                }
                static DESCRIPTOR: BlockDescriptor =
                    BlockDescriptor { reserved: 0, size: std::mem::size_of::<$name>(), copy, dispose };
                let closure: Arc<dyn Fn($($ty),*) + Send + Sync> = Arc::new(closure);
                // SAFETY: taking the address of the runtime's class symbol.
                #[allow(unused_unsafe, reason = "Rust 1.81 requires unsafe for extern statics")]
                let isa = unsafe { std::ptr::addr_of!(_NSConcreteStackBlock).cast() };
                Box::new($name {
                    isa,
                    flags: BLOCK_HAS_COPY_DISPOSE,
                    reserved: 0,
                    invoke,
                    descriptor: &DESCRIPTOR,
                    closure: Box::into_raw(Box::new(closure)),
                })
            }
        }

        impl Drop for $name {
            fn drop(&mut self) {
                // SAFETY: the stack literal's own reference (copies hold their own).
                unsafe { drop(Box::from_raw(self.closure)) };
            }
        }
    };
}

block_type!(OptionsBlock, (options: Object));
block_type!(StateBlock, (state: c_int, error: Object));
block_type!(ErrorBlock, (error: Object));
block_type!(ReceiveBlock, (content: Object, context: Object, is_complete: bool, error: Object));
block_type!(VerifyBlock, (metadata: Object, trust: Object, complete: Object));

/// Calls a `void (^)(bool)` block handed to us by the framework.
unsafe fn call_bool_block(block: Object, value: bool) {
    #[repr(C)]
    struct Literal {
        isa: *const c_void,
        flags: i32,
        reserved: i32,
        invoke: unsafe extern "C" fn(Object, bool),
    }
    // SAFETY: the caller passes a valid block of that signature.
    unsafe { ((*(block as *const Literal)).invoke)(block, value) };
}

// MARK: - Framework declarations

#[link(name = "Network", kind = "framework")]
extern "C" {
    /// `NW_CONNECTION_DEFAULT_MESSAGE_CONTEXT` is this variable's value (an object pointer).
    static _nw_content_context_default_message: Object;
    fn nw_parameters_create_secure_tcp(
        configure_tls: *mut c_void,
        configure_tcp: *mut c_void,
    ) -> Object;
    fn nw_tls_copy_sec_protocol_options(options: Object) -> Object;
    fn nw_tcp_options_set_no_delay(options: Object, no_delay: bool);
    fn nw_tcp_options_set_connection_timeout(options: Object, seconds: u32);
    fn nw_endpoint_create_host(host: *const c_char, port: *const c_char) -> Object;
    fn nw_connection_create(endpoint: Object, parameters: Object) -> Object;
    fn nw_connection_set_queue(connection: Object, queue: Object);
    fn nw_connection_set_state_changed_handler(connection: Object, handler: *mut c_void);
    fn nw_connection_start(connection: Object);
    fn nw_connection_cancel(connection: Object);
    fn nw_connection_send(
        connection: Object,
        content: Object,
        context: Object,
        is_complete: bool,
        completion: *mut c_void,
    );
    fn nw_connection_receive(
        connection: Object,
        minimum: u32,
        maximum: u32,
        completion: *mut c_void,
    );
    fn nw_connection_copy_protocol_metadata(connection: Object, definition: Object) -> Object;
    fn nw_protocol_copy_tls_definition() -> Object;
    fn nw_tls_copy_sec_protocol_metadata(metadata: Object) -> Object;
    fn nw_error_get_error_domain(error: Object) -> c_int;
    fn nw_error_get_error_code(error: Object) -> c_int;
    fn nw_release(object: Object);
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    static kSecAttrKeyType: CFTypeRef;
    static kSecAttrKeyTypeRSA: CFTypeRef;
    static kSecAttrKeyTypeECSECPrimeRandom: CFTypeRef;
    static kSecAttrKeyClass: CFTypeRef;
    static kSecAttrKeyClassPrivate: CFTypeRef;
    fn sec_protocol_options_set_min_tls_protocol_version(options: Object, version: u16);
    fn sec_protocol_options_set_tls_server_name(options: Object, name: *const c_char);
    fn sec_protocol_options_set_local_identity(options: Object, identity: Object);
    fn sec_protocol_options_set_verify_block(options: Object, block: *mut c_void, queue: Object);
    fn sec_protocol_metadata_get_negotiated_tls_protocol_version(metadata: Object) -> u16;
    fn sec_trust_copy_ref(trust: Object) -> CFTypeRef;
    fn sec_identity_create(identity: CFTypeRef) -> Object;
    fn sec_identity_create_with_certificates(
        identity: CFTypeRef,
        certificates: CFTypeRef,
    ) -> Object;
    fn sec_release(object: Object);
    fn SecPolicyCreateSSL(server: bool, hostname: CFTypeRef) -> CFTypeRef;
    fn SecTrustSetPolicies(trust: CFTypeRef, policies: CFTypeRef) -> i32;
    fn SecTrustSetAnchorCertificates(trust: CFTypeRef, anchors: CFTypeRef) -> i32;
    fn SecTrustSetAnchorCertificatesOnly(trust: CFTypeRef, only: bool) -> i32;
    fn SecTrustEvaluateWithError(trust: CFTypeRef, error: *mut CFTypeRef) -> bool;
    fn SecCertificateCreateWithData(allocator: CFTypeRef, data: CFTypeRef) -> CFTypeRef;
    fn SecCertificateCopyKey(certificate: CFTypeRef) -> CFTypeRef;
    fn SecKeyCreateWithData(
        data: CFTypeRef,
        attributes: CFTypeRef,
        error: *mut CFTypeRef,
    ) -> CFTypeRef;
    fn SecKeyCopyPublicKey(key: CFTypeRef) -> CFTypeRef;
    fn SecKeyCopyExternalRepresentation(key: CFTypeRef, error: *mut CFTypeRef) -> CFTypeRef;
    fn SecCopyErrorMessageString(status: i32, reserved: *const c_void) -> CFTypeRef;
}

#[repr(C)]
struct CallBacks([usize; 6]);

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeArrayCallBacks: CallBacks;
    static kCFTypeDictionaryKeyCallBacks: CallBacks;
    static kCFTypeDictionaryValueCallBacks: CallBacks;
    fn CFRelease(value: CFTypeRef);
    fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> bool;
    fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, length: c_long) -> CFTypeRef;
    fn CFArrayCreate(
        allocator: CFTypeRef,
        values: *const CFTypeRef,
        count: c_long,
        callbacks: *const CallBacks,
    ) -> CFTypeRef;
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: c_long,
        key_callbacks: *const CallBacks,
        value_callbacks: *const CallBacks,
    ) -> CFTypeRef;
    fn CFStringCreateWithCString(
        allocator: CFTypeRef,
        text: *const c_char,
        encoding: u32,
    ) -> CFTypeRef;
    fn CFStringGetCString(string: CFTypeRef, buffer: *mut c_char, size: isize, encoding: u32)
        -> u8;
    fn CFErrorGetCode(error: CFTypeRef) -> c_long;
    fn CFErrorCopyDescription(error: CFTypeRef) -> CFTypeRef;
}

extern "C" {
    fn dispatch_queue_create(label: *const c_char, attributes: Object) -> Object;
    fn dispatch_data_create(
        buffer: *const c_void,
        size: usize,
        queue: Object,
        destructor: Object,
    ) -> Object;
    fn dispatch_data_create_map(
        data: Object,
        buffer: *mut *const c_void,
        size: *mut usize,
    ) -> Object;
    fn dispatch_release(object: Object);
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn strerror(code: c_int) -> *const c_char;
}

const UTF8: u32 = 0x0800_0100;
const TLS_V12: u16 = 0x0303;
const TLS_V13: u16 = 0x0304;
const ERR_SEC_HOST_NAME_MISMATCH: c_long = -67602;
const ERR_SEC_INVALID_KEY_USAGE: c_long = -67608;
const ERR_SEC_INVALID_EXTENDED_KEY_USAGE: c_long = -67609;
const ERR_SEC_SSL_BAD_EXTENDED_KEY_USAGE: c_long = -67629;
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

// nw_connection_state_t
const STATE_WAITING: c_int = 1;
const STATE_READY: c_int = 3;
const STATE_FAILED: c_int = 4;
const STATE_CANCELLED: c_int = 5;

// nw_error_domain_t
const DOMAIN_POSIX: c_int = 1;
const DOMAIN_DNS: c_int = 2;
const DOMAIN_TLS: c_int = 3;

const ECONNREFUSED: c_int = 61;
const EHOSTUNREACH: c_int = 65;
const ENETUNREACH: c_int = 51;
const ETIMEDOUT: c_int = 60;
const ENOTCONN: c_int = 57;
const EPIPE: c_int = 32;
const ECONNRESET: c_int = 54;
const ECANCELED: c_int = 89;

// MARK: - CF helpers

/// An owned CF object, released on drop.
struct Cf(CFTypeRef);

// SAFETY: CF objects are immutable here and CF reference counting is thread-safe.
unsafe impl Send for Cf {}
unsafe impl Sync for Cf {}

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own one reference.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn cf_string_text(string: CFTypeRef) -> Option<String> {
    if string.is_null() {
        return None;
    }
    let mut buffer = vec![0 as c_char; 1024];
    // SAFETY: valid CFString; buffer sized as passed.
    (unsafe { CFStringGetCString(string, buffer.as_mut_ptr(), buffer.len() as isize, UTF8) } != 0)
        .then(|| {
            unsafe { CStr::from_ptr(buffer.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        })
}

fn cf_string(text: &str) -> Result<Cf> {
    let c = CString::new(text)
        .map_err(|_| DockerError::Tls(format!("'{text}' contains a NUL byte")))?;
    // SAFETY: valid C string.
    let string = Cf(unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) });
    if string.0.is_null() {
        return Err(DockerError::Tls(
            "could not allocate the server name for certificate verification — check available system memory".into(),
        ));
    }
    Ok(string)
}

fn cf_data(bytes: &[u8]) -> Cf {
    // SAFETY: the slice is valid for its length.
    Cf(unsafe { CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as c_long) })
}

fn cf_array(values: &[CFTypeRef]) -> Result<Cf> {
    // SAFETY: values are live CF objects; the array retains them.
    let array = Cf(unsafe {
        CFArrayCreate(
            std::ptr::null(),
            values.as_ptr(),
            values.len() as c_long,
            &kCFTypeArrayCallBacks,
        )
    });
    if array.0.is_null() {
        return Err(DockerError::Tls(
            "could not allocate the certificate list — check available system memory".into(),
        ));
    }
    Ok(array)
}

/// A CFError's code and description (consumes the reference).
fn take_cf_error(error: CFTypeRef) -> (c_long, String) {
    let error = Cf(error);
    // SAFETY: valid CFError.
    let code = unsafe { CFErrorGetCode(error.0) };
    let description = Cf(unsafe { CFErrorCopyDescription(error.0) });
    (
        code,
        cf_string_text(description.0).unwrap_or_else(|| format!("error {code}")),
    )
}

fn sec_error_message(status: i32) -> String {
    // SAFETY: returns a new CFString or null.
    let message = Cf(unsafe { SecCopyErrorMessageString(status, std::ptr::null()) });
    cf_string_text(message.0).unwrap_or_else(|| format!("OSStatus {status}"))
}

/// A Network.framework error, copied out of the framework object.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NwError {
    domain: c_int,
    code: c_int,
}

impl NwError {
    fn from_object(error: Object) -> Option<NwError> {
        if error.is_null() {
            return None;
        }
        // SAFETY: valid nw_error_t for the duration of the callback.
        Some(unsafe {
            NwError {
                domain: nw_error_get_error_domain(error),
                code: nw_error_get_error_code(error),
            }
        })
    }

    fn describe(&self) -> String {
        match self.domain {
            DOMAIN_POSIX => {
                // SAFETY: strerror returns a static string.
                let text = unsafe { CStr::from_ptr(strerror(self.code)) }.to_string_lossy();
                format!("{text} (POSIX {})", self.code)
            }
            DOMAIN_DNS => format!("DNS error {}", self.code),
            DOMAIN_TLS => format!("{} ({})", sec_error_message(self.code), self.code),
            _ => format!("network error {}", self.code),
        }
    }

    fn is_posix(&self, codes: &[c_int]) -> bool {
        self.domain == DOMAIN_POSIX && codes.contains(&self.code)
    }
}

// MARK: - Waiting

/// A value produced on Network.framework's queue for the waiting thread.
struct Outcome<T> {
    value: Mutex<Option<T>>,
    ready: Condvar,
}

impl<T> Outcome<T> {
    fn new() -> Arc<Outcome<T>> {
        Arc::new(Outcome {
            value: Mutex::new(None),
            ready: Condvar::new(),
        })
    }

    fn finish(&self, value: T) {
        if let Ok(mut slot) = self.value.lock() {
            if slot.is_none() {
                *slot = Some(value);
            }
        }
        self.ready.notify_all();
    }

    /// Waits; None when the timeout passed first or `abort` became true (checked every 100 ms,
    /// because a failed connection does not always complete pending operations).
    fn wait(&self, seconds: u64, abort: impl Fn() -> bool) -> Option<T> {
        let deadline = (seconds > 0).then(|| Instant::now() + Duration::from_secs(seconds));
        let mut slot = self.value.lock().ok()?;
        loop {
            if let Some(value) = slot.take() {
                return Some(value);
            }
            if abort() || deadline.is_some_and(|d| Instant::now() >= d) {
                return None;
            }
            slot = self
                .ready
                .wait_timeout(slot, Duration::from_millis(100))
                .ok()?
                .0;
        }
    }
}

#[derive(Clone, Debug)]
enum TrustFailure {
    Setup(String),
    Evaluation { code: c_long, reason: String },
}

fn trust_setup_status(status: i32, action: &str) -> std::result::Result<(), TrustFailure> {
    if status == 0 {
        Ok(())
    } else {
        Err(TrustFailure::Setup(format!(
            "{action}: {} ({status})",
            sec_error_message(status)
        )))
    }
}

/// Evaluates the peer only after every requested verification policy was applied successfully.
unsafe fn evaluate_server_trust(
    trust: Object,
    server_name: &str,
    anchors: Option<&Cf>,
) -> std::result::Result<(), TrustFailure> {
    if trust.is_null() {
        return Err(TrustFailure::Setup(
            "the TLS framework did not provide the server's trust object".into(),
        ));
    }
    // SAFETY: `trust` comes from the Network.framework verification callback.
    let sec_trust = Cf(unsafe { sec_trust_copy_ref(trust) });
    if sec_trust.0.is_null() {
        return Err(TrustFailure::Setup(
            "could not obtain the server's certificate trust".into(),
        ));
    }
    let hostname =
        cf_string(server_name).map_err(|error| TrustFailure::Setup(error.to_string()))?;
    // SAFETY: hostname is a live CFString and the created policy is owned by this scope.
    let policy = Cf(unsafe { SecPolicyCreateSSL(true, hostname.0) });
    if policy.0.is_null() {
        return Err(TrustFailure::Setup(
            "could not create the TLS server verification policy".into(),
        ));
    }
    // SAFETY: the trust, policy and any anchors are live Security.framework objects.
    unsafe {
        trust_setup_status(
            SecTrustSetPolicies(sec_trust.0, policy.0),
            "could not set the TLS server verification policy",
        )?;
        if let Some(anchors) = anchors {
            trust_setup_status(
                SecTrustSetAnchorCertificates(sec_trust.0, anchors.0),
                "could not set the configured CA certificates",
            )?;
            trust_setup_status(
                SecTrustSetAnchorCertificatesOnly(sec_trust.0, true),
                "could not restrict trust to the configured CA certificates",
            )?;
        }
        let mut error: CFTypeRef = std::ptr::null();
        let trusted = SecTrustEvaluateWithError(sec_trust.0, &mut error);
        if trusted {
            if !error.is_null() {
                CFRelease(error);
            }
            Ok(())
        } else if error.is_null() {
            Err(TrustFailure::Setup(
                "the TLS framework rejected the certificate without an error".into(),
            ))
        } else {
            let (code, reason) = take_cf_error(error);
            Err(TrustFailure::Evaluation { code, reason })
        }
    }
}

#[derive(Default)]
struct SharedState {
    trust_failure: Option<TrustFailure>,
    cancelled: bool,
    received_any: bool,
    /// The connection failed after it was ready (e.g. the daemon rejected us).
    failure: Option<NwError>,
}

// MARK: - Identity

struct Identity(Object);
// SAFETY: sec_identity_t is an immutable, reference-counted OS object.
unsafe impl Send for Identity {}
unsafe impl Sync for Identity {}

impl Identity {
    /// Accessor, so closures capture the `Send` wrapper rather than the raw field.
    fn get(&self) -> Object {
        self.0
    }
}

/// Client identities built from PEM files, cached per (certificate, key) because every request
/// opens a connection.
fn identity(configuration: &TlsConfiguration) -> Result<Option<Object>> {
    let (Some(certificate_file), Some(key_file)) =
        (&configuration.certificate_file, &configuration.key_file)
    else {
        return Ok(None);
    };
    static CACHE: OnceLock<Mutex<HashMap<String, Identity>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = format!("{certificate_file}\0{key_file}");
    if let Some(identity) = cache.lock().ok().and_then(|c| c.get(&key).map(|i| i.0)) {
        return Ok(Some(identity));
    }
    let chain = load_certificates(certificate_file)?;
    let private_key = PrivateKey::parse(&read_file(key_file, "client key")?, key_file)?;
    let (key_type, key_data) = match private_key.kind {
        // SAFETY: reading framework-provided constants.
        KeyKind::Rsa => (unsafe { kSecAttrKeyTypeRSA }, private_key.key_der),
        KeyKind::Ec(_) => {
            let x963 = private_key.x963_representation().ok_or_else(|| {
                DockerError::Tls(format!(
                    "{key_file} is an EC key without its public part; rewrite it with: openssl ec -in {key_file} -out {key_file}"
                ))
            })?;
            (unsafe { kSecAttrKeyTypeECSECPrimeRandom }, x963)
        }
    };
    // SAFETY: CF/Security calls with live objects; every created object is wrapped in `Cf`.
    unsafe {
        let keys = [kSecAttrKeyType, kSecAttrKeyClass];
        let values = [key_type, kSecAttrKeyClassPrivate];
        let attributes = Cf(CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            2,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        ));
        let data = cf_data(&key_data);
        let mut error: CFTypeRef = std::ptr::null();
        let sec_key = Cf(SecKeyCreateWithData(data.0, attributes.0, &mut error));
        if sec_key.0.is_null() {
            let reason = if error.is_null() {
                "unknown error".into()
            } else {
                take_cf_error(error).1
            };
            return Err(DockerError::Tls(format!(
                "macOS could not load the client key {key_file}: {reason}"
            )));
        }
        let mismatch = || {
            DockerError::Tls(format!("the client certificate {certificate_file} and key {key_file} do not belong together"))
        };
        // `SecIdentityCreate` pairs a certificate with an in-memory key. Security.framework
        // exports it (it backs SecPKCS12Import) but the headers do not declare it; the public
        // alternatives all need the keychain.
        let symbol = dlsym(RTLD_DEFAULT, c"SecIdentityCreate".as_ptr());
        if symbol.is_null() {
            return Err(DockerError::Tls(
                "this macOS version cannot use client certificates from files".into(),
            ));
        }
        let create: unsafe extern "C" fn(CFTypeRef, CFTypeRef, CFTypeRef) -> CFTypeRef =
            std::mem::transmute(symbol);
        let sec_identity = Cf(create(std::ptr::null(), chain[0].0, sec_key.0));
        if sec_identity.0.is_null() {
            return Err(mismatch());
        }
        // Check the key matches the certificate before the daemon tells us otherwise.
        let certificate_key = Cf(SecCertificateCopyKey(chain[0].0));
        let public_key = Cf(SecKeyCopyPublicKey(sec_key.0));
        if !certificate_key.0.is_null() && !public_key.0.is_null() {
            let a = Cf(SecKeyCopyExternalRepresentation(
                certificate_key.0,
                std::ptr::null_mut(),
            ));
            let b = Cf(SecKeyCopyExternalRepresentation(
                public_key.0,
                std::ptr::null_mut(),
            ));
            if !a.0.is_null() && !b.0.is_null() && !CFEqual(a.0, b.0) {
                return Err(mismatch());
            }
        }
        let identity = if chain.len() > 1 {
            let rest: Vec<CFTypeRef> = chain[1..].iter().map(|c| c.0).collect();
            let array = cf_array(&rest)?;
            sec_identity_create_with_certificates(sec_identity.0, array.0)
        } else {
            sec_identity_create(sec_identity.0)
        };
        if identity.is_null() {
            return Err(DockerError::Tls(format!(
                "could not build a TLS identity from {certificate_file}"
            )));
        }
        if let Ok(mut cache) = cache.lock() {
            cache.insert(key, Identity(identity));
        }
        Ok(Some(identity))
    }
}

fn load_certificates(file: &str) -> Result<Vec<Cf>> {
    let text = read_file(file, "certificate file")?;
    certificates(&text, file)?
        .iter()
        .map(|der| {
            let data = cf_data(der);
            // SAFETY: valid CFData.
            let certificate = Cf(unsafe { SecCertificateCreateWithData(std::ptr::null(), data.0) });
            if certificate.0.is_null() {
                Err(DockerError::Tls(format!(
                    "{file} contains a certificate macOS cannot parse"
                )))
            } else {
                Ok(certificate)
            }
        })
        .collect()
}

// MARK: - Connection

struct Handle(Object);
// SAFETY: nw_connection_t and dispatch queues are thread-safe OS objects.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    /// Accessor, so closures capture the `Send` wrapper rather than the raw field.
    fn get(&self) -> Object {
        self.0
    }
}

pub struct AppleTlsConnection {
    connection: Handle,
    queue: Handle,
    /// Bounds the handshake and writes.
    timeout_seconds: u64,
    /// Bounds reads; 0 (no limit) once `disable_read_timeout` is called for a stream.
    read_timeout_seconds: AtomicU64,
    configuration: TlsConfiguration,
    endpoint: String,
    server_name: String,
    state: Arc<Mutex<SharedState>>,
    pub negotiated: Option<TlsProtocolVersion>,
}

impl Drop for AppleTlsConnection {
    fn drop(&mut self) {
        // SAFETY: we own one reference to each object.
        unsafe {
            nw_connection_set_state_changed_handler(self.connection.0, std::ptr::null_mut());
            nw_connection_cancel(self.connection.0);
            nw_release(self.connection.0);
            dispatch_release(self.queue.0);
        }
    }
}

impl AppleTlsConnection {
    pub fn connect(
        host: &str,
        port: u16,
        configuration: &TlsConfiguration,
        server_name: &str,
        endpoint: &str,
        timeout_seconds: u64,
    ) -> Result<AppleTlsConnection> {
        let state = Arc::new(Mutex::new(SharedState::default()));
        let identity = identity(configuration)?;
        let anchors = match &configuration.ca_file {
            Some(file) => {
                let certificates = load_certificates(file)?;
                let values: Vec<CFTypeRef> = certificates.iter().map(|c| c.0).collect();
                Some(Arc::new(cf_array(&values)?))
            }
            None => None,
        };
        let name =
            CString::new(server_name).map_err(|_| DockerError::InvalidEndpoint(endpoint.into()))?;
        let host_c =
            CString::new(host).map_err(|_| DockerError::InvalidEndpoint(endpoint.into()))?;
        let port_c = CString::new(port.to_string())
            .map_err(|_| DockerError::InvalidEndpoint(endpoint.into()))?;
        // SAFETY: Network.framework calls with live objects; blocks outlive the calls that take
        // them (the framework copies what it keeps).
        unsafe {
            let queue = dispatch_queue_create(c"dtop.tls".as_ptr(), std::ptr::null_mut());
            let verify_peer = configuration.verify_peer;
            let verify_state = Arc::clone(&state);
            let verify_name = server_name.to_string();
            let identity_object = identity.map(Identity);
            let tls_queue = Handle(queue);
            let set_server_name = !is_ip_address(server_name);
            let name_for_sni = Arc::new(name);
            let configure_tls = OptionsBlock::new(move |options| {
                let security = nw_tls_copy_sec_protocol_options(options);
                sec_protocol_options_set_min_tls_protocol_version(security, TLS_V12);
                if set_server_name {
                    sec_protocol_options_set_tls_server_name(security, name_for_sni.as_ptr());
                }
                if let Some(identity) = &identity_object {
                    sec_protocol_options_set_local_identity(security, identity.get());
                }
                let anchors = anchors.clone();
                let verify_state = Arc::clone(&verify_state);
                let verify_name = verify_name.clone();
                let verify = VerifyBlock::new(move |_metadata, trust, complete| {
                    if !verify_peer {
                        call_bool_block(complete, true);
                        return;
                    }
                    let result = evaluate_server_trust(trust, &verify_name, anchors.as_deref());
                    let trusted = result.is_ok();
                    verify_state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .trust_failure = result.err();
                    call_bool_block(complete, trusted);
                });
                let verify = Box::into_raw(verify);
                sec_protocol_options_set_verify_block(security, verify.cast(), tls_queue.get());
                drop(Box::from_raw(verify));
                sec_release(security);
            });
            let configure_tcp = OptionsBlock::new(move |options| {
                nw_tcp_options_set_no_delay(options, true);
                if timeout_seconds > 0 {
                    nw_tcp_options_set_connection_timeout(options, timeout_seconds as u32);
                }
            });
            let configure_tls = Box::into_raw(configure_tls);
            let configure_tcp = Box::into_raw(configure_tcp);
            let parameters =
                nw_parameters_create_secure_tcp(configure_tls.cast(), configure_tcp.cast());
            drop(Box::from_raw(configure_tls));
            drop(Box::from_raw(configure_tcp));
            let endpoint_object = nw_endpoint_create_host(host_c.as_ptr(), port_c.as_ptr());
            let connection = nw_connection_create(endpoint_object, parameters);
            nw_release(endpoint_object);
            nw_release(parameters);
            nw_connection_set_queue(connection, queue);
            let mut this = AppleTlsConnection {
                connection: Handle(connection),
                queue: Handle(queue),
                timeout_seconds,
                read_timeout_seconds: AtomicU64::new(timeout_seconds),
                configuration: configuration.clone(),
                endpoint: endpoint.into(),
                server_name: server_name.into(),
                state: Arc::clone(&state),
                negotiated: None,
            };
            let ready: Arc<Outcome<Option<NwError>>> = Outcome::new();
            let ready_signal = Arc::clone(&ready);
            let handler = StateBlock::new(move |state, error| match state {
                STATE_READY => ready_signal.finish(None),
                // `waiting` means "no route / refused, will retry": dtop reports and retries itself.
                STATE_FAILED | STATE_WAITING => {
                    ready_signal.finish(Some(NwError::from_object(error).unwrap_or(NwError {
                        domain: DOMAIN_POSIX,
                        code: ECANCELED,
                    })))
                }
                STATE_CANCELLED => ready_signal.finish(Some(NwError {
                    domain: DOMAIN_POSIX,
                    code: ECANCELED,
                })),
                _ => {}
            });
            let handler = Box::into_raw(handler);
            nw_connection_set_state_changed_handler(connection, handler.cast());
            drop(Box::from_raw(handler));
            nw_connection_start(connection);
            let wait = if timeout_seconds > 0 {
                timeout_seconds + 5
            } else {
                60
            };
            let Some(outcome) = ready.wait(wait, || false) else {
                return Err(DockerError::ConnectionFailed {
                    endpoint: endpoint.into(),
                    reason: "timed out".into(),
                });
            };
            if let Some(error) = outcome {
                return Err(this.describe(&error, true));
            }
            let definition = nw_protocol_copy_tls_definition();
            let metadata = nw_connection_copy_protocol_metadata(connection, definition);
            nw_release(definition);
            if !metadata.is_null() {
                let security = nw_tls_copy_sec_protocol_metadata(metadata);
                this.negotiated =
                    match sec_protocol_metadata_get_negotiated_tls_protocol_version(security) {
                        TLS_V13 => Some(TlsProtocolVersion::Tls13),
                        TLS_V12 => Some(TlsProtocolVersion::Tls12),
                        _ => None,
                    };
                sec_release(security);
                nw_release(metadata);
            }
            trace(|| {
                format!(
                    "network.framework: negotiated {}",
                    this.negotiated
                        .map_or("an older protocol", TlsProtocolVersion::as_str)
                )
            });
            // Keep watching: a daemon that rejects our certificate fails the connection after ready.
            let watched = Arc::clone(&state);
            let handler = StateBlock::new(move |state, error| {
                if state == STATE_FAILED {
                    if let Ok(mut s) = watched.lock() {
                        s.failure = NwError::from_object(error);
                    }
                }
            });
            let handler = Box::into_raw(handler);
            nw_connection_set_state_changed_handler(connection, handler.cast());
            drop(Box::from_raw(handler));
            Ok(this)
        }
    }

    fn shared(&self) -> std::sync::MutexGuard<'_, SharedState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn current_failure(&self) -> Option<NwError> {
        self.shared().failure.clone()
    }

    /// The connection failed after it was ready: before any response, the daemon refusing us
    /// (usually the client certificate); later, an ordinary I/O failure.
    fn failed_connection(&self, error: &NwError) -> DockerError {
        if !self.shared().received_any {
            return DockerError::Tls(format!(
                "{} closed the connection before answering ({}) — {}",
                self.endpoint,
                error.describe(),
                closed_during_handshake_hint(&self.configuration)
            ));
        }
        self.describe(error, false)
    }

    fn describe(&self, error: &NwError, during_handshake: bool) -> DockerError {
        if let Some(failure) = self.shared().trust_failure.clone() {
            let (code, reason) = match failure {
                TrustFailure::Setup(reason) => {
                    return DockerError::Tls(format!(
                        "could not configure certificate verification for {} ({reason}) — \
                         check system resources and the configured TLS certificates",
                        self.endpoint
                    ));
                }
                TrustFailure::Evaluation { code, reason } => (code, reason),
            };
            trace(|| format!("server trust evaluation failed ({code}): {reason}"));
            if code == ERR_SEC_HOST_NAME_MISMATCH
                || reason.to_lowercase().contains("does not match")
            {
                return DockerError::Tls(name_mismatch_message(&self.server_name, &self.endpoint));
            }
            if matches!(
                code,
                ERR_SEC_INVALID_KEY_USAGE
                    | ERR_SEC_INVALID_EXTENDED_KEY_USAGE
                    | ERR_SEC_SSL_BAD_EXTENDED_KEY_USAGE
            ) {
                return DockerError::Tls(format!(
                    "macOS rejected the server certificate of {} ({reason}) — reissue it for \
                     TLS server use with Extended Key Usage serverAuth and appropriate key usage",
                    self.endpoint
                ));
            }
            return DockerError::Tls(untrusted_message(
                &reason,
                &self.endpoint,
                &self.configuration,
            ));
        }
        match error.domain {
            DOMAIN_TLS => DockerError::Tls(handshake_message(
                &error.describe(),
                &self.endpoint,
                &self.configuration,
            )),
            DOMAIN_POSIX if during_handshake => {
                if error.is_posix(&[ECONNREFUSED, EHOSTUNREACH, ENETUNREACH, ETIMEDOUT]) {
                    DockerError::ConnectionFailed {
                        endpoint: self.endpoint.clone(),
                        reason: error.describe(),
                    }
                } else {
                    DockerError::Tls(handshake_message(
                        &error.describe(),
                        &self.endpoint,
                        &self.configuration,
                    ))
                }
            }
            DOMAIN_DNS => DockerError::ConnectionFailed {
                endpoint: self.endpoint.clone(),
                reason: error.describe(),
            },
            _ => DockerError::Io(error.describe()),
        }
    }

    fn cancel(&self) {
        self.shared().cancelled = true;
        // SAFETY: valid connection.
        unsafe { nw_connection_cancel(self.connection.0) };
    }
}

impl Connection for AppleTlsConnection {
    fn write_all(&self, bytes: &[u8]) -> Result<()> {
        let done: Arc<Outcome<Option<NwError>>> = Outcome::new();
        let signal = Arc::clone(&done);
        // SAFETY: the data object copies the bytes (default destructor); the block is copied by
        // the framework before nw_connection_send returns.
        unsafe {
            let data = dispatch_data_create(
                bytes.as_ptr().cast(),
                bytes.len(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            let completion = Box::into_raw(ErrorBlock::new(move |error| {
                signal.finish(NwError::from_object(error))
            }));
            let context = _nw_content_context_default_message;
            nw_connection_send(self.connection.0, data, context, true, completion.cast());
            drop(Box::from_raw(completion));
            dispatch_release(data);
        }
        let Some(outcome) = done.wait(self.timeout_seconds, || self.current_failure().is_some())
        else {
            self.cancel();
            if let Some(failure) = self.current_failure() {
                return Err(self.failed_connection(&failure));
            }
            return Err(DockerError::TimedOut("socket write".into()));
        };
        if let Some(error) = outcome {
            // A daemon that refuses our client certificate drops the connection right after the
            // handshake, so the request write is what fails.
            if !self.shared().received_any && error.is_posix(&[ENOTCONN, EPIPE, ECONNRESET]) {
                return Err(DockerError::Tls(format!(
                    "{} closed the connection before answering ({}) — {}",
                    self.endpoint,
                    error.describe(),
                    closed_during_handshake_hint(&self.configuration)
                )));
            }
            return Err(self.describe(&error, false));
        }
        Ok(())
    }

    fn read(&self, max_len: usize) -> Result<Vec<u8>> {
        type Received = (Vec<u8>, Option<NwError>);
        let done: Arc<Outcome<Received>> = Outcome::new();
        let signal = Arc::clone(&done);
        // SAFETY: the content map is released after copying its bytes.
        unsafe {
            let completion = Box::into_raw(ReceiveBlock::new(
                move |content, _context, _is_complete, error| {
                    let mut bytes = Vec::new();
                    if !content.is_null() {
                        let mut pointer: *const c_void = std::ptr::null();
                        let mut size = 0usize;
                        let map = dispatch_data_create_map(content, &mut pointer, &mut size);
                        if !pointer.is_null() {
                            bytes.extend_from_slice(std::slice::from_raw_parts(
                                pointer.cast::<u8>(),
                                size,
                            ));
                        }
                        if !map.is_null() {
                            dispatch_release(map);
                        }
                    }
                    signal.finish((bytes, NwError::from_object(error)));
                },
            ));
            nw_connection_receive(
                self.connection.0,
                1,
                max_len.clamp(1, u32::MAX as usize) as u32,
                completion.cast(),
            );
            drop(Box::from_raw(completion));
        }
        let read_timeout = self.read_timeout_seconds.load(Ordering::SeqCst);
        let Some((data, error)) = done.wait(read_timeout, || self.current_failure().is_some())
        else {
            self.cancel();
            if let Some(failure) = self.current_failure() {
                return Err(self.failed_connection(&failure));
            }
            return Err(DockerError::TimedOut("socket read".into()));
        };
        let (cancelled, received_any) = {
            let mut s = self.shared();
            let previously = s.received_any;
            if !data.is_empty() {
                s.received_any = true;
            }
            (s.cancelled, previously)
        };
        if !data.is_empty() {
            return Ok(data);
        }
        if cancelled {
            return Err(DockerError::Cancelled);
        }
        if !received_any {
            // With TLS 1.3 a daemon that rejects (or misses) our client certificate does so after
            // our side of the handshake, by closing before it answers anything.
            let detail = error
                .as_ref()
                .map_or_else(|| "connection closed".to_string(), NwError::describe);
            return Err(DockerError::Tls(format!(
                "{} closed the connection before answering ({detail}) — {}",
                self.endpoint,
                closed_during_handshake_hint(&self.configuration)
            )));
        }
        match error {
            // A peer that resets after sending everything still ends the stream.
            Some(e) if e.is_posix(&[ECONNRESET, ENOTCONN]) => Ok(Vec::new()),
            Some(e) => Err(self.describe(&e, false)),
            None => Ok(Vec::new()),
        }
    }

    fn disable_read_timeout(&self) -> Result<()> {
        self.read_timeout_seconds.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn interrupt(&self) {
        self.cancel();
    }

    fn close(&self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_server_trust_is_rejected() {
        let result = unsafe { evaluate_server_trust(std::ptr::null_mut(), "localhost", None) };
        assert!(matches!(result, Err(TrustFailure::Setup(_))));
    }

    #[test]
    fn failed_trust_configuration_is_reported() {
        assert!(trust_setup_status(0, "set anchors").is_ok());
        let error = trust_setup_status(-50, "set anchors").unwrap_err();
        assert!(matches!(error, TrustFailure::Setup(message) if message.contains("set anchors")));
    }

    #[test]
    fn blocks_invoke_and_survive_copies() {
        extern "C" {
            fn _Block_copy(block: *const c_void) -> *mut c_void;
            fn _Block_release(block: *const c_void);
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let block = ErrorBlock::new(move |error| record.lock().unwrap().push(error as usize));
        let raw = Box::into_raw(block);
        // SAFETY: a valid block literal; the copy is released below.
        unsafe {
            let copy = _Block_copy(raw.cast()) as *mut ErrorBlock;
            drop(Box::from_raw(raw));
            ((*copy).invoke)(copy, 7 as Object);
            _Block_release(copy.cast());
        }
        assert_eq!(*seen.lock().unwrap(), vec![7]);
        assert_eq!(Arc::strong_count(&seen), 1, "copies released their closure");
    }
}
