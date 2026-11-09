//! PEM files, minimal DER, and private keys in the forms the platform TLS stacks want. Pure
//! Rust: base64, ASN.1 and key conversion (PKCS#1, SEC1, PKCS#8 → PKCS#8 and X9.63).

use std::collections::BTreeMap;

use crate::error::{DockerError, Result};

/// One `-----BEGIN <label>-----` block, base64-decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PemBlock {
    pub label: String,
    pub der: Vec<u8>,
    /// RFC 1421 headers such as `Proc-Type: 4,ENCRYPTED` (legacy encrypted keys).
    pub headers: BTreeMap<String, String>,
}

/// Decodes base64, skipping characters outside the alphabet.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Parses every PEM block, ignoring anything outside the blocks.
pub fn parse_pem(text: &str) -> Result<Vec<PemBlock>> {
    let mut blocks = Vec::new();
    let mut label: Option<String> = None;
    let mut headers = BTreeMap::new();
    let mut body = String::new();
    for raw in text.split(['\n', '\r']) {
        let line = raw.trim();
        if let Some(current) = &label {
            if line == format!("-----END {current}-----") {
                let der = base64_decode(&body).ok_or_else(|| {
                    DockerError::Tls(format!("the PEM block '{current}' is not valid base64"))
                })?;
                blocks.push(PemBlock {
                    label: current.clone(),
                    der,
                    headers: std::mem::take(&mut headers),
                });
                label = None;
            } else if let (Some(colon), true) = (line.find(':'), body.is_empty()) {
                headers.insert(
                    line[..colon].trim().to_string(),
                    line[colon + 1..].trim().to_string(),
                );
            } else {
                body.push_str(line);
            }
        } else if line.starts_with("-----BEGIN ") && line.ends_with("-----") && line.len() > 16 {
            label = Some(line[11..line.len() - 5].to_string());
            headers.clear();
            body.clear();
        }
    }
    if let Some(label) = label {
        return Err(DockerError::Tls(format!(
            "the PEM block '{label}' has no END line"
        )));
    }
    Ok(blocks)
}

/// DER certificates from a PEM file, in file order (leaf first for a chain).
pub fn certificates(text: &str, file: &str) -> Result<Vec<Vec<u8>>> {
    let certs: Vec<Vec<u8>> = parse_pem(text)?
        .into_iter()
        .filter(|b| b.label == "CERTIFICATE" || b.label == "X509 CERTIFICATE")
        .map(|b| b.der)
        .collect();
    if certs.is_empty() {
        return Err(DockerError::Tls(format!(
            "{file} contains no PEM certificate (expected -----BEGIN CERTIFICATE-----)"
        )));
    }
    Ok(certs)
}

pub mod der {
    //! Just enough ASN.1 DER for private keys.
    use super::*;

    pub const SEQUENCE: u8 = 0x30;
    pub const INTEGER: u8 = 0x02;
    pub const BIT_STRING: u8 = 0x03;
    pub const OCTET_STRING: u8 = 0x04;
    pub const NULL: u8 = 0x05;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Element {
        pub tag: u8,
        pub content: Vec<u8>,
    }

    impl Element {
        /// Children of a constructed element (SEQUENCE, context-specific [n]).
        pub fn children(&self) -> Result<Vec<Element>> {
            parse_all(&self.content)
        }
    }

    pub fn malformed(reason: &str) -> DockerError {
        DockerError::Tls(format!("the private key is not valid DER ({reason})"))
    }

    /// One element at the start of `bytes`, and how many bytes it used.
    pub fn parse_one(bytes: &[u8]) -> Result<(Element, usize)> {
        if bytes.len() < 2 {
            return Err(malformed("truncated header"));
        }
        let tag = bytes[0];
        if tag & 0x1F == 0x1F {
            return Err(malformed("multi-byte tags are not supported"));
        }
        let mut index = 2;
        let mut length = usize::from(bytes[1]);
        if length & 0x80 != 0 {
            let count = length & 0x7F;
            if count == 0 || count > 4 || bytes.len() - index < count {
                return Err(malformed("bad length"));
            }
            length = bytes[index..index + count]
                .iter()
                .fold(0usize, |acc, b| acc << 8 | usize::from(*b));
            index += count;
        }
        if bytes.len() - index < length {
            return Err(malformed("length exceeds input"));
        }
        Ok((
            Element {
                tag,
                content: bytes[index..index + length].to_vec(),
            },
            index + length,
        ))
    }

    pub fn parse_all(bytes: &[u8]) -> Result<Vec<Element>> {
        let mut elements = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            let (element, used) = parse_one(rest)?;
            elements.push(element);
            rest = &rest[used..];
        }
        Ok(elements)
    }

    /// A single top-level element that fills `bytes`.
    pub fn parse(bytes: &[u8]) -> Result<Element> {
        let mut elements = parse_all(bytes)?;
        if elements.len() != 1 {
            return Err(malformed("expected one top-level element"));
        }
        Ok(elements.remove(0))
    }

    pub fn encode(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        let length = content.len();
        if length < 0x80 {
            out.push(length as u8);
        } else {
            let bytes: Vec<u8> = length
                .to_be_bytes()
                .into_iter()
                .skip_while(|b| *b == 0)
                .collect();
            out.push(0x80 | bytes.len() as u8);
            out.extend(bytes);
        }
        out.extend_from_slice(content);
        out
    }
}

/// Curves dtop loads keys for (the ones Docker's tooling generates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcCurve {
    P256,
    P384,
    P521,
}

impl EcCurve {
    pub const ALL: [EcCurve; 3] = [EcCurve::P256, EcCurve::P384, EcCurve::P521];

    /// DER of the curve's OBJECT IDENTIFIER (tag and length included).
    pub fn oid(self) -> &'static [u8] {
        match self {
            EcCurve::P256 => &[0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07],
            EcCurve::P384 => &[0x06, 0x05, 0x2B, 0x81, 0x04, 0x00, 0x22],
            EcCurve::P521 => &[0x06, 0x05, 0x2B, 0x81, 0x04, 0x00, 0x23],
        }
    }

    /// Size of a private scalar / coordinate in bytes.
    pub fn byte_count(self) -> usize {
        match self {
            EcCurve::P256 => 32,
            EcCurve::P384 => 48,
            EcCurve::P521 => 66,
        }
    }

    fn from_oid(element: &der::Element) -> Option<EcCurve> {
        let encoded = der::encode(element.tag, &element.content);
        EcCurve::ALL
            .into_iter()
            .find(|c| c.oid() == encoded.as_slice())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Rsa,
    Ec(EcCurve),
}

/// An unencrypted private key read from PEM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateKey {
    pub kind: KeyKind,
    /// PKCS#1 RSAPrivateKey (RSA) or SEC1 ECPrivateKey (EC) DER.
    pub key_der: Vec<u8>,
    /// EC: the public point (`04 || X || Y`), when the key carries it.
    pub ec_public_point: Option<Vec<u8>>,
    /// EC: the private scalar, left-padded to the curve size.
    pub ec_private_scalar: Option<Vec<u8>>,
}

const RSA_OID: [u8; 11] = [
    0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01,
];
const EC_PUBLIC_KEY_OID: [u8; 9] = [0x06, 0x07, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];

impl PrivateKey {
    /// The first private key in a PEM file: PKCS#1 (`RSA PRIVATE KEY`), SEC1 (`EC PRIVATE KEY`)
    /// or unencrypted PKCS#8 (`PRIVATE KEY`).
    pub fn parse(text: &str, file: &str) -> Result<PrivateKey> {
        let blocks = parse_pem(text)?;
        let encrypted = blocks.iter().any(|b| {
            b.label == "ENCRYPTED PRIVATE KEY"
                || b.headers
                    .get("Proc-Type")
                    .is_some_and(|v| v.to_uppercase().contains("ENCRYPTED"))
        });
        if encrypted {
            return Err(DockerError::Tls(format!(
                "{file} is encrypted; dtop needs an unencrypted key (decrypt it with: openssl pkey -in {file} -out key-plain.pem)"
            )));
        }
        let block = blocks
            .iter()
            .find(|b| matches!(b.label.as_str(), "RSA PRIVATE KEY" | "EC PRIVATE KEY" | "PRIVATE KEY"))
            .ok_or_else(|| {
                DockerError::Tls(format!(
                    "{file} contains no private key (expected -----BEGIN PRIVATE KEY-----, RSA PRIVATE KEY or EC PRIVATE KEY)"
                ))
            })?;
        match block.label.as_str() {
            "RSA PRIVATE KEY" => {
                der::parse(&block.der)?;
                Ok(PrivateKey {
                    kind: KeyKind::Rsa,
                    key_der: block.der.clone(),
                    ec_public_point: None,
                    ec_private_scalar: None,
                })
            }
            "EC PRIVATE KEY" => Self::ec(&block.der, None),
            _ => Self::pkcs8(&block.der),
        }
    }

    fn pkcs8(bytes: &[u8]) -> Result<PrivateKey> {
        let fields = der::parse(bytes)?.children()?;
        if fields.len() < 3
            || fields[0].tag != der::INTEGER
            || fields[1].tag != der::SEQUENCE
            || fields[2].tag != der::OCTET_STRING
        {
            return Err(der::malformed("PKCS#8 structure"));
        }
        let algorithm = fields[1].children()?;
        let oid = algorithm
            .first()
            .ok_or_else(|| der::malformed("missing algorithm"))?;
        let encoded = der::encode(oid.tag, &oid.content);
        if encoded == RSA_OID {
            return Ok(PrivateKey {
                kind: KeyKind::Rsa,
                key_der: fields[2].content.clone(),
                ec_public_point: None,
                ec_private_scalar: None,
            });
        }
        if encoded == EC_PUBLIC_KEY_OID {
            let curve = algorithm.get(1).and_then(EcCurve::from_oid);
            return Self::ec(&fields[2].content, curve);
        }
        Err(DockerError::Tls(
            "unsupported private key algorithm (dtop supports RSA and ECDSA P-256/P-384/P-521 keys)".into(),
        ))
    }

    fn ec(sec1: &[u8], outer: Option<EcCurve>) -> Result<PrivateKey> {
        let fields = der::parse(sec1)?.children()?;
        if fields.len() < 2 || fields[0].tag != der::INTEGER || fields[1].tag != der::OCTET_STRING {
            return Err(der::malformed("SEC1 structure"));
        }
        let mut curve = outer;
        let mut public_point = None;
        for field in &fields[2..] {
            if field.tag == 0xA0 {
                if let Some(oid) = field.children()?.first() {
                    curve = EcCurve::from_oid(oid).or(curve);
                }
            } else if field.tag == 0xA1 {
                if let Some(bits) = field
                    .children()?
                    .first()
                    .filter(|b| b.tag == der::BIT_STRING && b.content.len() > 1)
                {
                    public_point = Some(bits.content[1..].to_vec()); // skip the unused-bits byte
                }
            }
        }
        let curve = curve.ok_or_else(|| {
            DockerError::Tls(
                "unsupported elliptic curve (dtop supports P-256, P-384 and P-521)".into(),
            )
        })?;
        let mut scalar = fields[1].content.clone();
        while scalar.len() > curve.byte_count() && scalar.first() == Some(&0) {
            scalar.remove(0);
        }
        if scalar.len() < curve.byte_count() {
            let mut padded = vec![0u8; curve.byte_count() - scalar.len()];
            padded.extend(scalar);
            scalar = padded;
        }
        Ok(PrivateKey {
            kind: KeyKind::Ec(curve),
            key_der: sec1.to_vec(),
            ec_public_point: public_point,
            ec_private_scalar: Some(scalar),
        })
    }

    /// Unencrypted PKCS#8 PrivateKeyInfo DER (what Windows CNG imports).
    pub fn pkcs8_der(&self) -> Vec<u8> {
        let version = der::encode(der::INTEGER, &[0]);
        let algorithm = match self.kind {
            KeyKind::Rsa => {
                let mut content = RSA_OID.to_vec();
                content.extend([der::NULL, 0]);
                der::encode(der::SEQUENCE, &content)
            }
            KeyKind::Ec(curve) => {
                let mut content = EC_PUBLIC_KEY_OID.to_vec();
                content.extend_from_slice(curve.oid());
                der::encode(der::SEQUENCE, &content)
            }
        };
        let mut body = version;
        body.extend(algorithm);
        body.extend(der::encode(der::OCTET_STRING, &self.key_der));
        der::encode(der::SEQUENCE, &body)
    }

    /// EC key in ANSI X9.63 form (`04 || X || Y || D`) for Apple's `SecKeyCreateWithData`.
    pub fn x963_representation(&self) -> Option<Vec<u8>> {
        let KeyKind::Ec(_) = self.kind else {
            return None;
        };
        let point = self
            .ec_public_point
            .as_ref()
            .filter(|p| p.first() == Some(&0x04))?;
        let mut out = point.clone();
        out.extend(self.ec_private_scalar.as_ref()?);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway P-256 key (SEC1) generated for these tests only.
    const EC_SEC1: &str = "-----BEGIN EC PRIVATE KEY-----
MHcCAQEEIDWgaJVzegqK0W4VbPSDh+ITRNFT1o/4iVM9ME87HQEmoAoGCCqGSM49
AwEHoUQDQgAEFDwpI1vhAXvwuhNo7txQLP8kZbnkBqkuN7Kh4GnsaoKGfru4cf1F
yUFLBvSU4CrzNWrYiuRLIC8b0nVT2hGc6g==
-----END EC PRIVATE KEY-----
";

    #[test]
    fn decodes_base64_ignoring_whitespace() {
        assert_eq!(base64_decode("aGVs\nbG8=").unwrap(), b"hello");
    }

    #[test]
    fn parses_sec1_ec_keys_and_converts() {
        let key = PrivateKey::parse(EC_SEC1, "key.pem").unwrap();
        assert_eq!(key.kind, KeyKind::Ec(EcCurve::P256));
        assert_eq!(key.ec_private_scalar.as_ref().unwrap().len(), 32);
        let x963 = key.x963_representation().unwrap();
        assert_eq!(x963.len(), 65 + 32);
        // PKCS#8 round trip.
        let pkcs8 = key.pkcs8_der();
        let reparsed = PrivateKey::pkcs8(&pkcs8).unwrap();
        assert_eq!(reparsed, key);
    }

    #[test]
    fn rejects_encrypted_and_missing_keys() {
        let encrypted =
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n";
        assert!(PrivateKey::parse(encrypted, "k")
            .unwrap_err()
            .to_string()
            .contains("encrypted"));
        assert!(PrivateKey::parse("nothing", "k")
            .unwrap_err()
            .to_string()
            .contains("no private key"));
        assert!(parse_pem("-----BEGIN X-----\nAAAA\n").is_err());
        assert!(certificates("", "c.pem").is_err());
    }

    #[test]
    fn der_lengths_round_trip() {
        let long = vec![7u8; 300];
        let encoded = der::encode(der::OCTET_STRING, &long);
        assert_eq!(&encoded[..4], &[0x04, 0x82, 0x01, 0x2C]);
        assert_eq!(der::parse(&encoded).unwrap().content, long);
    }
}
