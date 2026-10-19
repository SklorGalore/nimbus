//! One Nimbus per user (M25): a second `nimbus` hands its files to the
//! Nimbus already running instead of opening another window.
//!
//! The idea is a *single-instance app* with a doorbell. The running Nimbus
//! listens on a local socket in a folder only this user can open
//! (`run/` beside the config, mode 0700). A new `nimbus` first knocks: if
//! something answers, it sends its file paths (already made absolute, since
//! the two processes may have different current folders), waits for `ok`
//! and exits. If nothing answers, it becomes the running Nimbus itself and
//! listens. The knock happens before the window or the GPU exists, so
//! handing files over never draws anything.
//!
//! On macOS and Linux the socket is a Unix domain socket from `std`: a file
//! in the file system that processes connect to like a network address, but
//! which never leaves the machine and obeys the folder's permissions.
//! Windows has no Unix sockets in `std` and named pipes need `unsafe` or a
//! crate, so there the doorbell is a loopback TCP port (only reachable from
//! this machine) whose number and a random token are written to a file in
//! the user's own `%APPDATA%` folder; a request without the token is
//! dropped. `docs/security.md` has the rules: only file paths and `--wait`
//! are ever accepted, never commands.
//!
//! The wire format is a few records, each ended by a NUL byte (the one
//! byte no path can contain): `nimbus-open 1`, the token on Windows,
//! `wait` if asked, one `path /abs/path` per file, then `end`. The answer
//! is a line: `ok` once the files are handed to the event loop, `no` for a
//! refused request, and, for `--wait`, `done` when the tabs closed.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The first record of every request; the number changes if the format does.
const MAGIC: &str = "nimbus-open 1";
/// The largest request read, in bytes: room for thousands of long paths,
/// small enough that a stuck or hostile client cannot fill memory.
const MAX_REQUEST: u64 = 1 << 20;
/// The most paths one request may name.
const MAX_PATHS: usize = 4096;
/// How long the running Nimbus waits for a request to arrive in full.
const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a knocking `nimbus` waits for `ok`. Generous, because a Nimbus
/// that has just started (the `--wait` case) answers only once its event
/// loop is up.
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// What a second `nimbus` asks of the running one.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Request {
    /// Absolute paths of files and folders to open, in order.
    pub paths: Vec<PathBuf>,
    /// Keep the connection open and say `done` when these files' tabs
    /// have all closed (`--wait`).
    pub wait: bool,
}

/// The connection of a `--wait` request, kept by the running Nimbus until
/// the files it opened are closed.
pub struct Waiter(Stream);

impl Waiter {
    /// Tell the waiting `nimbus` its files are closed, so it exits.
    pub fn done(mut self) {
        // The other side may already be gone (Ctrl+C); nothing to do then.
        let _ = self.0.write_all(b"done\n");
    }
}

impl std::fmt::Debug for Waiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Waiter")
    }
}

/// The answer to a knock.
pub enum Knock {
    /// A running Nimbus took the request: the open connection, to wait on
    /// when the request asked to.
    Answered(Answered),
    /// Nobody is listening (or what answered did not speak the protocol).
    Nobody,
}

/// A running Nimbus that said `ok`.
pub struct Answered(Stream);

impl Answered {
    /// Block until the running Nimbus says the files are closed, or goes
    /// away (quits or crashes), which counts the same for `--wait`.
    pub fn wait(self) {
        let _ = self.0.set_read_timeout(None);
        let mut lines = BufReader::new(self.0);
        let mut line = String::new();
        while lines.read_line(&mut line).is_ok_and(|n| n > 0) {
            if line.trim() == "done" {
                return;
            }
            line.clear();
        }
    }
}

/// The folder holding the doorbell, `run/` beside the config (so a test
/// config in `NIMBUS_CONFIG_DIR` gets a Nimbus of its own), created and
/// checked: `None`, with the reason logged, when it cannot be used safely.
#[must_use]
pub fn dir() -> Option<PathBuf> {
    let dir = crate::config::path()?.parent()?.join("run");
    match prepare(&dir) {
        Ok(()) => Some(dir),
        Err(e) => {
            log::warn!("no single-instance socket: {}: {e}", dir.display());
            None
        }
    }
}

/// Make `dir` readable by this user only, and check that it is.
#[cfg(unix)]
fn prepare(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let parent = dir.parent().ok_or("no parent folder")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.to_string()),
    }
    // Not followed if it is a link: a link could point anywhere.
    let meta = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    if !meta.is_dir() {
        return Err("not a folder".to_owned());
    }
    if meta.mode() & 0o077 != 0 {
        return Err("other users can open it (it must be mode 0700)".to_owned());
    }
    let owner = std::fs::metadata(parent).map_err(|e| e.to_string())?.uid();
    if meta.uid() != owner {
        return Err("it belongs to another user".to_owned());
    }
    Ok(())
}

/// On Windows, `%APPDATA%` is readable by its user only, and a folder made
/// inside it inherits that.
#[cfg(not(unix))]
fn prepare(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())
}

// ---- The wire format, the same on every platform. ----

/// The request as bytes, `None` when a path is not valid Unicode (rare;
/// such a run opens a window of its own rather than mangle the name).
#[must_use]
pub fn encode(req: &Request, token: Option<&str>) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut record = |r: &str| {
        out.extend_from_slice(r.as_bytes());
        out.push(0);
    };
    record(MAGIC);
    if let Some(t) = token {
        record(&format!("token {t}"));
    }
    if req.wait {
        record("wait");
    }
    for p in &req.paths {
        record(&format!("path {}", p.to_str()?));
    }
    record("end");
    Some(out)
}

/// Why a request was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The connection closed before anything was sent: a knock that only
    /// checks whether Nimbus is running.
    Empty,
    /// Anything else, with what was wrong.
    Bad(String),
}

/// Read one request from `r`, checking `token` when there is one. Only
/// the records above are accepted; anything else refuses the whole
/// request, so the socket can never be used to make Nimbus do more than
/// open files.
///
/// # Errors
///
/// [`Refused`] says why.
pub fn read_request(r: impl Read, token: Option<&str>) -> Result<Request, Refused> {
    let mut reader = BufReader::new(r.take(MAX_REQUEST));
    let mut req = Request::default();
    let mut buf = Vec::new();
    let mut first = true;
    let mut token_seen = token.is_none();
    loop {
        buf.clear();
        let n = reader
            .read_until(0, &mut buf)
            .map_err(|e| Refused::Bad(e.to_string()))?;
        if n == 0 && first {
            return Err(Refused::Empty);
        }
        if buf.pop() != Some(0) {
            return Err(Refused::Bad("cut short".to_owned()));
        }
        let record = std::str::from_utf8(&buf).map_err(|_| Refused::Bad("not UTF-8".to_owned()))?;
        if first {
            if record != MAGIC {
                return Err(Refused::Bad("not a Nimbus request".to_owned()));
            }
            first = false;
            continue;
        }
        if !token_seen {
            let given = record.strip_prefix("token ").unwrap_or("");
            if !token.is_some_and(|t| same(t.as_bytes(), given.as_bytes())) {
                return Err(Refused::Bad("wrong token".to_owned()));
            }
            token_seen = true;
            continue;
        }
        match record {
            "end" => return Ok(req),
            "wait" => req.wait = true,
            _ => {
                let Some(p) = record.strip_prefix("path ") else {
                    return Err(Refused::Bad(format!("unknown record {record:?}")));
                };
                let path = PathBuf::from(p);
                if !path.is_absolute() {
                    return Err(Refused::Bad(format!("{p:?} is not an absolute path")));
                }
                if req.paths.len() == MAX_PATHS {
                    return Err(Refused::Bad("too many paths".to_owned()));
                }
                req.paths.push(path);
            }
        }
    }
}

/// Compare two byte strings in time that does not depend on where they
/// differ, so the token cannot be guessed a byte at a time.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Send `req` (or, for `None`, nothing: just see whether anyone is there)
/// over `stream` and read the `ok`.
fn ask(mut stream: Stream, req: Option<&Request>, token: Option<&str>) -> Knock {
    let Some(req) = req else {
        // Connecting was the question; closing says "nothing to open".
        return Knock::Answered(Answered(stream));
    };
    let Some(bytes) = encode(req, token) else {
        return Knock::Nobody;
    };
    let _ = stream.set_read_timeout(Some(ACK_TIMEOUT));
    let _ = stream.set_write_timeout(Some(ACK_TIMEOUT));
    if let Err(e) = stream.write_all(&bytes) {
        log::warn!("could not hand files to the running Nimbus: {e}");
        return Knock::Nobody;
    }
    let mut line = [0u8; 3];
    match stream.read_exact(&mut line) {
        Ok(()) if &line == b"ok\n" => Knock::Answered(Answered(stream)),
        Ok(()) => {
            log::warn!("the running Nimbus refused the files");
            Knock::Nobody
        }
        Err(e) => {
            log::warn!("no answer from the running Nimbus: {e}");
            Knock::Nobody
        }
    }
}

/// Serve one connection on the running Nimbus: read the request, say `ok`
/// and hand it to `deliver` with the connection when it waits.
fn answer(
    mut stream: Stream,
    token: Option<&str>,
    deliver: &mut dyn FnMut(Request, Option<Waiter>),
) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
    let req = match read_request(&mut stream, token) {
        Ok(r) => r,
        Err(Refused::Empty) => return,
        Err(Refused::Bad(why)) => {
            log::warn!("refused a request on the single-instance socket: {why}");
            let _ = stream.write_all(b"no\n");
            return;
        }
    };
    if stream.write_all(b"ok\n").is_err() {
        return;
    }
    let waiter = req.wait.then(|| {
        let _ = stream.set_read_timeout(None);
        Waiter(stream)
    });
    deliver(req, waiter);
}

/// The running Nimbus's side: the socket it listens on.
pub struct Doorbell {
    listener: Listener,
    /// The token requests must carry (Windows).
    token: Option<String>,
    /// The socket or port file, removed when Nimbus quits.
    file: PathBuf,
}

/// Removes the doorbell's file when dropped, at the end of `main`, so the
/// next `nimbus` does not knock on a closed door. A crash leaves it; the
/// next knock finds nobody and replaces it.
pub struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Doorbell {
    /// Answer knocks on a thread of its own, handing each request to
    /// `deliver` (which posts it to the event loop). Returns what removes
    /// the socket when Nimbus quits.
    pub fn listen(
        self,
        mut deliver: impl FnMut(Request, Option<Waiter>) + Send + 'static,
    ) -> Cleanup {
        let Self {
            listener,
            token,
            file,
        } = self;
        let spawned = std::thread::Builder::new()
            .name("nimbus-instance".to_owned())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(s) => answer(s, token.as_deref(), &mut deliver),
                        Err(e) => log::debug!("single-instance socket: {e}"),
                    }
                }
            });
        if let Err(e) = spawned {
            log::warn!("could not start the single-instance listener: {e}");
        }
        Cleanup(file)
    }
}

// ---- macOS and Linux: a Unix domain socket. ----

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(unix)]
type Listener = std::os::unix::net::UnixListener;

/// The socket's file name inside the run folder.
#[cfg(unix)]
const SOCKET: &str = "nimbus.sock";

/// Knock on the doorbell in `dir`, sending `req`.
#[cfg(unix)]
#[must_use]
pub fn knock(dir: &Path, req: Option<&Request>) -> Knock {
    let path = dir.join(SOCKET);
    match Stream::connect(&path) {
        Ok(stream) => ask(stream, req, None),
        Err(e) => {
            if e.kind() == std::io::ErrorKind::ConnectionRefused {
                // A socket file nobody listens on: left by a crash.
                log::debug!("removing a stale socket {}", path.display());
                let _ = std::fs::remove_file(&path);
            }
            Knock::Nobody
        }
    }
}

/// Start listening in `dir`, after [`knock`] found nobody. `None` when
/// another Nimbus got there first, or the socket cannot be made.
#[cfg(unix)]
#[must_use]
pub fn claim(dir: &Path) -> Option<Doorbell> {
    let file = dir.join(SOCKET);
    match Listener::bind(&file) {
        Ok(listener) => Some(Doorbell {
            listener,
            token: None,
            file,
        }),
        Err(e) => {
            log::debug!("single-instance socket {}: {e}", file.display());
            None
        }
    }
}

// ---- Windows: a loopback port and a token in a file. ----

#[cfg(not(unix))]
type Stream = std::net::TcpStream;
#[cfg(not(unix))]
type Listener = std::net::TcpListener;

/// The file holding the port and token inside the run folder.
#[cfg(not(unix))]
const PORT_FILE: &str = "instance";

/// Knock on the port named in `dir`'s port file, sending `req`.
#[cfg(not(unix))]
#[must_use]
pub fn knock(dir: &Path, req: Option<&Request>) -> Knock {
    let Ok(text) = std::fs::read_to_string(dir.join(PORT_FILE)) else {
        return Knock::Nobody;
    };
    let mut words = text.split_whitespace();
    let (Some(port), Some(token)) = (words.next().and_then(|p| p.parse().ok()), words.next())
    else {
        return Knock::Nobody;
    };
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    match Stream::connect_timeout(&address, Duration::from_millis(500)) {
        Ok(stream) => ask(stream, req, Some(token)),
        Err(_) => Knock::Nobody,
    }
}

/// Listen on a free loopback port and write it, with a fresh token, to
/// `dir`'s port file.
#[cfg(not(unix))]
#[must_use]
pub fn claim(dir: &Path) -> Option<Doorbell> {
    let listener = Listener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    let token = random_token();
    let file = dir.join(PORT_FILE);
    let temp = dir.join(format!("{PORT_FILE}.tmp"));
    let written = std::fs::write(&temp, format!("{port} {token}\n"))
        .and_then(|()| std::fs::rename(&temp, &file));
    if let Err(e) = written {
        log::warn!("could not write {}: {e}", file.display());
        return None;
    }
    Some(Doorbell {
        listener,
        token: Some(token),
        file,
    })
}

/// 128 random bits as hex. `std` has no random number API, but each
/// `RandomState` is seeded from the operating system's random source (that
/// is what protects `HashMap` from crafted keys), so hashing with two of
/// them gives unpredictable numbers without a new crate.
#[cfg(not(unix))]
fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let half = || {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(u64::from(std::process::id()));
        h.finish()
    };
    format!("{:016x}{:016x}", half(), half())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(paths: &[&str], wait: bool) -> Request {
        Request {
            paths: paths.iter().map(PathBuf::from).collect(),
            wait,
        }
    }

    #[cfg(unix)]
    const A: &str = "/tmp/a b.rs";
    #[cfg(not(unix))]
    const A: &str = r"C:\tmp\a b.rs";

    #[test]
    fn requests_round_trip() {
        for r in [req(&[], false), req(&[A], true), req(&[A, A], false)] {
            let bytes = encode(&r, None).unwrap();
            assert_eq!(read_request(&bytes[..], None), Ok(r.clone()));
            let bytes = encode(&r, Some("secret")).unwrap();
            assert_eq!(read_request(&bytes[..], Some("secret")), Ok(r));
        }
    }

    #[test]
    fn bad_requests_are_refused() {
        let good = encode(&req(&[A], false), Some("secret")).unwrap();
        assert!(matches!(
            read_request(&good[..], Some("other")),
            Err(Refused::Bad(_))
        ));
        assert_eq!(read_request(&b""[..], None), Err(Refused::Empty));
        for bad in [
            &b"hello\0end\0"[..],
            b"nimbus-open 1\0path relative.rs\0end\0",
            b"nimbus-open 1\0run rm -rf\0end\0",
            b"nimbus-open 1\0wait\0",
            b"nimbus-open 1\0wait",
            b"nimbus-open 1\0\xff\0end\0",
        ] {
            assert!(
                matches!(read_request(bad, None), Err(Refused::Bad(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn requests_are_capped() {
        let many = req(&vec![A; MAX_PATHS + 1], false);
        let bytes = encode(&many, None).unwrap();
        assert!(matches!(
            read_request(&bytes[..], None),
            Err(Refused::Bad(_))
        ));
        let mut huge = b"nimbus-open 1\0path /".to_vec();
        huge.resize(usize::try_from(MAX_REQUEST).unwrap() + 10, b'a');
        assert!(matches!(
            read_request(&huge[..], None),
            Err(Refused::Bad(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_paths_are_not_sent() {
        use std::os::unix::ffi::OsStrExt;
        let odd = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff.rs"));
        let r = Request {
            paths: vec![odd],
            wait: false,
        };
        assert_eq!(encode(&r, None), None);
    }

    #[test]
    fn tokens_compare_whole() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"ab"));
    }

    /// A fresh run folder for one test.
    fn run_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("nimbus-instance-{tag}-{}", std::process::id()))
            .join("run");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        prepare(&dir).unwrap();
        dir
    }

    #[test]
    fn a_second_nimbus_hands_its_files_over() {
        let dir = run_dir("handoff");
        assert!(matches!(knock(&dir, None), Knock::Nobody));
        let bell = claim(&dir).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let cleanup = bell.listen(move |r, w| {
            let _ = tx.send((r, w));
        });
        // Anyone there? Yes, and it is not asked to open anything.
        assert!(matches!(knock(&dir, None), Knock::Answered(_)));

        let sent = req(&[A], false);
        assert!(matches!(knock(&dir, Some(&sent)), Knock::Answered(_)));
        let (got, waiter) = rx.recv().unwrap();
        assert_eq!(got, sent);
        assert!(waiter.is_none());

        // --wait: the knock's connection returns once the waiter is done.
        let asked = req(&[A], true);
        let Knock::Answered(answered) = knock(&dir, Some(&asked)) else {
            panic!("no answer");
        };
        let (got, waiter) = rx.recv().unwrap();
        assert_eq!(got, asked);
        let back = std::thread::spawn(move || answered.wait());
        waiter.unwrap().done();
        back.join().unwrap();

        drop(cleanup);
        assert!(matches!(knock(&dir, None), Knock::Nobody));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_socket_is_replaced() {
        let dir = run_dir("stale");
        // Bound and dropped without removing the file, as after a crash.
        drop(Listener::bind(dir.join(SOCKET)).unwrap());
        assert!(dir.join(SOCKET).exists());
        assert!(matches!(knock(&dir, None), Knock::Nobody));
        assert!(claim(&dir).is_some());
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn an_open_run_folder_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = run_dir("open");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare(&dir).is_err());
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}
