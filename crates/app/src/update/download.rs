//! Fetching a release's files on a worker thread (M31): the small
//! `SHA256SUMS` and its signature into memory, the package to a file,
//! hashed as it arrives so the file is read once.
//!
//! A download can be cancelled: the worker checks a shared flag between
//! chunks, so Cancel Update takes effect within one read (64 KB). Progress
//! is reported at most every [`PROGRESS_STEP`] bytes, so a fast
//! connection does not flood the event loop with redraws.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::UpdateError;

/// The most a package may be. Releases are about 25 MB; anything far
/// larger is not one of them, and is refused before it fills the disk.
pub const PACKAGE_LIMIT: u64 = 200 * 1024 * 1024;

/// The most `SHA256SUMS` or its signature may be.
const SMALL_LIMIT: u64 = 64 * 1024;

/// How often progress is reported, in bytes.
const PROGRESS_STEP: u64 = 256 * 1024;

/// How the files are fetched. Outside tests this is GitHub over HTTPS;
/// the tests point it at a server on 127.0.0.1 that speaks plain HTTP.
#[derive(Debug, Clone)]
pub struct Source {
    /// The folder of release downloads, without a trailing `/`: the tag
    /// and file name are appended (`{base}/v2.0.0/SHA256SUMS`).
    pub base: String,
    /// Refuse plain `http://`, including on redirects. GitHub's download
    /// links redirect to its file host; a redirect to `http://` would
    /// let anyone on the path change the file, and is refused.
    pub https_only: bool,
}

impl Source {
    /// GitHub's release downloads for this project.
    #[must_use]
    pub fn github() -> Self {
        Self {
            base: format!("{}/download", super::RELEASES_URL),
            https_only: true,
        }
    }

    /// The URL of `file` in release `version`.
    #[must_use]
    pub fn url(&self, version: &str, file: &str) -> String {
        format!("{}/v{version}/{file}", self.base)
    }

    /// An HTTP client for downloads: no overall time limit (a package on
    /// a slow line can take minutes), but connecting and the first byte
    /// of the answer must come quickly.
    fn agent(&self) -> ureq::Agent {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        ureq::Agent::config_builder()
            .tls_config(tls)
            .https_only(self.https_only)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .user_agent(concat!("nimbus/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    }

    /// GET a small file (`SHA256SUMS` or its signature) into memory.
    /// `Ok(None)` when the release has no such file (HTTP 404): a release
    /// that was never signed.
    ///
    /// # Errors
    ///
    /// When the request fails or the file is larger than 64 KB.
    pub fn fetch_small(&self, version: &str, file: &str) -> Result<Option<Vec<u8>>, UpdateError> {
        let url = self.url(version, file);
        let mut response = match self.agent().get(&url).call() {
            Ok(r) => r,
            Err(ureq::Error::StatusCode(404)) => return Ok(None),
            Err(e) => return Err(UpdateError::Request(e.to_string())),
        };
        response
            .body_mut()
            .with_config()
            .limit(SMALL_LIMIT)
            .read_to_vec()
            .map(Some)
            .map_err(|e| UpdateError::Request(e.to_string()))
    }

    /// GET `file` of release `version` into `dest`, returning its SHA-256.
    /// `progress(got, total)` is called as it arrives (`total` when the
    /// server says the size). On any failure the partial file is deleted.
    ///
    /// # Errors
    ///
    /// When the request fails, the package is over [`PACKAGE_LIMIT`],
    /// `cancel` is set, or `dest` cannot be written (a full disk).
    pub fn fetch_to(
        &self,
        version: &str,
        file: &str,
        dest: &Path,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<[u8; 32], UpdateError> {
        let result = self.fetch_to_inner(version, file, dest, cancel, progress);
        if result.is_err() {
            let _ = std::fs::remove_file(dest);
        }
        result
    }

    fn fetch_to_inner(
        &self,
        version: &str,
        file: &str,
        dest: &Path,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<[u8; 32], UpdateError> {
        let url = self.url(version, file);
        let mut response = self
            .agent()
            .get(&url)
            .call()
            .map_err(|e| UpdateError::Request(e.to_string()))?;
        let total = response.body().content_length();
        if total.is_some_and(|t| t > PACKAGE_LIMIT) {
            return Err(UpdateError::TooLarge);
        }
        let mut reader = response
            .body_mut()
            .with_config()
            .limit(PACKAGE_LIMIT)
            .reader();
        let mut out = File::create(dest).map_err(|e| UpdateError::Io(e.to_string()))?;
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buf = vec![0; 64 * 1024];
        let mut got = 0u64;
        let mut reported = 0u64;
        progress(0, total);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(UpdateError::Cancelled);
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(UpdateError::Request(e.to_string())),
            };
            hash.update(&buf[..n]);
            out.write_all(&buf[..n])
                .map_err(|e| UpdateError::Io(e.to_string()))?;
            got += n as u64;
            if got - reported >= PROGRESS_STEP {
                reported = got;
                progress(got, total);
            }
        }
        if total.is_some_and(|t| t != got) {
            return Err(UpdateError::Request(format!(
                "the download stopped after {got} bytes"
            )));
        }
        // The file must be whole on disk before anything trusts it.
        out.sync_all().map_err(|e| UpdateError::Io(e.to_string()))?;
        progress(got, total);
        let mut digest = [0; 32];
        digest.copy_from_slice(hash.finish().as_ref());
        Ok(digest)
    }
}
