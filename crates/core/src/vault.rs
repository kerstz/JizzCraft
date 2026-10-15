//! Vault cryptography: header file, key hierarchy, thumbnail cipher. Pure
//! functions on bytes; no file or database access (see `docs/vault.md`).
//!
//! Only RustCrypto primitives (Argon2id, XChaCha20-Poly1305) and BLAKE3 are
//! used. Secrets live in non-`Clone` newtypes that zero themselves on drop and
//! print as `<redacted>`. Errors carry fixed messages, never input data.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use std::fmt;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

pub const HEADER_LEN: usize = 111;
pub const FORMAT_VERSION: u16 = 1;
const MAGIC: &[u8; 8] = b"JZVAULT\0";
const KDF_ARGON2ID: u8 = 1;
/// Bytes covered by the AEAD as associated data (everything before the nonce).
const AAD_LEN: usize = 39;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const DEK_LEN: usize = 32;
const TAG_LEN: usize = 16;

const CTX_DB: &str = "jizzcraft 2026 vault db v1";
const CTX_THUMB_AEAD: &str = "jizzcraft 2026 vault thumb aead v1";
const CTX_THUMB_NAME: &str = "jizzcraft 2026 vault thumb name v1";

const THUMB_MAGIC: &[u8; 4] = b"JZT1";

/// Accepted ranges when a header is read (a hostile file must not be able to
/// ask for gigabytes of memory or hours of CPU).
pub const M_COST_RANGE: (u32, u32) = (19_456, 1_048_576);
pub const T_COST_RANGE: (u32, u32) = (2, 10);
pub const P_COST_RANGE: (u32, u32) = (1, 8);

/// Name of the header file that sits next to the database (`library.vault`).
pub fn header_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("vault")
}

/// Everything that can go wrong. Messages are fixed on purpose: no passphrase,
/// key, path or SQLCipher text ever reaches a log or a toast through here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultError {
    /// The authentication of the wrapped key failed: wrong passphrase.
    WrongPassphrase,
    /// The key is right but the database refuses it, or is damaged.
    DbCorrupt,
    /// The database file is not encrypted.
    NotEncrypted,
    /// Header is the wrong size, has a bad magic number or unknown field.
    HeaderCorrupt,
    /// The header file is missing.
    HeaderMissing,
    /// Header written by a newer format version.
    UnsupportedVersion,
    /// KDF parameters outside the accepted bounds.
    ParamsOutOfBounds,
    /// A passphrase must not be empty.
    EmptyPassphrase,
    /// A cache file failed authentication or is malformed.
    ThumbCorrupt,
    /// The key derivation could not run (out of memory).
    Kdf,
    /// The system random generator failed.
    Random,
    /// Another connection or process holds the library; nothing was changed.
    Busy,
    /// Conversion refused: the library is already encrypted.
    AlreadyEncrypted,
    /// Conversion failed before its commit point; the library is unchanged.
    ConvertFailed,
    /// The converted copy did not verify; the library is unchanged.
    VerifyFailed,
    /// A valid vault header exists next to a clear or missing library and no
    /// interrupted disable explains it: nothing was deleted, nothing is created.
    HeaderOrphan,
    /// The caller cancelled before the commit point; nothing was changed.
    Cancelled,
    /// A test fail point stopped the conversion (never returned otherwise).
    Interrupted,
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            VaultError::WrongPassphrase => "wrong passphrase",
            VaultError::DbCorrupt => "the library cannot be opened with this key (damaged)",
            VaultError::NotEncrypted => "the library is not encrypted",
            VaultError::HeaderCorrupt => "vault header is damaged",
            VaultError::HeaderMissing => "vault header is missing",
            VaultError::UnsupportedVersion => "vault format is newer than this build",
            VaultError::ParamsOutOfBounds => "vault key derivation parameters are out of bounds",
            VaultError::EmptyPassphrase => "the passphrase is empty",
            VaultError::ThumbCorrupt => "encrypted cache file is damaged",
            VaultError::Kdf => "key derivation failed",
            VaultError::Random => "random generator failed",
            VaultError::Busy => "the library is in use by another process",
            VaultError::AlreadyEncrypted => "the library is already encrypted",
            VaultError::ConvertFailed => "conversion failed, the library was not changed",
            VaultError::VerifyFailed => "the converted library did not verify, nothing was changed",
            VaultError::HeaderOrphan => {
                "a vault header exists but the library is not encrypted, nothing was changed"
            }
            VaultError::Interrupted => "conversion interrupted",
            VaultError::Cancelled => "conversion cancelled, nothing was changed",
        })
    }
}

impl std::error::Error for VaultError {}

/// True when `library.vault` or `library.vault.new` next to `db_path` is a
/// well-formed header.
pub fn valid_header_exists(db_path: &Path) -> bool {
    let h = header_path(db_path);
    let mut n = h.as_os_str().to_os_string();
    n.push(".new");
    [h, PathBuf::from(n)].iter().any(|p| {
        std::fs::read(p)
            .ok()
            .is_some_and(|b| validate_header(&b).is_ok())
    })
}

macro_rules! secret_type {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Zeroize, ZeroizeOnDrop)]
        pub struct $name([u8; 32]);
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("<redacted>")
            }
        }
    };
}

secret_type!(
    /// The random data key (DEK) wrapped in the header. Not `Clone`: share it
    /// with an `Arc`.
    VaultKey
);
secret_type!(
    /// Raw 32-byte SQLCipher key derived from the DEK.
    DbKey
);

/// Keys for the encrypted thumbnail store.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct ThumbKeys {
    aead: [u8; 32],
    name: [u8; 32],
}

impl fmt::Debug for ThumbKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl ThumbKeys {
    /// The two raw sub-keys (file encryption, file naming), so an integration
    /// test can look for them on disk. Never call it in product code.
    #[doc(hidden)]
    pub fn bytes_for_tests(&self) -> [Zeroizing<[u8; 32]>; 2] {
        [Zeroizing::new(self.aead), Zeroizing::new(self.name)]
    }
}

impl VaultKey {
    pub fn db_key(&self) -> DbKey {
        DbKey(blake3::derive_key(CTX_DB, &self.0))
    }

    pub fn thumb_keys(&self) -> ThumbKeys {
        ThumbKeys {
            aead: blake3::derive_key(CTX_THUMB_AEAD, &self.0),
            name: blake3::derive_key(CTX_THUMB_NAME, &self.0),
        }
    }

    /// A fresh random key.
    pub fn generate() -> Result<VaultKey, VaultError> {
        let mut k = [0u8; 32];
        getrandom::fill(&mut k).map_err(|_| VaultError::Random)?;
        Ok(VaultKey(k))
    }

    /// The raw data key, so an integration test can look for it on disk.
    /// Never call it in product code: the key must not leave this module.
    #[doc(hidden)]
    pub fn bytes_for_tests(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0)
    }

    /// A key from fixed bytes, for tests of other modules. Never use it for
    /// real data: real keys come from `create` / `unlock`.
    #[doc(hidden)]
    pub fn from_bytes_for_tests(b: [u8; 32]) -> VaultKey {
        VaultKey(b)
    }
}

impl DbKey {
    /// Value of `PRAGMA key`: `"x'<64 hex>'"` (raw key, no SQLCipher KDF).
    /// The returned string zeroes itself on drop.
    pub fn pragma_value(&self) -> Zeroizing<String> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut s = Zeroizing::new(String::with_capacity(68));
        s.push_str("\"x'");
        for b in self.0 {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 15) as usize] as char);
        }
        s.push_str("'\"");
        s
    }

    /// The raw database key, so an integration test can look for it on disk.
    /// Never call it in product code.
    #[doc(hidden)]
    pub fn bytes_for_tests(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0)
    }

    #[cfg(test)]
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Argon2id cost parameters stored in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    /// Memory in KiB.
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        KdfParams {
            m_cost: 65_536,
            t_cost: 3,
            p_cost: 1,
        }
    }
}

impl KdfParams {
    pub fn check(&self) -> Result<(), VaultError> {
        let ok = |v: u32, r: (u32, u32)| (r.0..=r.1).contains(&v);
        if ok(self.m_cost, M_COST_RANGE)
            && ok(self.t_cost, T_COST_RANGE)
            && ok(self.p_cost, P_COST_RANGE)
        {
            Ok(())
        } else {
            Err(VaultError::ParamsOutOfBounds)
        }
    }
}

/// NFC-normalised, untrimmed passphrase bytes.
fn normalise(passphrase: &str) -> Zeroizing<Vec<u8>> {
    let s: Zeroizing<String> = Zeroizing::new(passphrase.nfc().collect());
    Zeroizing::new(s.as_bytes().to_vec())
}

fn derive_kek(
    passphrase: &str,
    salt: &[u8],
    p: &KdfParams,
) -> Result<Zeroizing<[u8; 32]>, VaultError> {
    let params = Params::new(p.m_cost, p.t_cost, p.p_cost, Some(32))
        .map_err(|_| VaultError::ParamsOutOfBounds)?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(&normalise(passphrase), salt, out.as_mut())
        .map_err(|_| VaultError::Kdf)?;
    Ok(out)
}

/// A header split into its fields; `bytes` is kept for the AAD and the tag.
struct Parsed<'a> {
    params: KdfParams,
    salt: &'a [u8],
    nonce: &'a [u8],
    wrapped: &'a [u8],
    aad: &'a [u8],
}

fn parse(header: &[u8]) -> Result<Parsed<'_>, VaultError> {
    if header.len() != HEADER_LEN || &header[..8] != MAGIC {
        return Err(VaultError::HeaderCorrupt);
    }
    let u16_at = |o: usize| u16::from_le_bytes([header[o], header[o + 1]]);
    let u32_at =
        |o: usize| u32::from_le_bytes([header[o], header[o + 1], header[o + 2], header[o + 3]]);
    let version = u16_at(8);
    if version > FORMAT_VERSION {
        return Err(VaultError::UnsupportedVersion);
    }
    if version != FORMAT_VERSION || header[10] != KDF_ARGON2ID {
        return Err(VaultError::HeaderCorrupt);
    }
    let params = KdfParams {
        m_cost: u32_at(11),
        t_cost: u32_at(15),
        p_cost: u32_at(19),
    };
    params.check()?;
    Ok(Parsed {
        params,
        salt: &header[23..23 + SALT_LEN],
        nonce: &header[AAD_LEN..AAD_LEN + NONCE_LEN],
        wrapped: &header[63..HEADER_LEN],
        aad: &header[..AAD_LEN],
    })
}

fn open_envelope(p: &Parsed<'_>, kek: &[u8; 32]) -> Result<VaultKey, VaultError> {
    let cipher = XChaCha20Poly1305::new(&Key::from(*kek));
    let mut plain = cipher
        .decrypt(
            &XNonce::try_from(p.nonce).map_err(|_| VaultError::HeaderCorrupt)?,
            Payload {
                msg: p.wrapped,
                aad: p.aad,
            },
        )
        .map_err(|_| VaultError::WrongPassphrase)?;
    let mut dek = [0u8; DEK_LEN];
    let ok = plain.len() == DEK_LEN;
    if ok {
        dek.copy_from_slice(&plain);
    }
    plain.zeroize();
    if ok {
        Ok(VaultKey(dek))
    } else {
        dek.zeroize();
        Err(VaultError::HeaderCorrupt)
    }
}

/// Build a header with every random input supplied by the caller.
fn wrap_with(
    dek: &VaultKey,
    passphrase: &str,
    params: KdfParams,
    salt: [u8; SALT_LEN],
    nonce: [u8; NONCE_LEN],
) -> Result<[u8; HEADER_LEN], VaultError> {
    params.check()?;
    if passphrase.is_empty() {
        return Err(VaultError::EmptyPassphrase);
    }
    let kek = derive_kek(passphrase, &salt, &params)?;
    let mut h = [0u8; HEADER_LEN];
    h[..8].copy_from_slice(MAGIC);
    h[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    h[10] = KDF_ARGON2ID;
    h[11..15].copy_from_slice(&params.m_cost.to_le_bytes());
    h[15..19].copy_from_slice(&params.t_cost.to_le_bytes());
    h[19..23].copy_from_slice(&params.p_cost.to_le_bytes());
    h[23..39].copy_from_slice(&salt);
    h[39..63].copy_from_slice(&nonce);
    let cipher = XChaCha20Poly1305::new(&Key::from(*kek));
    let ct = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: &dek.0,
                aad: &h[..AAD_LEN],
            },
        )
        .map_err(|_| VaultError::Kdf)?;
    debug_assert_eq!(ct.len(), DEK_LEN + TAG_LEN);
    h[63..].copy_from_slice(&ct);
    Ok(h)
}

/// Wrap an existing key under `passphrase` with a fresh salt and nonce.
/// Changing the passphrase is `unlock` then `wrap`: only the envelope changes.
pub fn wrap(
    dek: &VaultKey,
    passphrase: &str,
    params: KdfParams,
) -> Result<[u8; HEADER_LEN], VaultError> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut salt).map_err(|_| VaultError::Random)?;
    getrandom::fill(&mut nonce).map_err(|_| VaultError::Random)?;
    wrap_with(dek, passphrase, params, salt, nonce)
}

/// New vault: random key, wrapped under `passphrase`. Slow (Argon2id): never
/// call it on a UI thread.
pub fn create(
    passphrase: &str,
    params: KdfParams,
) -> Result<(VaultKey, [u8; HEADER_LEN]), VaultError> {
    let dek = VaultKey::generate()?;
    let header = wrap(&dek, passphrase, params)?;
    Ok((dek, header))
}

/// Check the structure and bounds of a header without any passphrase.
pub fn validate_header(header: &[u8]) -> Result<KdfParams, VaultError> {
    parse(header).map(|p| p.params)
}

/// Unwrap the key. Slow (Argon2id): never call it on a UI thread.
pub fn unlock(header: &[u8], passphrase: &str) -> Result<VaultKey, VaultError> {
    let p = parse(header)?;
    let kek = derive_kek(passphrase, p.salt, &p.params)?;
    open_envelope(&p, &kek)
}

/// Encrypts and names thumbnail cache files.
pub struct ThumbCipher {
    cipher: XChaCha20Poly1305,
    name_key: Zeroizing<[u8; 32]>,
}

impl fmt::Debug for ThumbCipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl ThumbCipher {
    pub fn new(keys: &ThumbKeys) -> ThumbCipher {
        ThumbCipher {
            cipher: XChaCha20Poly1305::new(&Key::from(keys.aead)),
            name_key: Zeroizing::new(keys.name),
        }
    }

    /// 32 hex characters, a keyed hash of `path || size || mtime`: neither the
    /// path nor the content can be read from the name.
    pub fn file_name(&self, path: &[u8], size: u64, mtime: i64) -> String {
        let mut h = blake3::Hasher::new_keyed(&self.name_key);
        h.update(path);
        h.update(&size.to_le_bytes());
        h.update(&mtime.to_le_bytes());
        let hash = h.finalize();
        hash.to_hex()[..32].to_string()
    }

    fn aad(name: &str) -> Vec<u8> {
        let mut a = Vec::with_capacity(4 + name.len());
        a.extend_from_slice(THUMB_MAGIC);
        a.extend_from_slice(name.as_bytes());
        a
    }

    fn seal_with(&self, name: &str, plain: &[u8], nonce: [u8; NONCE_LEN]) -> Vec<u8> {
        let ct = self
            .cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plain,
                    aad: &Self::aad(name),
                },
            )
            .expect("XChaCha20-Poly1305 encryption cannot fail for in-memory input");
        let mut out = Vec::with_capacity(4 + NONCE_LEN + ct.len());
        out.extend_from_slice(THUMB_MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    /// `"JZT1" || nonce || ciphertext+tag`, bound to `name`.
    pub fn seal(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>, VaultError> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| VaultError::Random)?;
        Ok(self.seal_with(name, plain, nonce))
    }

    /// A failure means the file is absent as far as the cache is concerned:
    /// the caller deletes it and regenerates.
    pub fn open(&self, name: &str, sealed: &[u8]) -> Result<Vec<u8>, VaultError> {
        if sealed.len() < 4 + NONCE_LEN + TAG_LEN || &sealed[..4] != THUMB_MAGIC {
            return Err(VaultError::ThumbCorrupt);
        }
        self.cipher
            .decrypt(
                &XNonce::try_from(&sealed[4..4 + NONCE_LEN])
                    .map_err(|_| VaultError::ThumbCorrupt)?,
                Payload {
                    msg: &sealed[4 + NONCE_LEN..],
                    aad: &Self::aad(name),
                },
            )
            .map_err(|_| VaultError::ThumbCorrupt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimum accepted cost: keeps the tamper test fast.
    const FAST: KdfParams = KdfParams {
        m_cost: 19_456,
        t_cost: 2,
        p_cost: 1,
    };

    fn dek() -> VaultKey {
        VaultKey::from_bytes_for_tests(std::array::from_fn(|i| i as u8))
    }

    fn fixed_header(pass: &str) -> [u8; HEADER_LEN] {
        let salt = std::array::from_fn(|i| 0xA0 + i as u8);
        let nonce = std::array::from_fn(|i| 0x40 + i as u8);
        wrap_with(&dek(), pass, FAST, salt, nonce).unwrap()
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    // Frozen reference vector. If one of these changes, existing vaults can no
    // longer be opened: that is a format break, not a test to update.
    const VECTOR_HEADER: &str = "4a5a5641554c5400010001004c00000200000001000000a0a1a2a3a4a5a6a7a8a9aaabacadaeaf404142434445464748494a4b4c4d4e4f5051525354555657b81235c2c44f2f4d7d8e346f758b74e2fa1466a773fc9c8bc66d6bd90c48ac89874c269d147bbe91466befc70f397bd5";
    const VECTOR_DB_KEY: &str = "06baec3bf1c3d934a1d538b9c3d63ee819505c342b6dfca230d541de8ec80594";
    const VECTOR_THUMB_NAME: &str = "ecd6f9553341c84eaa523d6bea60f645";
    const VECTOR_THUMB_FILE: &str = "4a5a5431707172737475767778797a7b7c7d7e7f8081828384858687370c4e700bdc19983480f5df964fe87d2e6585bed3403c97013b";

    #[test]
    fn reference_vector_is_frozen() {
        let h = fixed_header("correct horse battery staple");
        assert_eq!(hex(&h), VECTOR_HEADER);
        let k = unlock(&h, "correct horse battery staple").unwrap();
        assert_eq!(hex(k.db_key().as_bytes()), VECTOR_DB_KEY);
        let c = ThumbCipher::new(&k.thumb_keys());
        let name = c.file_name(b"/lib/a.png", 1234, 5678);
        assert_eq!(name, VECTOR_THUMB_NAME);
        let nonce = std::array::from_fn(|i| 0x70 + i as u8);
        let f = c.seal_with(&name, b"jpeg bytes", nonce);
        assert_eq!(hex(&f), VECTOR_THUMB_FILE);
        assert_eq!(c.open(&name, &f).unwrap(), b"jpeg bytes");
    }

    #[test]
    fn header_layout_matches_the_documentation() {
        let h = fixed_header("x");
        assert_eq!(h.len(), 111);
        assert_eq!(&h[..8], b"JZVAULT\0");
        assert_eq!(&h[8..10], &[1, 0]);
        assert_eq!(h[10], 1);
        assert_eq!(u32::from_le_bytes(h[11..15].try_into().unwrap()), 19_456);
        assert_eq!(u32::from_le_bytes(h[15..19].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(h[19..23].try_into().unwrap()), 1);
        assert_eq!(h[23], 0xA0);
        assert_eq!(h[39], 0x40);
        assert_eq!(h.len() - 63, 48);
    }

    #[test]
    fn create_unlock_roundtrip_and_rewrap() {
        let (k, h) = create("a passphrase", FAST).unwrap();
        let k2 = unlock(&h, "a passphrase").unwrap();
        assert_eq!(k.db_key().as_bytes(), k2.db_key().as_bytes());
        // Fresh salt and nonce each time.
        let h2 = wrap(&k, "a passphrase", FAST).unwrap();
        assert_ne!(h[..], h2[..]);
        // Changing the passphrase keeps the key.
        let h3 = wrap(&k2, "another one", FAST).unwrap();
        assert_eq!(
            unlock(&h3, "another one").unwrap().db_key().as_bytes(),
            k.db_key().as_bytes()
        );
        assert_eq!(
            unlock(&h3, "a passphrase").unwrap_err(),
            VaultError::WrongPassphrase
        );
    }

    #[test]
    fn wrong_passphrase_and_no_trim() {
        let h = fixed_header("secret");
        assert_eq!(
            unlock(&h, "Secret").unwrap_err(),
            VaultError::WrongPassphrase
        );
        assert_eq!(
            unlock(&h, "secret ").unwrap_err(),
            VaultError::WrongPassphrase
        );
        assert_eq!(unlock(&h, "").unwrap_err(), VaultError::WrongPassphrase);
        assert_eq!(
            wrap(&dek(), "", FAST).unwrap_err(),
            VaultError::EmptyPassphrase
        );
    }

    #[test]
    fn passphrase_is_nfc_normalised() {
        // "é" precomposed vs "e" + combining acute.
        let h = fixed_header("caf\u{e9}");
        assert!(unlock(&h, "cafe\u{301}").is_ok());
    }

    #[test]
    fn every_header_byte_is_authenticated() {
        let pass = "tamper me";
        let h = fixed_header(pass);
        let p = parse(&h).unwrap();
        let kek = derive_kek(pass, p.salt, &p.params).unwrap();
        assert!(open_envelope(&p, &kek).is_ok());
        for i in 0..HEADER_LEN {
            let mut t = h;
            t[i] ^= 0x01;
            let r = if i < AAD_LEN {
                // Parameters or salt changed: the full path, KDF included.
                unlock(&t, pass)
            } else {
                // The key encryption key is unchanged: only the AEAD matters.
                match parse(&t) {
                    Ok(p) => open_envelope(&p, &kek),
                    Err(e) => Err(e),
                }
            };
            assert!(r.is_err(), "flipping byte {i} must fail");
        }
    }

    #[test]
    fn structural_errors() {
        let h = fixed_header("p");
        assert_eq!(
            unlock(&h[..110], "p").unwrap_err(),
            VaultError::HeaderCorrupt
        );
        let mut long = h.to_vec();
        long.push(0);
        assert_eq!(unlock(&long, "p").unwrap_err(), VaultError::HeaderCorrupt);
        assert_eq!(unlock(&[], "p").unwrap_err(), VaultError::HeaderCorrupt);
        let mut t = h;
        t[0] ^= 1;
        assert_eq!(unlock(&t, "p").unwrap_err(), VaultError::HeaderCorrupt);
        let mut t = h;
        t[8] = 2;
        assert_eq!(unlock(&t, "p").unwrap_err(), VaultError::UnsupportedVersion);
        let mut t = h;
        t[10] = 2;
        assert_eq!(unlock(&t, "p").unwrap_err(), VaultError::HeaderCorrupt);
    }

    #[test]
    fn parameter_bounds_on_read_and_write() {
        let h = fixed_header("p");
        let with = |m: u32, t: u32, p: u32| {
            let mut x = h;
            x[11..15].copy_from_slice(&m.to_le_bytes());
            x[15..19].copy_from_slice(&t.to_le_bytes());
            x[19..23].copy_from_slice(&p.to_le_bytes());
            validate_header(&x)
        };
        assert!(with(19_456, 2, 1).is_ok());
        assert!(with(1_048_576, 10, 8).is_ok());
        for (m, t, p) in [
            (19_455, 2, 1),
            (1_048_577, 2, 1),
            (u32::MAX, 2, 1),
            (19_456, 1, 1),
            (19_456, 11, 1),
            (19_456, 2, 0),
            (19_456, 2, 9),
        ] {
            assert_eq!(
                with(m, t, p).unwrap_err(),
                VaultError::ParamsOutOfBounds,
                "{m} {t} {p}"
            );
        }
        let bad = KdfParams {
            m_cost: 1024,
            ..FAST
        };
        assert_eq!(
            wrap(&dek(), "p", bad).unwrap_err(),
            VaultError::ParamsOutOfBounds
        );
        assert!(KdfParams::default().check().is_ok());
    }

    #[test]
    fn secrets_are_redacted_in_debug() {
        let k = dek();
        assert_eq!(format!("{k:?}"), "<redacted>");
        assert_eq!(format!("{:?}", k.db_key()), "<redacted>");
        let tk = k.thumb_keys();
        assert_eq!(format!("{tk:?}"), "<redacted>");
        assert_eq!(format!("{:?}", ThumbCipher::new(&tk)), "<redacted>");
        assert_eq!(format!("{:#?}", Some(&k)), "Some(\n    <redacted>,\n)");
    }

    #[test]
    fn subkeys_are_distinct_and_deterministic() {
        let k = dek();
        let (a, b) = (k.db_key(), k.db_key());
        assert_eq!(a.as_bytes(), b.as_bytes());
        let t = k.thumb_keys();
        assert_ne!(a.as_bytes(), &t.aead);
        assert_ne!(a.as_bytes(), &t.name);
        assert_ne!(t.aead, t.name);
        assert_ne!(a.as_bytes(), &k.0);
    }

    #[test]
    fn pragma_value_is_raw_hex_key() {
        let v = dek().db_key().pragma_value();
        assert_eq!(v.len(), 3 + 64 + 2);
        assert!(v.starts_with("\"x'") && v.ends_with("'\""));
    }

    #[test]
    fn thumb_cipher_roundtrip_and_binding() {
        let c = ThumbCipher::new(&dek().thumb_keys());
        let name = c.file_name(b"/a/b.jpg", 10, 20);
        assert_eq!(name.len(), 32);
        assert!(name.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(name, c.file_name(b"/a/b.jpg", 11, 20));
        assert_ne!(name, c.file_name(b"/a/b.jpg", 10, 21));
        assert_ne!(name, c.file_name(b"/a/c.jpg", 10, 20));
        let s1 = c.seal(&name, b"payload").unwrap();
        let s2 = c.seal(&name, b"payload").unwrap();
        assert_ne!(s1, s2, "fresh nonce per file");
        assert_eq!(c.open(&name, &s1).unwrap(), b"payload");
        // Renamed file, tampered file, truncated file, other key: all fail.
        assert!(c.open("00000000000000000000000000000000", &s1).is_err());
        for i in 0..s1.len() {
            let mut t = s1.clone();
            t[i] ^= 1;
            assert!(c.open(&name, &t).is_err(), "byte {i}");
        }
        assert!(c.open(&name, &s1[..s1.len() - 1]).is_err());
        assert!(c.open(&name, &[]).is_err());
        let other = ThumbCipher::new(&VaultKey::from_bytes_for_tests([9; 32]).thumb_keys());
        assert!(other.open(&name, &s1).is_err());
    }

    #[test]
    fn error_messages_are_fixed() {
        for e in [
            VaultError::WrongPassphrase,
            VaultError::DbCorrupt,
            VaultError::HeaderCorrupt,
            VaultError::ParamsOutOfBounds,
        ] {
            assert!(!e.to_string().is_empty());
        }
        assert_eq!(VaultError::WrongPassphrase.to_string(), "wrong passphrase");
    }
}
