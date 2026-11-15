//! Project Wycheproof's vectors for the primitives fennec-http writes by
//! hand (`crypto.rs`), every case of six files: ChaCha20-Poly1305, which
//! seals an archive; RS256's verification, RSASSA-PKCS1-v1_5 with SHA-256,
//! at 2048, 3072 and 4096 bits; HMAC-SHA-256; and PBKDF2-HMAC-SHA-256.
//! ES256's file is `es256.rs`'s. The files are C2SP/wycheproof's,
//! committed unchanged (`wycheproof/README.md` names the commit and each
//! file's SHA-256).
//!
//! A `valid` case must give the file's bytes and take them back, and an
//! `invalid` one must be refused; an `acceptable` one -- legal by some
//! reading, weak or legacy -- is held to the answer chosen for its kind
//! here, with why. Every case a file's primitive answers otherwise is
//! named at once, never only the first; and each file's cases are
//! counted, all of them against its `numberOfTests` and each result's
//! against what the file held when it was committed, so no case is skipped
//! unseen and a file changed is seen too.

use fennec_core::json::parse_json;
use fennec_core::prelude::*;
use fennec_http::crypto::{
    b64url_decode, ct_eq, hmac_sha256, open_in_place, pbkdf2_sha256, seal_in_place, HmacKey, RsaKey,
};

type Members = [(String, Value)];

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "hex of an odd length: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn member<'a>(o: &'a Members, name: &str) -> &'a Value {
    &o.iter()
        .find(|(k, _)| k == name)
        .unwrap_or_else(|| panic!("no `{name}`"))
        .1
}

fn text<'a>(o: &'a Members, name: &str) -> &'a str {
    member(o, name).as_text().unwrap()
}

fn bytes(o: &Members, name: &str) -> Vec<u8> {
    unhex(text(o, name))
}

fn int(o: &Members, name: &str) -> i64 {
    match member(o, name) {
        Value::Int(n) => *n,
        v => panic!("`{name}` is {v:?}"),
    }
}

fn object(v: &Value) -> &Members {
    match v {
        Value::Object(o) => o,
        _ => panic!("not an object"),
    }
}

fn list(v: &Value) -> &[Value] {
    match v {
        Value::List(l) => l,
        _ => panic!("not a list"),
    }
}

fn flags(case: &Members) -> Vec<&str> {
    list(member(case, "flags"))
        .iter()
        .map(|f| f.as_text().unwrap())
        .collect()
}

/// A file's groups, each with its cases, the file held to the algorithm
/// and every group to the type asked.
fn groups<'a>(doc: &'a Value, algorithm: &str, kind: &str) -> Vec<(&'a Members, Vec<&'a Members>)> {
    let doc = object(doc);
    assert_eq!(text(doc, "algorithm"), algorithm);
    list(member(doc, "testGroups"))
        .iter()
        .map(|g| {
            let g = object(g);
            assert_eq!(text(g, "type"), kind);
            (g, list(member(g, "tests")).iter().map(object).collect())
        })
        .collect()
}

/// What a file's cases came to.
#[derive(Default)]
struct Ran {
    /// The valid, invalid and acceptable cases run.
    counts: [i64; 3],
    /// Each answer otherwise than the file says, by its case: a case may
    /// give more than one, a valid AEAD case opened and sealed both.
    otherwise: Vec<String>,
}

impl Ran {
    /// Holds `took` -- whether fennec took the case -- to the case's
    /// result: a valid case taken, an invalid one refused, an acceptable
    /// one as `chosen` says, `None` where its file is to hold none.
    fn hold(&mut self, case: &Members, took: bool, chosen: Option<bool>) {
        let (at, want) = match text(case, "result") {
            "valid" => (0, Some(true)),
            "invalid" => (1, Some(false)),
            "acceptable" => (2, chosen),
            other => panic!("case {}: a result `{other}`", int(case, "tcId")),
        };
        self.counts[at] += 1;
        match want {
            None => self.differs(case, "acceptable, and no answer chosen for its kind"),
            Some(want) if want != took => {
                self.differs(case, if took { "taken" } else { "refused" })
            }
            Some(_) => {}
        }
    }

    fn differs(&mut self, case: &Members, how: &str) {
        self.otherwise.push(format!(
            "case {} ({}, {:?}, {:?}): {how}",
            int(case, "tcId"),
            text(case, "result"),
            text(case, "comment"),
            flags(case),
        ));
    }

    /// Every case answered as its file says, every case run, and as many
    /// valid, invalid and acceptable ones as `counts` says.
    fn end(self, doc: &Value, counts: [i64; 3]) {
        assert!(
            self.otherwise.is_empty(),
            "{} answer(s) otherwise than the file:\n{}",
            self.otherwise.len(),
            self.otherwise.join("\n")
        );
        let total = int(object(doc), "numberOfTests");
        assert_eq!(self.counts.iter().sum::<i64>(), total, "every case ran");
        assert_eq!(self.counts, counts, "valid, invalid and acceptable");
    }
}

/// ChaCha20-Poly1305 (`seal_in_place`, `open_in_place`), as `seal.rs`
/// seals a backup's and an archive's frames: a valid case sealed gives
/// the file's ciphertext and tag, and opened gives its message back; an
/// invalid one is refused, its bytes left as they came. The valid cases
/// hold Poly1305's edge cases -- a one-time key's limbs, sums that carry,
/// the final addition's overflow -- where limb arithmetic goes wrong, and
/// the invalid ones are a tag with bits changed (60) or a nonce not of 12
/// bytes (9). Such a nonce cannot reach the primitives, which take
/// `&[u8; 12]` as `seal.rs` reads a frame's 12: it is refused by its type.
#[test]
fn aead_wycheproof() {
    let doc = parse_json(include_str!("wycheproof/chacha20_poly1305_test.json")).unwrap();
    let mut ran = Ran::default();
    let mut nonces_refused = 0;
    for (group, cases) in groups(&doc, "CHACHA20-POLY1305", "AeadTest") {
        assert_eq!((int(group, "keySize"), int(group, "tagSize")), (256, 128));
        for case in cases {
            let key: [u8; 32] = bytes(case, "key").try_into().unwrap();
            let (aad, msg) = (bytes(case, "aad"), bytes(case, "msg"));
            let (ct, tag) = (bytes(case, "ct"), bytes(case, "tag"));
            let Ok(nonce) = <[u8; 12]>::try_from(&bytes(case, "iv")[..]) else {
                nonces_refused += 1;
                ran.hold(case, false, None);
                continue;
            };
            let mut opened = ct.clone();
            let took = open_in_place(&key, &nonce, &aad, &mut opened, &tag);
            ran.hold(case, took, None);
            if took && opened != msg {
                ran.differs(case, "opened into other bytes than its message");
            }
            if !took && opened != ct {
                ran.differs(case, "refused, its bytes changed");
            }
            if text(case, "result") == "valid" {
                let mut sealed = msg.clone();
                let sealed_tag = seal_in_place(&key, &nonce, &aad, &mut sealed);
                if sealed != ct || sealed_tag[..] != tag[..] {
                    ran.differs(case, "sealed into another ciphertext or tag");
                }
            }
        }
    }
    assert_eq!(nonces_refused, 9);
    ran.end(&doc, [256, 69, 0]);
}

/// RS256's verification (`RsaKey::verify_sha256`) over every case of a
/// file, each group's key read from its JWK as a JWKS file gives it to
/// `--jwt-keys`. A signature is taken only where it is the one encoding
/// RFC 8017 makes of the message (8.2.2: the message encoded again, and
/// compared): `verify_sha256` parses no padding, so the forged paddings,
/// BER encodings, lengths, hashes and DigestInfos the invalid cases hold --
/// a parser's leniency, which an e = 3 key turns into anyone's forgery
/// (Bleichenbacher's, 2006) -- have no parser to get past, and the e = 3
/// keys' valid cases (two keys at 2048 bits, one at 3072) verify. A
/// signature of another length than the modulus, or not below it, is
/// refused before it is raised to `e`.
fn rsa_pkcs1_sha256(json: &str, bits: usize, counts: [i64; 3]) {
    let doc = parse_json(json).unwrap();
    let mut ran = Ran::default();
    for (group, cases) in groups(&doc, "RSASSA-PKCS1-v1_5", "RsassaPkcs1Verify") {
        assert_eq!(text(group, "sha"), "SHA-256");
        let jwk = object(member(group, "keyJwk"));
        assert_eq!((text(jwk, "kty"), text(jwk, "alg")), ("RSA", "RS256"));
        let n = b64url_decode(text(jwk, "n")).unwrap();
        let e = b64url_decode(text(jwk, "e")).unwrap();
        assert_eq!(n.len() * 8, bits, "a key of {bits} bits");
        let key = RsaKey::new(&n, &e).expect("a key of 2048 bits up and an odd exponent from 3");
        for case in cases {
            let took = key.verify_sha256(&bytes(case, "msg"), &bytes(case, "sig"));
            // `MissingNull`, the one kind acceptable here: a DigestInfo
            // whose algorithm carries no NULL parameters. RFC 8017's
            // EMSA-PKCS1-v1_5 writes the NULL for SHA-256 (9.2, note 1, and
            // appendix B.1's exception for it), so the encoding without it
            // is another one: refused. Wycheproof calls such signatures
            // invalid, taken by some libraries for old signers' sake; an
            // RS256 token is an identity provider's of today.
            let chosen = (flags(case) == ["MissingNull"]).then_some(false);
            ran.hold(case, took, chosen);
        }
    }
    ran.end(&doc, counts);
}

#[test]
fn rsa_pkcs1_sha256_wycheproof_2048() {
    rsa_pkcs1_sha256(
        include_str!("wycheproof/rsa_signature_2048_sha256_test.json"),
        2048,
        [9, 249, 1],
    );
}

#[test]
fn rsa_pkcs1_sha256_wycheproof_3072() {
    rsa_pkcs1_sha256(
        include_str!("wycheproof/rsa_signature_3072_sha256_test.json"),
        3072,
        [8, 250, 1],
    );
}

#[test]
fn rsa_pkcs1_sha256_wycheproof_4096() {
    rsa_pkcs1_sha256(
        include_str!("wycheproof/rsa_signature_4096_sha256_test.json"),
        4096,
        [7, 250, 1],
    );
}

/// HMAC-SHA-256 as fennecdb checks a MAC: made under its key (`HmacKey`,
/// as a hook's signatures and the global entry's marks are, and
/// `hmac_sha256`, an HS256 token's, alike), cut to the group's tag -- 128
/// bits is the router's and the entry's marks' -- and compared by `ct_eq`.
/// Keys of 16, 32 and 65 bytes, the last hashed first, messages of 0 to
/// 255 bytes; every invalid case is a tag with bits changed.
#[test]
fn hmac_sha256_wycheproof() {
    let doc = parse_json(include_str!("wycheproof/hmac_sha256_test.json")).unwrap();
    let mut ran = Ran::default();
    for (group, cases) in groups(&doc, "HMACSHA256", "MacTest") {
        let cut = int(group, "tagSize") as usize / 8;
        for case in cases {
            let (key, msg, tag) = (bytes(case, "key"), bytes(case, "msg"), bytes(case, "tag"));
            assert_eq!(key.len() as i64 * 8, int(group, "keySize"));
            let mac = HmacKey::new(&key).mac(&[&msg]);
            if mac != hmac_sha256(&key, &msg) {
                ran.differs(case, "HmacKey's MAC is not hmac_sha256's");
            }
            ran.hold(case, ct_eq(&mac[..cut], &tag), None);
        }
    }
    ran.end(&doc, [66, 108, 0]);
}

/// PBKDF2-HMAC-SHA-256 (`pbkdf2_sha256`, SCRAM's `SaltedPassword`), which
/// makes a derived key's first 32-byte block and nothing past it: a case
/// asking 16 or 32 bytes is held to all of them, one asking 42, 64 or 65
/// to its first 32. The bytes after are blocks 2 and 3, made with INT(2)
/// and INT(3) where block 1 has INT(1), which nothing in fennecdb asks
/// for. Every case is valid: passwords of 0 to 257 bytes, UTF-8 or not
/// (65 and more hashed first), salts of 4 to 16, at 1, 4 096 and 80 000
/// rounds -- RFC 7914's two among them.
#[test]
fn pbkdf2_sha256_wycheproof() {
    let doc = parse_json(include_str!("wycheproof/pbkdf2_hmacsha256_test.json")).unwrap();
    let mut ran = Ran::default();
    let (mut whole, mut first_block) = (0, 0);
    for (_, cases) in groups(&doc, "PBKDF2-HMACSHA256", "PbkdfTest") {
        for case in cases {
            let (password, salt, dk) = (
                bytes(case, "password"),
                bytes(case, "salt"),
                bytes(case, "dk"),
            );
            assert_eq!(dk.len() as i64, int(case, "dkLen"));
            let rounds = u32::try_from(int(case, "iterationCount")).unwrap();
            let got = pbkdf2_sha256(&password, &salt, rounds);
            let n = dk.len().min(got.len());
            if dk.len() > got.len() {
                first_block += 1;
            } else {
                whole += 1;
            }
            ran.hold(case, got[..n] == dk[..n], None);
        }
    }
    assert_eq!((whole, first_block), (26, 34));
    ran.end(&doc, [60, 0, 0]);
}
