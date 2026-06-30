// Персист настроек. Путь даёт платформенный слой (`Paths::config_path`):
// Windows — `%APPDATA%\Zaprust\config.json`; Linux — `$XDG_CONFIG_HOME/zaprust`
// (фолбэк `~/.config/zaprust`).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    /// Выбранный пункт дропдауна по имени; спец-значение "smart" = режим smart.
    pub strategy: Option<String>,
    /// Last-known-good: имя последнего подобранного автоподбором победителя.
    pub auto_best: Option<String>,
    pub game_filter: bool,
    pub ipset: bool,
    /// Простой режим (по умолчанию при первом запуске).
    pub simple_mode: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            strategy: None,
            auto_best: None,
            game_filter: false,
            ipset: true,
            simple_mode: true,
        }
    }
}

/// Спец-значение `strategy`, означающее режим smart (автоподбор).
pub const SMART: &str = "smart";

impl Config {
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            if std::fs::write(&path, text).is_ok() {
                // На случай записи из-под root — оставить файл за исходным
                // пользователем (no-op в обычном GUI и на Windows).
                crate::platform::host().fixup_owner(&path);
            }
        }
    }

    fn path() -> Option<PathBuf> {
        crate::platform::host().config_path()
    }
}
