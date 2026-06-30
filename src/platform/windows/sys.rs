// Прочие системные мелочи Windows: версия ОС, буфер обмена, открытие пути.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Полный путь к файлу конфигурации: `%APPDATA%\Zaprust\config.json`.
pub fn config_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.config_dir().join("Zaprust").join("config.json"))
}

/// Упорядоченные кандидаты каталога логов (как было в логгере до L8): сперва
/// `logs\` рядом с exe (портабл), затем `%LOCALAPPDATA%\Zaprust\logs`. Temp-фолбэк
/// добавляет сам логгер.
pub fn log_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join("logs"));
        }
    }
    if let Some(base) = directories::BaseDirs::new() {
        dirs.push(base.data_local_dir().join("Zaprust").join("logs"));
    }
    dirs
}

/// Платформенные строки диагностики (на Windows ограничиваемся архитектурой —
/// остальное уже в общей шапке).
pub fn diag_lines() -> Vec<String> {
    vec![format!("arch: {}", std::env::consts::ARCH)]
}

/// Версия Windows (через `cmd /c ver`).
pub fn os_version() -> String {
    let mut cmd = Command::new("cmd");
    cmd.args(["/c", "ver"]);
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::consts::OS.to_owned())
}

/// Открыть путь в проводнике.
pub fn open_path(path: &Path) {
    let _ = Command::new("explorer").arg(path).spawn();
}

/// Положить текст в буфер обмена (CF_UNICODETEXT).
pub fn set_clipboard(text: &str) -> bool {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;
    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;
    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(h: *mut c_void) -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(fmt: u32, mem: *mut c_void) -> *mut c_void;
        fn CloseClipboard() -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalLock(h: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(h: *mut c_void) -> i32;
    }
    let wide: Vec<u16> = OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
        if h.is_null() {
            CloseClipboard();
            return false;
        }
        let p = GlobalLock(h) as *mut u16;
        if !p.is_null() {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            GlobalUnlock(h);
        }
        SetClipboardData(CF_UNICODETEXT, h);
        CloseClipboard();
        true
    }
}
