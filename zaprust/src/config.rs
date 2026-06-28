// Персист настроек: %APPDATA%\Zaprust\config.json.

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
            let _ = std::fs::write(path, text);
        }
    }

    fn path() -> Option<PathBuf> {
        directories::BaseDirs::new()
            .map(|b| b.config_dir().join("Zaprust").join("config.json"))
    }
}
