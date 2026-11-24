use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use log::{Level, LevelFilter, Log, Metadata, Record};

const MAX_BYTES: u64 = 1 << 20;

/// Sends warnings, errors and panics to `minits.log` in the local data directory,
/// keeping one rotated predecessor.
pub fn init() {
    let dir = crate::settings::local_data_dir();
    let _ = fs::create_dir_all(&dir);
    let logger = FileLogger {
        path: dir.join("minits.log"),
        file: Mutex::new(None),
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(LevelFilter::Warn);
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("{info}");
        default_hook(info);
    }));
}

struct FileLogger {
    path: PathBuf,
    /// The open file and its current length.
    file: Mutex<Option<(File, u64)>>,
}

impl FileLogger {
    fn open(&self) -> Option<(File, u64)> {
        if fs::metadata(&self.path).is_ok_and(|m| m.len() >= MAX_BYTES) {
            let _ = fs::rename(&self.path, self.path.with_extension("old.log"));
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok()?;
        let length = file.metadata().map_or(0, |m| m.len());
        Some((file, length))
    }
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Warn
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {}: {}\n",
            jiff::ZonedDateTime::now().strftime("%Y-%m-%d %H:%M:%S"),
            record.level(),
            record.target(),
            record.args()
        );
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        if file.as_ref().is_none_or(|(_, length)| *length >= MAX_BYTES) {
            // Closed before rotating: Windows cannot rename an open file.
            *file = None;
            *file = self.open();
        }
        if let Some((handle, length)) = file.as_mut()
            && handle.write_all(line.as_bytes()).is_ok()
        {
            *length += line.len() as u64;
        }
    }

    fn flush(&self) {}
}
