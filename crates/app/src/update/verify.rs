//! Proving a release is the owner's before any of it is used (M31).
//!
//! Two checks, because they answer different questions:
//!
//! - A **checksum** (SHA-256) says a file is the one a list names. Anyone
//!   who can change the release page can change the file and the list
//!   together, so a checksum alone proves nothing about who made them.
//! - A **signature** says the owner made the list. Each release's
//!   `SHA256SUMS` is signed with `minisign` on the owner's Mac
//!   (`scripts/sign-release.sh`, M26); the matching public key,
//!   `minisign.pub` at the root of the repository, is compiled into this
//!   binary. A changed file, a changed list or a list signed by any
//!   other key is refused.
//!
//! A signature alone is not enough either: the owner's signature on an
//! *older* release is still a good signature. So the signature's trusted
//! comment, which the signature also covers, must read exactly
//! `Nimbus X.Y.Z SHA256SUMS` for the version being installed, and that
//! version must be newer than the one running. An attacker who can serve
//! files cannot replay 1.0.0 as 2.0.0, nor push a downgrade.

use std::fmt;

use minisign_verify::{PublicKey, Signature};

/// The release-signing keys this build trusts, in `minisign.pub` format.
/// One today; when M26 rotates the key, the new one is added here for a
/// release or two so builds that know only the old key still update.
pub const TRUSTED_KEYS: &[&str] = &[include_str!("../../../../minisign.pub")];

/// Why a release was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// A key built in, or the signature file, is not minisign's format.
    Malformed(&'static str),
    /// No trusted key made this signature, or it does not match the list.
    BadSignature,
    /// The signature is good but names another release, as when an older
    /// release's list is offered as a newer one.
    WrongRelease {
        /// What the trusted comment says.
        found: String,
        /// What it had to say.
        wanted: String,
    },
    /// The release is not newer than the running version.
    NotNewer {
        /// The release offered.
        version: String,
        /// The running version.
        running: String,
    },
    /// The signed list has no line for this package.
    NotListed(String),
    /// The package's SHA-256 is not the one in the signed list.
    HashMismatch(String),
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "the {what} is not in minisign's format"),
            Self::BadSignature => f.write_str("the release's signature is not the owner's"),
            Self::WrongRelease { found, wanted } => {
                write!(f, "the signature is for {found:?}, not {wanted:?}")
            }
            Self::NotNewer { version, running } => {
                write!(f, "{version} is not newer than the running {running}")
            }
            Self::NotListed(name) => write!(f, "{name} is not in the signed SHA256SUMS"),
            Self::HashMismatch(name) => {
                write!(f, "{name} does not match its SHA-256 in the signed list")
            }
        }
    }
}

impl std::error::Error for VerifyError {}

/// A `SHA256SUMS` whose signature has been checked: each package's name
/// and hash. Only [`verify_sums`] makes one, so holding a `Sums` means
/// the list is the owner's for the version it was checked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sums {
    /// `(file name, SHA-256)` for each line.
    entries: Vec<(String, [u8; 32])>,
}

impl Sums {
    /// Check that `name`'s hash, computed as it was downloaded, is the one
    /// the signed list gives it.
    ///
    /// # Errors
    ///
    /// When the list has no line for `name`, or the hashes differ.
    pub fn check(&self, name: &str, hash: &[u8; 32]) -> Result<(), VerifyError> {
        let (_, want) = self
            .entries
            .iter()
            .find(|(n, _)| n == name)
            .ok_or_else(|| VerifyError::NotListed(name.to_owned()))?;
        if want == hash {
            Ok(())
        } else {
            Err(VerifyError::HashMismatch(name.to_owned()))
        }
    }
}

/// The trusted comment a release's signature must carry.
#[must_use]
pub fn trusted_comment(version: &str) -> String {
    format!("Nimbus {version} SHA256SUMS")
}

/// Check `sums`, the bytes of a release's `SHA256SUMS`, against
/// `signature`, its `SHA256SUMS.minisig`, for release `version` while
/// `running` is the running version. `keys` are the trusted public keys
/// ([`TRUSTED_KEYS`] outside tests); a signature by any one of them is
/// accepted.
///
/// # Errors
///
/// When the version is not newer, the signature is unreadable, no
/// trusted key made it, it names another release, or the list is not
/// `HASH  NAME` lines.
pub fn verify_sums(
    sums: &[u8],
    signature: &str,
    keys: &[&str],
    version: &str,
    running: &str,
) -> Result<Sums, VerifyError> {
    let newer = match (super::parse_version(version), super::parse_version(running)) {
        (Some(v), Some(r)) => v > r,
        _ => false,
    };
    if !newer {
        return Err(VerifyError::NotNewer {
            version: version.to_owned(),
            running: running.to_owned(),
        });
    }
    let signature =
        Signature::decode(signature).map_err(|_| VerifyError::Malformed("signature"))?;
    let mut signed = false;
    for key in keys {
        let key = PublicKey::decode(key).map_err(|_| VerifyError::Malformed("built-in key"))?;
        // Prehashed ("ED") signatures only, which minisign has made by
        // default since 0.8; the legacy kind signs the whole file and is
        // not what scripts/sign-release.sh produces.
        if key.verify(sums, &signature, false).is_ok() {
            signed = true;
            break;
        }
    }
    if !signed {
        return Err(VerifyError::BadSignature);
    }
    // Only now is the comment worth reading: the signature covers it.
    let wanted = trusted_comment(version);
    if signature.trusted_comment() != wanted {
        return Err(VerifyError::WrongRelease {
            found: signature.trusted_comment().to_owned(),
            wanted,
        });
    }
    parse_sums(sums).ok_or(VerifyError::Malformed("SHA256SUMS"))
}

/// Read `sha256sum`'s output: one `HEX  NAME` line per file (a `*` before
/// the name marks binary mode on some systems and is dropped).
fn parse_sums(sums: &[u8]) -> Option<Sums> {
    let text = std::str::from_utf8(sums).ok()?;
    let mut entries = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (hex, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start();
        let name = name.strip_prefix('*').unwrap_or(name);
        entries.push((name.to_owned(), parse_hex(hex)?));
    }
    Some(Sums { entries })
}

/// 64 hex digits as 32 bytes.
fn parse_hex(hex: &str) -> Option<[u8; 32]> {
    let bytes = hex.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0; 32];
    let (pairs, _) = bytes.as_chunks::<2>();
    for (o, pair) in out.iter_mut().zip(pairs) {
        let digit = |b: u8| char::from(b).to_digit(16);
        let (hi, lo) = (digit(pair[0])?, digit(pair[1])?);
        *o = u8::try_from(hi * 16 + lo).ok()?;
    }
    Some(out)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// The throwaway key and fake 2.0.0 release in `testdata/`
    /// (`make-fixtures.py` writes them).
    pub const TEST_KEY: &str = include_str!("testdata/test-key.pub");
    pub const SUMS: &[u8] = include_bytes!("testdata/SHA256SUMS");
    pub const SIG: &str = include_str!("testdata/SHA256SUMS.minisig");
    const SIG_OLD: &str = include_str!("testdata/SHA256SUMS-old-version.minisig");
    const SIG_OTHER: &str = include_str!("testdata/SHA256SUMS-other-key.minisig");

    /// What `make-fixtures.py` puts in each fake package.
    pub fn fake_package(name: &str) -> Vec<u8> {
        format!("fake package {name}\n").into_bytes()
    }

    fn sha256(data: &[u8]) -> [u8; 32] {
        let digest = ring::digest::digest(&ring::digest::SHA256, data);
        let mut out = [0; 32];
        out.copy_from_slice(digest.as_ref());
        out
    }

    #[test]
    fn the_real_key_verifies_the_real_1_0_1_release() {
        let sums = include_bytes!("testdata/release-1.0.1-SHA256SUMS");
        let sig = include_str!("testdata/release-1.0.1-SHA256SUMS.minisig");
        let checked = verify_sums(sums, sig, TRUSTED_KEYS, "1.0.1", "1.0.0").unwrap();
        let mut hash = [0; 32];
        hash.copy_from_slice(
            &parse_hex("f70bd1822656f2f4bfc1536e34ea1821327bd70462e4f45c479cf294478710b4").unwrap(),
        );
        checked.check("Nimbus-1.0.1-macos.zip", &hash).unwrap();
    }

    #[test]
    fn an_older_release_signature_is_not_a_newer_release() {
        // The owner's real 1.0.0 signature, offered for the 1.0.1 list.
        let sums = include_bytes!("testdata/release-1.0.1-SHA256SUMS");
        let sig = include_str!("testdata/release-1.0.0-SHA256SUMS.minisig");
        let err = verify_sums(sums, sig, TRUSTED_KEYS, "1.0.1", "1.0.0").unwrap_err();
        // It signs other bytes (1.0.0's list), so the signature fails first.
        assert_eq!(err, VerifyError::BadSignature);
        // And the same list re-signed with an old comment fails on the comment.
        let err = verify_sums(SUMS, SIG_OLD, &[TEST_KEY], "2.0.0", "1.1.0").unwrap_err();
        assert_eq!(
            err,
            VerifyError::WrongRelease {
                found: "Nimbus 1.0.0 SHA256SUMS".to_owned(),
                wanted: "Nimbus 2.0.0 SHA256SUMS".to_owned(),
            }
        );
    }

    #[test]
    fn a_good_release_checks_each_package() {
        let checked = verify_sums(SUMS, SIG, &[TEST_KEY], "2.0.0", "1.1.0").unwrap();
        let name = "Nimbus-2.0.0-macos.zip";
        checked.check(name, &sha256(&fake_package(name))).unwrap();
        assert_eq!(
            checked.check(name, &sha256(b"something else")),
            Err(VerifyError::HashMismatch(name.to_owned()))
        );
        assert_eq!(
            checked.check("nimbus_2.0.0_amd64.deb", &[0; 32]),
            Err(VerifyError::NotListed("nimbus_2.0.0_amd64.deb".to_owned()))
        );
    }

    #[test]
    fn other_keys_and_changed_lists_are_refused() {
        assert_eq!(
            verify_sums(SUMS, SIG_OTHER, &[TEST_KEY], "2.0.0", "1.1.0"),
            Err(VerifyError::BadSignature)
        );
        // The owner's key does not vouch for the test key's signature.
        assert_eq!(
            verify_sums(SUMS, SIG, TRUSTED_KEYS, "2.0.0", "1.1.0"),
            Err(VerifyError::BadSignature)
        );
        // Either of two keys will do (a key rotation).
        assert!(verify_sums(SUMS, SIG, &[TRUSTED_KEYS[0], TEST_KEY], "2.0.0", "1.1.0").is_ok());
        let mut changed = SUMS.to_vec();
        changed[0] ^= 1;
        assert_eq!(
            verify_sums(&changed, SIG, &[TEST_KEY], "2.0.0", "1.1.0"),
            Err(VerifyError::BadSignature)
        );
        assert_eq!(
            verify_sums(SUMS, "not a signature", &[TEST_KEY], "2.0.0", "1.1.0"),
            Err(VerifyError::Malformed("signature"))
        );
    }

    #[test]
    fn downgrades_and_reinstalls_are_refused() {
        for running in ["2.0.0", "2.0.1", "10.0.0", "not a version"] {
            assert!(matches!(
                verify_sums(SUMS, SIG, &[TEST_KEY], "2.0.0", running),
                Err(VerifyError::NotNewer { .. })
            ));
        }
    }

    #[test]
    fn sums_lines_are_read_like_sha256sum_writes_them() {
        let hex = "00".repeat(31) + "ff";
        let sums = parse_sums(format!("{hex}  a.zip\n{hex} *b.exe\n\n").as_bytes()).unwrap();
        let mut want = [0; 32];
        want[31] = 0xff;
        assert_eq!(
            sums.entries,
            vec![("a.zip".to_owned(), want), ("b.exe".to_owned(), want)]
        );
        assert!(parse_sums(b"abc  a.zip\n").is_none());
        assert!(parse_sums(format!("{hex}\n").as_bytes()).is_none());
        assert!(parse_hex(&"g".repeat(64)).is_none());
    }
}
