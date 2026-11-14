# Project Wycheproof's vectors

The six JSON files here are [Project Wycheproof](https://github.com/C2SP/wycheproof)'s
test vectors, unchanged, from its `testvectors_v1/` at commit
`65ca494727a52944c204441c860b039e6786b99b`, main's head on 2026-11-14.
`../wycheproof.rs` runs every case of each against fennec-http's own
primitives (`src/crypto.rs`) and holds each file's count of cases:

| File | Primitive | Cases: valid, invalid, acceptable | Last changed in |
|---|---|---|---|
| `chacha20_poly1305_test.json` | `seal_in_place`, `open_in_place` | 325: 256, 69, 0 | `e0df04e0c033f2d25c5051dd06230336c7822358` |
| `rsa_signature_2048_sha256_test.json` | `RsaKey::verify_sha256` | 259: 9, 249, 1 | `e0df04e0c033f2d25c5051dd06230336c7822358` |
| `rsa_signature_3072_sha256_test.json` | `RsaKey::verify_sha256` | 259: 8, 250, 1 | `e0df04e0c033f2d25c5051dd06230336c7822358` |
| `rsa_signature_4096_sha256_test.json` | `RsaKey::verify_sha256` | 258: 7, 250, 1 | `e0df04e0c033f2d25c5051dd06230336c7822358` |
| `hmac_sha256_test.json` | `HmacKey`, `hmac_sha256`, `ct_eq` | 174: 66, 108, 0 | `e0df04e0c033f2d25c5051dd06230336c7822358` |
| `pbkdf2_hmacsha256_test.json` | `pbkdf2_sha256` | 60: 60, 0, 0 | `42ed2a86fbea37301c5d51d58ab9e245482e81f4` |

Their SHA-256, as `shasum -a 256 *.json` prints them in this directory,
which `grep -E '^[0-9a-f]{64}  ' README.md | shasum -a 256 -c` checks:

```
fe61d25f90e1bde4461d00eafe61049e5f29bd999f36b766df9cda90906ad53d  chacha20_poly1305_test.json
2d201cfa61d1bf95e6f5d07d96634b4a348b31e8eaa277ad7c8d09677b7a743f  hmac_sha256_test.json
1bf37af2cefe40c829ee9ecebb3505bb6424be8824bc97aa3e1c2076e860d192  pbkdf2_hmacsha256_test.json
94a917b01ff50fb874cfc05bf29b4af44868d944a6558201cf18380da93fb393  rsa_signature_2048_sha256_test.json
0f5f18cabfaad3e2792e82f7e9882f8999049b456714de924b8a5e202f61ca43  rsa_signature_3072_sha256_test.json
957aca128e30bd02c8982f8ca482d6521d16683f4bbc3f193cdfd4c8f04bc667  rsa_signature_4096_sha256_test.json
```

Each file's git blob is the one the commit's tree names, so the bytes are
upstream's to the bit. ES256's file is `../es256/`'s, pinned at a commit
of its own (`../es256/NOTICE`).

They are licensed under the Apache License, Version 2.0, a copy of which
is `LICENSE` beside them, as Wycheproof's repository holds it at that
commit (the same file as `../es256/LICENSE-wycheproof`).

## Updating them

A file here changes on purpose, in a change of its own, never in passing:
take it from `testvectors_v1/` at a newer commit, read its `notes` for a
kind of case it did not hold, and run
`cargo test -p fennec-http --test all wycheproof::`. Each test fails
until its counts of valid, invalid and acceptable cases are written again,
and a new acceptable kind until an answer is chosen for it, with why; a
case fennec answers otherwise is a finding, the primitive's to fix. Then
write the commit, the counts and the digests here.
