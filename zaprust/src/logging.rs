// Свой минималистичный логгер (без крейтов). Инициализируется первой строкой
// в каждой точке входа (GUI и элевированные реинвоки), пишет в ОДИН общий файл
// в append-режиме (построчная запись атомарна — безопасно для двух процессов).
//
// Формат: [YYYY-MM-DD HH:MM:SS.mmm] [LEVEL] [role:pid] [component] message

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }
    fn sev(self) -> u8 {
        match self {
            Level::Error => 0,
            Level::Warn => 1,
            Level::Info => 2,
            Level::Debug => 3,
        }
    }
}

struct Logger {
    path: PathBuf,
    role: String,
    pid: u32,
    min: Level,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Инициализировать логгер для роли (gui/svc/autoselect/update/…). Идемпотентно.
pub fn init(role: &str) {
    if LOGGER.get().is_some() {
        return;
    }
    let path = pick_log_path();
    rotate(&path);
    let min = if cfg!(debug_assertions) {
        Level::Debug
    } else {
        Level::Info
    };
    let _ = LOGGER.set(Logger {
        path,
        role: role.to_owned(),
        pid: std::process::id(),
        min,
    });
    install_panic_hook();
}

pub fn log_dir() -> Option<PathBuf> {
    LOGGER
        .get()
        .and_then(|l| l.path.parent().map(|p| p.to_path_buf()))
}

pub fn log_path() -> Option<PathBuf> {
    LOGGER.get().map(|l| l.path.clone())
}

pub fn error(component: &str, msg: impl AsRef<str>) {
    write(Level::Error, component, msg.as_ref());
}
pub fn warn(component: &str, msg: impl AsRef<str>) {
    write(Level::Warn, component, msg.as_ref());
}
pub fn info(component: &str, msg: impl AsRef<str>) {
    write(Level::Info, component, msg.as_ref());
}
pub fn debug(component: &str, msg: impl AsRef<str>) {
    write(Level::Debug, component, msg.as_ref());
}

fn write(level: Level, component: &str, msg: &str) {
    let Some(l) = LOGGER.get() else {
        return;
    };
    if level.sev() > l.min.sev() {
        return;
    }
    let line = format!(
        "[{}] [{}] [{}:{}] [{}] {}\n",
        now(),
        level.tag(),
        l.role,
        l.pid,
        component,
        msg.replace('\n', " | ")
    );
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&l.path) {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
}

/// Последние n строк уровней ERROR/WARN из текущего лога (для «Скопировать диагностику»).
pub fn recent_problems(n: usize) -> Vec<String> {
    let Some(path) = log_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| l.contains("[ERROR]") || l.contains("[WARN]"))
        .map(|l| l.to_owned())
        .collect();
    let len = lines.len();
    if len > n {
        lines.drain(0..len - n);
    }
    lines
}

// ── Внутреннее ───────────────────────────────────────────────────────────────

fn pick_log_path() -> PathBuf {
    // 1) logs/ рядом с exe (портабл).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let logs = dir.join("logs");
            if std::fs::create_dir_all(&logs).is_ok() && writable(&logs) {
                return logs.join("zaprust.log");
            }
        }
    }
    // 2) %LOCALAPPDATA%\Zaprust\logs.
    if let Some(base) = directories::BaseDirs::new() {
        let logs = base.data_local_dir().join("Zaprust").join("logs");
        if std::fs::create_dir_all(&logs).is_ok() && writable(&logs) {
            return logs.join("zaprust.log");
        }
    }
    // 3) %TEMP%.
    std::env::temp_dir().join("zaprust.log")
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(".zaprust_w");
    let ok = std::fs::write(&probe, b"x").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

fn sibling(path: &Path, n: u32) -> PathBuf {
    path.with_file_name(format!("zaprust.{n}.log"))
}

/// Ротация по размеру (~2 МБ): zaprust.log → .1 → .2 → .3 (хранить 3).
fn rotate(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= 2 * 1024 * 1024 {
        return;
    }
    let _ = std::fs::remove_file(sibling(path, 3));
    let _ = std::fs::rename(sibling(path, 2), sibling(path, 3));
    let _ = std::fs::rename(sibling(path, 1), sibling(path, 2));
    let _ = std::fs::rename(path, sibling(path, 1));
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "?".to_owned());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<no message>".to_owned());
        let bt = std::backtrace::Backtrace::force_capture();
        write(
            Level::Error,
            "panic",
            &format!("PANIC at {loc}: {msg}\n{bt}"),
        );
    }));
}

#[cfg(windows)]
fn now() -> String {
    #[repr(C)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        millis: u16,
    }
    extern "system" {
        fn GetLocalTime(out: *mut SystemTime);
    }
    unsafe {
        let mut t: SystemTime = std::mem::zeroed();
        GetLocalTime(&mut t);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            t.year, t.month, t.day, t.hour, t.minute, t.second, t.millis
        )
    }
}

#[cfg(not(windows))]
fn now() -> String {
    "0000-00-00 00:00:00.000".to_owned()
}
