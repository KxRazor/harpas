//! The server log, on the console and in `logs/latest.log`
//!
//! Lines have vanilla's format, `[12:34:56] [thread/INFO]: message`. As in vanilla, `latest.log` is compressed into
//! `logs/<date>-<n>.log.gz` when the server starts and when the day changes (`docs/operations.md`).

use std::fs::{self, File};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Local, NaiveDate};
use flate2::Compression;
use flate2::write::GzEncoder;
use log::{Level, LevelFilter, Log, Metadata, Record};
use rustyline::ExternalPrinter;

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Prints above the console's input line, so that log lines do not break what is being typed.
pub type Printer = Box<dyn ExternalPrinter + Send>;

struct Logger {
    state: Mutex<State>,
    colors: bool,
}

struct State {
    dir: PathBuf,
    file: Option<File>,
    date: NaiveDate,
    printer: Option<Printer>,
}

/// Archives the previous `latest.log` in `dir` and sends every log line to the console and a new `latest.log`.
pub fn init(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    archive(dir)?;
    let logger = Logger {
        state: Mutex::new(State {
            dir: dir.to_owned(),
            file: Some(File::create(dir.join("latest.log"))?),
            date: Local::now().date_naive(),
            printer: None,
        }),
        colors: io::stdout().is_terminal(),
    };
    if LOGGER.set(logger).is_err() {
        return Err(io::Error::other("the logger is already set"));
    }
    log::set_logger(LOGGER.get().unwrap()).map_err(|e| io::Error::other(e.to_string()))?;
    log::set_max_level(LevelFilter::Info);
    Ok(())
}

/// Prints console lines through `printer` from now on.
pub fn set_printer(printer: Printer) {
    if let Some(logger) = LOGGER.get() {
        logger.lock().printer = Some(printer);
    }
}

impl Logger {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A panic while logging leaves nothing half-written that matters, so the log keeps working.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        // Harpas logs its own information; libraries only their warnings and errors.
        let own = metadata.target().starts_with("harpas");
        metadata.level() <= if own { Level::Info } else { Level::Warn }
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = Local::now();
        let thread = std::thread::current();
        let line = format_line(
            now,
            thread.name().unwrap_or("unnamed"),
            record.level(),
            &record.args().to_string(),
        );
        let mut state = self.lock();
        if now.date_naive() != state.date {
            state.date = now.date_naive();
            state.file = None;
            let opened =
                archive(&state.dir).and_then(|()| File::create(state.dir.join("latest.log")));
            state.file = opened.ok();
        }
        if let Some(file) = &mut state.file {
            // A full disk must not stop the server; the console still shows the line.
            let _ = writeln!(file, "{line}");
        }
        let console = match (self.colors, record.level()) {
            (true, Level::Error) => format!("\x1b[31m{line}\x1b[0m\n"),
            (true, Level::Warn) => format!("\x1b[33m{line}\x1b[0m\n"),
            _ => format!("{line}\n"),
        };
        let printed = state.printer.as_mut().map(|p| p.print(console.clone()));
        if !matches!(printed, Some(Ok(()))) {
            let _ = io::stdout().write_all(console.as_bytes());
        }
    }

    fn flush(&self) {
        if let Some(file) = &mut self.lock().file {
            let _ = file.flush();
        }
        let _ = io::stdout().flush();
    }
}

/// A log line in vanilla's format, without the line ending.
fn format_line(time: DateTime<Local>, thread: &str, level: Level, message: &str) -> String {
    format!(
        "[{}] [{thread}/{level}]: {message}",
        time.format("%H:%M:%S")
    )
}

/// Compresses `latest.log` in `dir` into `<date>-<n>.log.gz`, dated by its last change, and removes it.
fn archive(dir: &Path) -> io::Result<()> {
    let latest = dir.join("latest.log");
    let metadata = match fs::metadata(&latest) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if metadata.len() > 0 {
        let date = DateTime::<Local>::from(metadata.modified()?).format("%Y-%m-%d");
        let target = (1..)
            .map(|n| dir.join(format!("{date}-{n}.log.gz")))
            .find(|path| !path.exists())
            .unwrap();
        let mut encoder = GzEncoder::new(File::create_new(target)?, Compression::default());
        io::copy(&mut File::open(&latest)?, &mut encoder)?;
        encoder.finish()?;
    }
    fs::remove_file(latest)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use chrono::TimeZone;
    use flate2::read::GzDecoder;

    use super::*;

    #[test]
    fn lines_have_vanillas_format() {
        let time = Local.with_ymd_and_hms(2026, 11, 2, 9, 5, 7).unwrap();
        assert_eq!(
            format_line(time, "main", Level::Warn, "Hello"),
            "[09:05:07] [main/WARN]: Hello"
        );
    }

    #[test]
    fn archives_number_the_logs_of_each_day() {
        let dir = std::env::temp_dir().join(format!("harpas-logs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        archive(&dir).unwrap();
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);

        for text in ["first run\n", "second run\n"] {
            fs::write(dir.join("latest.log"), text).unwrap();
            archive(&dir).unwrap();
        }
        assert!(!dir.join("latest.log").exists());
        let date = Local::now().format("%Y-%m-%d");
        for (n, text) in [(1, "first run\n"), (2, "second run\n")] {
            let mut decoded = String::new();
            GzDecoder::new(File::open(dir.join(format!("{date}-{n}.log.gz"))).unwrap())
                .read_to_string(&mut decoded)
                .unwrap();
            assert_eq!(decoded, text);
        }

        // An empty log is removed without an archive.
        File::create(dir.join("latest.log")).unwrap();
        archive(&dir).unwrap();
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }
}
