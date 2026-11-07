import Foundation

/// One `-----BEGIN <label>-----` block of a PEM file, base64-decoded.
public struct PEMBlock: Sendable, Equatable {
    public var label: String
    public var der: [UInt8]
    /// RFC 1421 headers such as `Proc-Type: 4,ENCRYPTED` (legacy encrypted keys).
    public var headers: [String: String]

    public init(label: String, der: [UInt8], headers: [String: String] = [:]) {
        self.label = label
        self.der = der
        self.headers = headers
    }
}

public enum PEM {
    /// Parses every PEM block in `text`, ignoring anything outside the blocks.
    public static func parse(_ text: String) throws -> [PEMBlock] {
        var blocks: [PEMBlock] = []
        var label: String?
        var headers: [String: String] = [:]
        var body = ""
        for rawLine in text.split(omittingEmptySubsequences: false, whereSeparator: { $0 == "\n" || $0 == "\r\n" }) {
            let line = String(rawLine).trimmingWhitespace()
            if let current = label {
                if line == "-----END \(current)-----" {
                    guard let data = Data(base64Encoded: body, options: .ignoreUnknownCharacters), !data.isEmpty else {
                        throw DockerError.tls("the PEM block '\(current)' is not valid base64")
                    }
                    blocks.append(PEMBlock(label: current, der: [UInt8](data), headers: headers))
                    label = nil
                } else if let colon = line.firstIndex(of: ":"), body.isEmpty {
                    headers[String(line[..<colon]).trimmingWhitespace()] = String(line[line.index(after: colon)...]).trimmingWhitespace()
                } else {
                    body += line
                }
            } else if line.hasPrefix("-----BEGIN "), line.hasSuffix("-----"), line.count > 16 {
                label = String(line.dropFirst(11).dropLast(5))
                headers = [:]
                body = ""
            }
        }
        if let label {
            throw DockerError.tls("the PEM block '\(label)' has no END line")
        }
        return blocks
    }

    /// DER certificates from a PEM file, in file order (leaf first for a chain).
    public static func certificates(_ text: String, file: String) throws -> [[UInt8]] {
        let certificates = try parse(text).filter { $0.label == "CERTIFICATE" || $0.label == "X509 CERTIFICATE" }.map(\.der)
        guard !certificates.isEmpty else {
            throw DockerError.tls("\(file) contains no PEM certificate (expected -----BEGIN CERTIFICATE-----)")
        }
        return certificates
    }
}

/// Minimal DER (ASN.1) reading and writing: just enough for private keys.
public enum DER {
    public struct Element: Sendable, Equatable {
        public var tag: UInt8
        public var content: [UInt8]

        /// Children of a constructed element (SEQUENCE, context-specific [n]).
        public func children() throws -> [Element] {
            try DER.parseAll(content)
        }
    }

    public static let sequence: UInt8 = 0x30
    public static let integer: UInt8 = 0x02
    public static let bitString: UInt8 = 0x03
    public static let octetString: UInt8 = 0x04
    public static let null: UInt8 = 0x05
    public static let objectIdentifier: UInt8 = 0x06

    /// Parses one element at the start of `bytes`; returns it and the number of bytes it used.
    public static func parseOne(_ bytes: ArraySlice<UInt8>) throws -> (Element, Int) {
        guard bytes.count >= 2 else { throw malformed("truncated header") }
        var index = bytes.startIndex
        let tag = bytes[index]
        guard tag & 0x1F != 0x1F else { throw malformed("multi-byte tags are not supported") }
        index += 1
        var length = Int(bytes[index])
        index += 1
        if length & 0x80 != 0 {
            let count = length & 0x7F
            guard count > 0, count <= 4, bytes.endIndex - index >= count else { throw malformed("bad length") }
            length = 0
            for _ in 0..<count {
                length = length << 8 | Int(bytes[index])
                index += 1
            }
        }
        guard length >= 0, bytes.endIndex - index >= length else { throw malformed("length exceeds input") }
        let content = Array(bytes[index..<(index + length)])
        return (Element(tag: tag, content: content), index + length - bytes.startIndex)
    }

    /// Parses a run of consecutive elements that fills `bytes` exactly.
    public static func parseAll(_ bytes: [UInt8]) throws -> [Element] {
        var elements: [Element] = []
        var slice = bytes[...]
        while !slice.isEmpty {
            let (element, used) = try parseOne(slice)
            elements.append(element)
            slice = slice.dropFirst(used)
        }
        return elements
    }

    /// Parses a single top-level element that must fill `bytes`.
    public static func parse(_ bytes: [UInt8]) throws -> Element {
        let elements = try parseAll(bytes)
        guard elements.count == 1 else { throw malformed("expected one top-level element") }
        return elements[0]
    }

    public static func encode(tag: UInt8, _ content: [UInt8]) -> [UInt8] {
        var output = [tag]
        let length = content.count
        if length < 0x80 {
            output.append(UInt8(length))
        } else {
            var lengthBytes: [UInt8] = []
            var remaining = length
            while remaining > 0 {
                lengthBytes.insert(UInt8(remaining & 0xFF), at: 0)
                remaining >>= 8
            }
            output.append(0x80 | UInt8(lengthBytes.count))
            output += lengthBytes
        }
        return output + content
    }

    static func malformed(_ reason: String) -> DockerError {
        .tls("the private key is not valid DER (\(reason))")
    }
}

/// Elliptic curves dtop can load keys for (the ones Docker's tooling generates).
public enum ECCurve: String, Sendable, Equatable, CaseIterable {
    case p256 = "P-256"
    case p384 = "P-384"
    case p521 = "P-521"

    /// DER encoding of the curve's OBJECT IDENTIFIER (tag and length included).
    public var oid: [UInt8] {
        switch self {
        case .p256: return [0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07]
        case .p384: return [0x06, 0x05, 0x2B, 0x81, 0x04, 0x00, 0x22]
        case .p521: return [0x06, 0x05, 0x2B, 0x81, 0x04, 0x00, 0x23]
        }
    }

    /// Size of a private scalar / coordinate in bytes.
    public var byteCount: Int {
        switch self {
        case .p256: return 32
        case .p384: return 48
        case .p521: return 66
        }
    }

    init?(oidElement: DER.Element) {
        let encoded = DER.encode(tag: oidElement.tag, oidElement.content)
        guard let curve = ECCurve.allCases.first(where: { $0.oid == encoded }) else { return nil }
        self = curve
    }
}

/// An unencrypted private key read from PEM, in the forms the platform TLS stacks want.
public struct PrivateKey: Sendable, Equatable {
    public enum Kind: Sendable, Equatable {
        case rsa
        case ec(ECCurve)
    }

    public var kind: Kind
    /// PKCS#1 RSAPrivateKey (RSA) or SEC1 ECPrivateKey (EC) DER.
    public var keyDER: [UInt8]
    /// For EC keys: the public point (`04 || X || Y`), when the key carries it.
    public var ecPublicPoint: [UInt8]?
    /// For EC keys: the private scalar, left-padded to the curve size.
    public var ecPrivateScalar: [UInt8]?

    static let rsaOID: [UInt8] = [0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01]
    static let ecPublicKeyOID: [UInt8] = [0x06, 0x07, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01]

    /// Reads the first private key in a PEM file. Accepts PKCS#1 (`RSA PRIVATE KEY`), SEC1
    /// (`EC PRIVATE KEY`) and unencrypted PKCS#8 (`PRIVATE KEY`).
    public static func parse(pem text: String, file: String) throws -> PrivateKey {
        let blocks = try PEM.parse(text)
        if blocks.contains(where: { $0.label == "ENCRYPTED PRIVATE KEY" })
            || blocks.contains(where: { ($0.headers["Proc-Type"] ?? "").uppercased().contains("ENCRYPTED") }) {
            throw DockerError.tls("\(file) is encrypted; dtop needs an unencrypted key "
                + "(decrypt it with: openssl pkey -in \(file) -out key-plain.pem)")
        }
        guard let block = blocks.first(where: { ["RSA PRIVATE KEY", "EC PRIVATE KEY", "PRIVATE KEY"].contains($0.label) }) else {
            throw DockerError.tls("\(file) contains no private key (expected -----BEGIN PRIVATE KEY-----, "
                + "RSA PRIVATE KEY or EC PRIVATE KEY)")
        }
        switch block.label {
        case "RSA PRIVATE KEY":
            _ = try DER.parse(block.der)
            return PrivateKey(kind: .rsa, keyDER: block.der)
        case "EC PRIVATE KEY":
            return try ec(sec1: block.der, curve: nil)
        default:
            return try pkcs8(block.der)
        }
    }

    static func pkcs8(_ der: [UInt8]) throws -> PrivateKey {
        let fields = try DER.parse(der).children()
        guard fields.count >= 3, fields[0].tag == DER.integer, fields[1].tag == DER.sequence,
              fields[2].tag == DER.octetString else {
            throw DER.malformed("PKCS#8 structure")
        }
        let algorithm = try fields[1].children()
        guard let oid = algorithm.first else { throw DER.malformed("missing algorithm") }
        let encodedOID = DER.encode(tag: oid.tag, oid.content)
        if encodedOID == rsaOID {
            return PrivateKey(kind: .rsa, keyDER: fields[2].content)
        }
        if encodedOID == ecPublicKeyOID {
            let curve = algorithm.count > 1 ? ECCurve(oidElement: algorithm[1]) : nil
            return try ec(sec1: fields[2].content, curve: curve)
        }
        throw DockerError.tls("unsupported private key algorithm (dtop supports RSA and ECDSA P-256/P-384/P-521 keys)")
    }

    static func ec(sec1 der: [UInt8], curve outerCurve: ECCurve?) throws -> PrivateKey {
        let fields = try DER.parse(der).children()
        guard fields.count >= 2, fields[0].tag == DER.integer, fields[1].tag == DER.octetString else {
            throw DER.malformed("SEC1 structure")
        }
        var curve = outerCurve
        var publicPoint: [UInt8]?
        for field in fields.dropFirst(2) {
            if field.tag == 0xA0, let oid = try field.children().first {
                curve = ECCurve(oidElement: oid) ?? curve
            } else if field.tag == 0xA1, let bits = try field.children().first, bits.tag == DER.bitString,
                      bits.content.count > 1 {
                publicPoint = Array(bits.content.dropFirst()) // skip the unused-bits byte
            }
        }
        guard let curve else {
            throw DockerError.tls("unsupported elliptic curve (dtop supports P-256, P-384 and P-521)")
        }
        var scalar = fields[1].content
        while scalar.count > curve.byteCount, scalar.first == 0 { scalar.removeFirst() }
        if scalar.count < curve.byteCount { scalar = [UInt8](repeating: 0, count: curve.byteCount - scalar.count) + scalar }
        return PrivateKey(kind: .ec(curve), keyDER: der, ecPublicPoint: publicPoint, ecPrivateScalar: scalar)
    }

    /// The key as unencrypted PKCS#8 PrivateKeyInfo DER (what Windows CNG imports).
    public var pkcs8DER: [UInt8] {
        let version = DER.encode(tag: DER.integer, [0])
        let algorithm: [UInt8]
        switch kind {
        case .rsa:
            algorithm = DER.encode(tag: DER.sequence, Self.rsaOID + [DER.null, 0])
        case .ec(let curve):
            algorithm = DER.encode(tag: DER.sequence, Self.ecPublicKeyOID + curve.oid)
        }
        return DER.encode(tag: DER.sequence, version + algorithm + DER.encode(tag: DER.octetString, keyDER))
    }

    /// The EC key in ANSI X9.63 form (`04 || X || Y || D`), which Apple's `SecKeyCreateWithData`
    /// expects. Nil for RSA keys or EC keys stored without their public point.
    public var x963Representation: [UInt8]? {
        guard case .ec = kind, let point = ecPublicPoint, point.first == 0x04, let scalar = ecPrivateScalar else { return nil }
        return point + scalar
    }
}
