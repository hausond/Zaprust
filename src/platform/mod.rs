// Платформенный слой Zaprust.
//
// Весь платформо-зависимый код (служба, элевация, движок обхода, перехват
// пакетов, TLS-бэкенд теста, системные пути) спрятан за трейтами этого модуля.
// GUI, оркестрация Старт/Стоп, автоподбор, апдейтер-оркестрация, конфиг и логи
// зависят ТОЛЬКО от этих трейтов и обращаются к платформе через `host()`.
//
// Windows-реализация (`windows/`) — переезд существующего кода без изменения
// поведения: winws-as-service, ShellExecuteExW("runas"), `sc`, WinDivert,
// native-tls. Linux-реализация (`linux/`) на шаге L1 — заглушки, которые
// компилируются и не паникуют, но возвращают «не реализовано»; настоящий движок
// (nfqws + nftables, pkexec, systemd, rustls) приходит на шагах L2+.

use std::path::{Path, PathBuf};

use crate::strategies::{CoreScan, Strategy};

#[cfg(windows)]
mod windows;
#[cfg(not(windows))]
mod linux;

// ── Общие платформо-независимые типы ─────────────────────────────────────────

/// Состояние службы / драйвера обхода.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum ServiceState {
    #[default]
    Unknown,
    Running,
    /// Конструируется платформенной реализацией (Win: `sc query` → STOPPED);
    /// Linux-заглушка пока его не отдаёт.
    #[allow(dead_code)]
    Stopped,
    NotInstalled,
}

impl ServiceState {
    /// Установлена ли служба (в любом рабочем состоянии).
    pub fn installed(self) -> bool {
        matches!(self, ServiceState::Running | ServiceState::Stopped)
    }
}

/// Готовая к запуску команда движка обхода: что запустить, откуда и с какими
/// аргументами. (Win: winws.exe + bin/ + разрешённые аргументы стратегии.)
#[derive(Clone, Debug)]
pub struct EngineCommand {
    pub program: PathBuf,
    pub cwd: PathBuf,
    pub args: Vec<String>,
}

/// Хэндл одиночного прогона движка для пробы автоподбора. Реализация платформы
/// при `Drop` обязана полностью свернуть прогон (Win: kill winws; Linux позже:
/// kill nfqws + снять правила nftables).
pub trait ProbeHandle {
    /// Код выхода, если движок уже завершился сам; `None` — ещё жив.
    fn try_exit(&mut self) -> Option<i32>;
}

// ── Трейты платформенного слоя ───────────────────────────────────────────────

/// Каталоги и системные мелочи. *(Win: рядом с exe / LOCALAPPDATA; Linux: XDG.)*
pub trait Paths {
    /// Найденная папка ядра (если есть).
    fn core_dir(&self) -> Option<PathBuf>;
    /// Куда устанавливать ядро, если его ещё нет.
    fn preferred_core_dir(&self) -> PathBuf;
    /// Полный путь к файлу конфигурации (config.json). *(Win: `%APPDATA%\Zaprust`;
    /// Linux: `$XDG_CONFIG_HOME/zaprust` с фолбэком `~/.config/zaprust`.)* None —
    /// если каталог не определить.
    fn config_path(&self) -> Option<PathBuf>;
    /// Упорядоченные кандидаты каталога для лог-файла (без temp-фолбэка — его
    /// добавляет логгер сам). Логгер берёт первый создаваемый и пишемый. *(Win:
    /// рядом с exe, затем `%LOCALAPPDATA%\Zaprust\logs`; Linux: XDG state-каталог
    /// исходного пользователя — общий для GUI и элевированных реинвоков.)*
    fn log_dirs(&self) -> Vec<PathBuf>;
    /// Выставить владельца только что созданного файла/каталога на ИСХОДНОГО
    /// пользователя, если процесс выполняется под root. Нужно, чтобы root-реинвок
    /// не оставил лог с владельцем root, в который потом не допишет неэлевированный
    /// GUI. На Windows — no-op (процессы одного пользователя). Рекурсивно для
    /// каталога.
    fn fixup_owner(&self, path: &Path);
    /// Человекочитаемая версия ОС (для шапки лога и диагностики).
    fn os_version(&self) -> String;
    /// Платформенные строки для шапки лога и «Диагностики» (Linux: ядро Linux,
    /// systemd, бэкенд фаервола, euid, версия nfqws, правила перехвата). Пусто —
    /// если добавить нечего.
    fn diag_lines(&self) -> Vec<String>;
    /// Открыть путь в файловом менеджере. *(Win: explorer; Linux: xdg-open.)*
    fn open_path(&self, path: &Path);
    /// Положить текст в системный буфер обмена. `false` — не удалось.
    fn set_clipboard(&self, text: &str) -> bool;
    /// Зарегистрировать desktop-entry + иконку приложения в пользовательских
    /// XDG-каталогах, чтобы среда показывала иконку приложения в доке/таскбаре,
    /// а не обобщённую заглушку. Вызывается один раз при старте GUI; идемпотентно
    /// и без прав root. *(Win: no-op — иконка окна берётся из вшитого ресурса.
    /// Linux: на Wayland окно сопоставляется с `zaprust.desktop` по app_id, а
    /// `_NET_WM_ICON` среда игнорирует, поэтому без установленного entry иконки
    /// нет.)*
    fn integrate_desktop(&self) {}
}

/// Запуск привилегированного реинвока самого приложения. *(Win: runas; Linux: pkexec.)*
pub trait Elevator {
    /// Запущены ли мы с правами администратора/root.
    fn is_elevated(&self) -> bool;
    /// Перезапустить наш exe элевированно с подкомандой и дождаться кода выхода.
    fn run_elevated_self(&self, args: &[&str]) -> Result<i32, String>;
}

/// Источник стратегий обхода. *(Win: парс `.bat` Flowseal; Linux: bol-van.)*
pub trait StrategySource {
    fn scan(&self) -> CoreScan;
}

/// Управление службой обхода. *(Win: `sc` + winws инлайн; Linux: systemd-юнит.)*
/// Все методы, кроме чтения состояния, меняют систему и требуют прав.
pub trait ServiceController {
    /// Состояние службы обхода (read-only, прав не требует).
    fn state(&self) -> ServiceState;
    /// Имя стратегии, с которой установлена служба (если есть).
    fn installed_strategy(&self) -> Option<String>;
    /// Установить службу (start=auto) с выбранной стратегией и запустить.
    fn install(&self, strategy: &str, game_filter: bool) -> Result<(), String>;
    fn start(&self) -> Result<(), String>;
    fn stop(&self) -> Result<(), String>;
    /// Остановить и удалить службу.
    fn remove(&self) -> Result<(), String>;
    /// Полное удаление для чистого сноса (Win: remove + выгрузка драйвера).
    fn uninstall(&self) -> Result<(), String>;
    /// Сбросить движок перехвата перед заменой ядра/подбором.
    /// *(Win: выгрузить драйвер WinDivert; Linux: снять правила + глушить nfqws.)*
    fn reset_engine(&self);
}

/// Авторитетная проверка «обход работает» (tri-check). *(Win: служба RUNNING +
/// процесс winws; Linux: systemctl is-active + nft list + nfqws.)*
pub trait StatusProbe {
    /// PID живого процесса движка (None — не запущен).
    fn engine_alive(&self) -> Option<u32>;
    /// Истинно, только если обход действительно поднят (служба И движок).
    fn authoritative_running(&self) -> bool;
}

/// То, что фактически исполняет служба под root: построить команду движка и (на
/// Linux) управлять правилами фаервола. На шаге автоподбора — поднимать движок
/// для проб. *(Win: аргументы winws; Linux: nft-правила + nfqws.)*
pub trait BypassRuntime {
    /// Команда запуска движка для стратегии (None — стратегия/ядро непригодны).
    fn engine_command(&self, strategy: &Strategy, game_filter: bool) -> Option<EngineCommand>;
    /// Установлен ли движок (Win: winws.exe + WinDivert; Linux позже: nfqws).
    fn engine_installed(&self) -> bool;
    /// Строка диагностики готовности движка (для шапки лога и «Диагностики»).
    fn engine_diag(&self) -> String;
    /// Подготовка к свипу автоподбора: снять активный обход, сбросить движок,
    /// прогреть перехват. *(Win: kill winws + reset WinDivert + прайм.)*
    fn prepare_sweep(&self, strategies: &[Strategy], game_filter: bool);
    /// Поднять движок одиночным прогоном для пробы стратегии.
    fn spawn_probe(&self, strategy: &Strategy, game_filter: bool)
        -> std::io::Result<Box<dyn ProbeHandle>>;
    /// Последний диагностический вывод движка (для разбора кода выхода).
    fn last_engine_output(&self) -> String;
    /// L2: ручной foreground-прогон движка из-под root для проверки обхода —
    /// поднять правила перехвата (nftables/iptables) + запустить демон с выбранной
    /// стратегией, держать до Ctrl-C/Enter, затем СНЯТЬ всё (демон + правила).
    /// Блокирующий. На Windows движок живёт службой (winws-as-service), отдельного
    /// foreground-режима нет — метод возвращает Err. Полноценный Старт через
    /// службу даёт systemd (L5) с элевацией pkexec (L4).
    fn run_foreground(&self, strategy: &Strategy, game_filter: bool) -> Result<(), String>;
    /// L5: тело systemd-службы (`ExecStart`). Под root, без интерактива: поднять
    /// правила перехвата + nfqws с выбранной стратегией и держать на переднем плане,
    /// пока systemd не пришлёт SIGTERM (Стоп), затем СНЯТЬ всё (демон + правила).
    /// Блокирующий, вывод движка идёт в journald. На Windows движок работает
    /// службой winws-as-service напрямую — отдельного тела нет, метод возвращает Err.
    fn run_service(&self, strategy: &Strategy, game_filter: bool) -> Result<(), String>;
}

/// Прогон доменов теста. Сама логика проб (ureq/TCP) общая; за `#[cfg]` спрятан
/// только TLS-бэкенд через настройку HTTP-агента. *(Win: native-tls/SChannel;
/// Linux: rustls — придёт в L6, пока агент без TLS = заглушка.)*
pub trait Tester {
    /// Агент для кнопки «Тест» и запросов GitHub API (умеренные таймауты, редиректы).
    fn agent(&self) -> ureq::Agent;
    /// Агент для автоподбора: тугие таймауты, без редиректов.
    fn probe_agent(&self) -> ureq::Agent;
    /// Агент для скачивания крупных файлов (ядро/обновления): щедрые таймауты и БЕЗ
    /// общего дедлайна, чтобы многомегабайтная загрузка не обрывалась по таймауту
    /// (у `agent()` общий таймаут короткий — он для маленьких запросов).
    fn download_agent(&self) -> ureq::Agent;
}

/// Полный платформенный бэкенд — объединение всех трейтов слоя.
pub trait Platform:
    Paths
    + Elevator
    + StrategySource
    + ServiceController
    + StatusProbe
    + BypassRuntime
    + Tester
    + Send
    + Sync
{
}

// ── Доступ к активной платформе ──────────────────────────────────────────────

#[cfg(windows)]
pub fn host() -> &'static dyn Platform {
    use std::sync::OnceLock;
    static H: OnceLock<windows::WindowsPlatform> = OnceLock::new();
    H.get_or_init(windows::WindowsPlatform::new)
}

#[cfg(not(windows))]
pub fn host() -> &'static dyn Platform {
    use std::sync::OnceLock;
    static H: OnceLock<linux::LinuxPlatform> = OnceLock::new();
    H.get_or_init(linux::LinuxPlatform::new)
}
