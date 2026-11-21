//! The walk of a certificate's DER that picks the hash of its channel
//! binding, RFC 5929's `tls-server-end-point` (`fennec_tls::server_end_point`):
//! the bytes are whoever answered a dial's, under `prefer` or `require`
//! anyone's, so three elements are read and no further, each length held to
//! the bytes there.
//!
//! std alone, no other crate's name and nothing of this one's: the fuzzer,
//! which takes nothing from outside the workspace, compiles this file
//! itself (`crates/fennec-fuzz`, its `certificate` target, by `#[path]`),
//! where depending on this crate would bring rustls and ring into it.

/// One DER element at the start of `b`: its tag, its contents and what
/// follows it. `None` where its length runs past `b` or is not one DER
/// writes (an indefinite length, more than four bytes of it).
pub(crate) fn der(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = b.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 {
            return None;
        }
        let (bytes, rest) = (rest.get(..n)?, rest.get(n..)?);
        (bytes.iter().fold(0usize, |l, &d| l << 8 | d as usize), rest)
    };
    Some((tag, rest.get(..len)?, rest.get(len..)?))
}

pub(crate) const SEQUENCE: u8 = 0x30;
pub(crate) const OID: u8 = 0x06;

/// The hash a binding is made with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hash {
    Sha256,
    Sha384,
    Sha512,
}

/// The hash a certificate's binding is made with, read off its
/// `signatureAlgorithm` (RFC 5280: `Certificate ::= SEQUENCE {
/// tbsCertificate, signatureAlgorithm, signatureValue }`).
pub(crate) fn hash_of(cert: &[u8]) -> Result<Hash, &'static str> {
    let malformed = "the certificate is not DER";
    let (SEQUENCE, outer, _) = der(cert).ok_or(malformed)? else {
        return Err(malformed);
    };
    let (SEQUENCE, _, rest) = der(outer).ok_or(malformed)? else {
        return Err(malformed);
    };
    let (SEQUENCE, alg, _) = der(rest).ok_or(malformed)? else {
        return Err(malformed);
    };
    let (OID, oid, params) = der(alg).ok_or(malformed)? else {
        return Err(malformed);
    };
    // 1.2.840.113549.1.1.x, PKCS #1's; 1.2.840.10045.4.x, ECDSA's.
    const RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01];
    const ECDSA: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04];
    let sha224 = "the certificate is signed with SHA-224, which this client has not";
    let none =
        "the certificate's signature uses no single hash (Ed25519, Ed448, or one not known here)";
    match oid {
        // md5WithRSAEncryption, sha1WithRSAEncryption, ecdsa-with-SHA1:
        // SHA-256, by the RFC's rule.
        [p @ .., 4 | 5] if p == RSA => Ok(Hash::Sha256),
        [p @ .., 0x01] if p == ECDSA => Ok(Hash::Sha256),
        [p @ .., 0x0b] if p == RSA => Ok(Hash::Sha256),
        [p @ .., 0x0c] if p == RSA => Ok(Hash::Sha384),
        [p @ .., 0x0d] if p == RSA => Ok(Hash::Sha512),
        [p @ .., 0x0e] if p == RSA => Err(sha224),
        [p @ .., 0x03, 0x02] if p == ECDSA => Ok(Hash::Sha256),
        [p @ .., 0x03, 0x03] if p == ECDSA => Ok(Hash::Sha384),
        [p @ .., 0x03, 0x04] if p == ECDSA => Ok(Hash::Sha512),
        [p @ .., 0x03, 0x01] if p == ECDSA => Err(sha224),
        // RSASSA-PSS: the hash its parameters name, `[0]`, SHA-1 (so
        // SHA-256) where they name none (RFC 4055).
        [p @ .., 0x0a] if p == RSA => {
            let (SEQUENCE, params, _) = der(params).ok_or(malformed)? else {
                return Err(malformed);
            };
            match der(params) {
                Some((0xa0, hash, _)) => {
                    let (SEQUENCE, hash, _) = der(hash).ok_or(malformed)? else {
                        return Err(malformed);
                    };
                    let (OID, hash, _) = der(hash).ok_or(malformed)? else {
                        return Err(malformed);
                    };
                    // 1.3.14.3.2.26, SHA-1; 2.16.840.1.101.3.4.2.x, the rest.
                    const NIST: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02];
                    match hash {
                        [0x2b, 0x0e, 0x03, 0x02, 0x1a] => Ok(Hash::Sha256),
                        [p @ .., 1] if p == NIST => Ok(Hash::Sha256),
                        [p @ .., 2] if p == NIST => Ok(Hash::Sha384),
                        [p @ .., 3] if p == NIST => Ok(Hash::Sha512),
                        [p @ .., 4] if p == NIST => Err(sha224),
                        _ => Err(none),
                    }
                }
                _ => Ok(Hash::Sha256),
            }
        }
        _ => Err(none),
    }
}
