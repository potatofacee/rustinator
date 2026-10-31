//! Log output: a size-capped file, mirrored to stderr only when stderr is a
//! terminal.
//!
//! Logging used to go to stderr alone. Started from a launcher or script whose
//! stderr is a pipe nobody reads, the pipe fills after ~64 KB and every later
//! write blocks forever, including log calls on the UI thread and the
//! clipboard worker. Records now go to `rustinator.log` in the platform log
//! directory (see `log_dir`). When stderr is a pipe or socket, fd 2 itself is
//! pointed at the same file, so `eprintln!`, panic messages and whatever the
//! GL driver prints cannot block either. A terminal keeps getting a copy of
//! every record; a regular file the user redirected stderr to is left alone.
//! `RUST_LOG` filters as before (default `info`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The log file is moved to `rustinator.log.1` (replacing the previous one)
/// once it reaches this size, at startup or while running.
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

const LOG_FILE: &str = "rustinator.log";

/// Install the global logger. Returns whether log records also reach stderr
/// (so a caller printing a fatal error to stderr as well would print it twice).
pub(crate) fn init() -> bool {
    let builder = || {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
    };
    let stderr = StderrKind::detect();
    let sink = log_dir().and_then(|dir| LogSink::open(&dir.join(LOG_FILE), stderr));
    match sink {
        Ok(sink) => {
            let path = sink.path.clone();
            let mirrored = sink.mirror_stderr;
            let redirected = sink.redirect_fd2;
            let _ = builder().target(env_logger::Target::Pipe(Box::new(sink))).try_init();
            log::info!("logging to {}", path.display());
            mirrored || redirected
        }
        Err(e) => {
            // No usable log file: keep the old stderr logging rather than none.
            let _ = builder().try_init();
            log::warn!("log file unavailable ({e}); logging to stderr only");
            true
        }
    }
}

/// `$XDG_STATE_HOME/rustinator` (only an absolute value counts, per the XDG
/// spec), else `~/.local/state/rustinator`; on macOS `~/Library/Logs/rustinator`.
fn log_dir() -> io::Result<PathBuf> {
    let home = || {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))
    };
    #[cfg(target_os = "macos")]
    {
        Ok(home()?.join("Library/Logs/rustinator"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        Ok(match state {
            Some(state) => state.join("rustinator"),
            None => home()?.join(".local/state/rustinator"),
        })
    }
}

/// What fd 2 is connected to, which decides how logging treats it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StderrKind {
    /// A terminal: someone is watching, so every record is copied there.
    Tty,
    /// A pipe or socket: may have no reader, and blocks once full. fd 2 is
    /// redirected into the log file.
    Pipe,
    /// Anything else (a regular file, /dev/null, closed): cannot block, left as is.
    Other,
}

impl StderrKind {
    #[cfg(unix)]
    fn detect() -> Self {
        if unsafe { libc::isatty(libc::STDERR_FILENO) } == 1 {
            return StderrKind::Tty;
        }
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(libc::STDERR_FILENO, st.as_mut_ptr()) } != 0 {
            return StderrKind::Other;
        }
        let fmt = unsafe { st.assume_init() }.st_mode & libc::S_IFMT;
        if fmt == libc::S_IFIFO || fmt == libc::S_IFSOCK {
            StderrKind::Pipe
        } else {
            StderrKind::Other
        }
    }

    #[cfg(not(unix))]
    fn detect() -> Self {
        StderrKind::Other
    }
}

/// The logger's output: appends records to the log file, rotating it at
/// `MAX_LOG_BYTES`, and copies them to a terminal stderr. env_logger writes
/// each record with one `write_all` under its own lock.
struct LogSink {
    path: PathBuf,
    file: File,
    /// Bytes in `file` as far as this process knows (other instances append
    /// to the same file; `rotate` rechecks the real size).
    written: u64,
    mirror_stderr: bool,
    /// fd 2 points at the log file and must follow it across rotations.
    redirect_fd2: bool,
}

impl LogSink {
    fn open(path: &Path, stderr: StderrKind) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        if fs::metadata(path).is_ok_and(|m| m.len() >= MAX_LOG_BYTES) {
            let _ = fs::rename(path, backup_path(path));
        }
        let file = open_append(path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut sink = LogSink {
            path: path.to_path_buf(),
            file,
            written,
            mirror_stderr: stderr == StderrKind::Tty,
            redirect_fd2: stderr == StderrKind::Pipe,
        };
        if sink.redirect_fd2 && !sink.point_fd2_at_file() {
            sink.redirect_fd2 = false;
        }
        Ok(sink)
    }

    /// dup2 the log file onto fd 2. Returns whether it took.
    #[cfg(unix)]
    fn point_fd2_at_file(&self) -> bool {
        use std::os::fd::AsRawFd;
        unsafe { libc::dup2(self.file.as_raw_fd(), libc::STDERR_FILENO) != -1 }
    }

    #[cfg(not(unix))]
    fn point_fd2_at_file(&self) -> bool {
        false
    }

    /// Called when this process's count says the file is full: move it to
    /// `.1` and start a new one. Another instance appending to the same file
    /// may have rotated it already, leaving our handle on what is now `.1`; then
    /// just reopen the path, without renaming that instance's new file away.
    fn rotate(&mut self) {
        if self.same_file_at_path() {
            let len = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
            if len < MAX_LOG_BYTES {
                self.written = len;
                return;
            }
            let _ = fs::rename(&self.path, backup_path(&self.path));
        }
        if let Ok(file) = open_append(&self.path) {
            self.written = file.metadata().map(|m| m.len()).unwrap_or(0);
            self.file = file;
            if self.redirect_fd2 {
                self.point_fd2_at_file();
            }
        }
    }

    #[cfg(unix)]
    fn same_file_at_path(&self) -> bool {
        use std::os::unix::fs::MetadataExt;
        match (self.file.metadata(), fs::metadata(&self.path)) {
            (Ok(ours), Ok(at_path)) => ours.dev() == at_path.dev() && ours.ino() == at_path.ino(),
            _ => false,
        }
    }

    #[cfg(not(unix))]
    fn same_file_at_path(&self) -> bool {
        true
    }
}

impl Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written > 0 && self.written + buf.len() as u64 > MAX_LOG_BYTES {
            self.rotate();
        }
        let n = self.file.write(buf)?;
        self.written += n as u64;
        if self.mirror_stderr {
            let _ = io::stderr().write_all(&buf[..n]);
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("rustinator-logging-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// Crossing the cap moves the full file to `.1` and continues in a fresh
    /// one; the record that crossed starts the new file.
    #[test]
    fn rotates_at_cap() {
        let dir = scratch_dir("rotate");
        let path = dir.join(LOG_FILE);
        let mut sink = LogSink::open(&path, StderrKind::Other).unwrap();
        let line = vec![b'x'; 1024];
        let lines = MAX_LOG_BYTES / 1024;
        for _ in 0..lines {
            sink.write_all(&line).unwrap();
        }
        assert_eq!(fs::metadata(&path).unwrap().len(), MAX_LOG_BYTES);
        assert!(!backup_path(&path).exists());

        sink.write_all(b"next\n").unwrap();
        assert_eq!(fs::metadata(backup_path(&path)).unwrap().len(), MAX_LOG_BYTES);
        assert_eq!(fs::read(&path).unwrap(), b"next\n");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A file already at the cap is rotated at startup.
    #[test]
    fn rotates_full_file_at_open() {
        let dir = scratch_dir("startup");
        let path = dir.join(LOG_FILE);
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize]).unwrap();
        let mut sink = LogSink::open(&path, StderrKind::Other).unwrap();
        sink.write_all(b"fresh\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"fresh\n");
        assert_eq!(fs::metadata(backup_path(&path)).unwrap().len(), MAX_LOG_BYTES);
        let _ = fs::remove_dir_all(&dir);
    }

    /// When another instance rotated first, this one reopens the new file
    /// instead of renaming it over the backup.
    #[test]
    fn follows_rotation_by_another_instance() {
        let dir = scratch_dir("shared");
        let path = dir.join(LOG_FILE);
        let mut ours = LogSink::open(&path, StderrKind::Other).unwrap();
        ours.write_all(&vec![b'a'; MAX_LOG_BYTES as usize]).unwrap();
        // The other instance: rotates, then writes to the fresh file.
        fs::rename(&path, backup_path(&path)).unwrap();
        fs::write(&path, b"other\n").unwrap();

        ours.write_all(b"ours\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"other\nours\n");
        assert_eq!(fs::metadata(backup_path(&path)).unwrap().len(), MAX_LOG_BYTES);
        let _ = fs::remove_dir_all(&dir);
    }
}
