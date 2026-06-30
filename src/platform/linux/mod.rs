// Linux-реализация платформенного слоя — ЗАГЛУШКИ шага L1.
//
// Цель L1 — собираемость и запуск GUI, не функциональность. Привилегированные и
// движковые операции (служба, элевация, движок обхода, тест по TLS) возвращают
// «не реализовано» и не паникуют. Настоящая реализация приходит дальше:
//   L2 — движок nfqws + правила nftables (BypassRuntime, запуск из root);
//   L3 — источник стратегий bol-van (StrategySource);
//   L4 — элевация через pkexec (Elevator);
//   L5 — systemd-служба (ServiceController);
//   L6 — статус-tri-check (StatusProbe) и тест на rustls (Tester).
//
// Платформо-независимые мелочи (каталоги XDG, версия ОС, открытие пути) сделаны
// сразу по-настоящему — это не движок и заглушкой быть не обязано.

use std::path::{Path, PathBuf};

use crate::platform::{
    BypassRuntime, Elevator, EngineCommand, Paths, Platform, ProbeHandle, ServiceController,
    ServiceState, StatusProbe, StrategySource, Tester,
};
use crate::strategies::{CoreScan, Strategy};

mod desktop;
mod elevate;
mod nft;
pub mod runtime;
mod service;
mod status;
mod strategy;
mod tester;

pub struct LinuxPlatform;

impl LinuxPlatform {
    pub fn new() -> Self {
        LinuxPlatform
    }
}

impl Platform for LinuxPlatform {}

/// Домашний каталог ИСХОДНОГО пользователя, а не root. Под `sudo`/`pkexec` `$HOME`
/// становится `/root`, и ядро/конфиг искались бы там. Поэтому при запуске из-под
/// root уважаем `SUDO_USER`/`PKEXEC_UID` и берём домашний каталог того, кто позвал.
pub(super) fn invoking_user_home() -> Option<PathBuf> {
    // pkexec прокидывает числовой uid позвавшего (L4).
    if let Ok(uid) = std::env::var("PKEXEC_UID") {
        if let Some(home) = home_of_uid(&uid) {
            return Some(home);
        }
    }
    // sudo прокидывает имя позвавшего.
    if let Ok(user) = std::env::var("SUDO_USER") {
        if !user.is_empty() && user != "root" {
            if let Some(home) = home_of_user(&user) {
                return Some(home);
            }
        }
    }
    // Не под элевацией — обычный $HOME.
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Домашний каталог пользователя по имени (через `getent passwd`, фолбэк /home/<user>).
fn home_of_user(user: &str) -> Option<PathBuf> {
    if let Ok(out) = std::process::Command::new("getent").args(["passwd", user]).output() {
        if out.status.success() {
            if let Some(home) = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .and_then(|l| l.split(':').nth(5))
                .filter(|h| !h.is_empty())
            {
                return Some(PathBuf::from(home));
            }
        }
    }
    let guess = PathBuf::from("/home").join(user);
    guess.is_dir().then_some(guess)
}

/// Домашний каталог по числовому uid (через `getent passwd <uid>`).
fn home_of_uid(uid: &str) -> Option<PathBuf> {
    passwd_field(uid, 5).map(PathBuf::from)
}

/// Поле строки `getent passwd <key>` по индексу (0=имя … 3=gid … 5=home).
fn passwd_field(key: &str, idx: usize) -> Option<String> {
    let out = std::process::Command::new("getent").args(["passwd", key]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|l| l.split(':').nth(idx))
        .filter(|v| !v.is_empty())
        .map(|v| v.to_owned())
}

/// uid:gid ИСХОДНОГО пользователя при запуске под sudo/pkexec — чтобы nfqws сбросил
/// привилегии именно на него (а не на nobody) и смог читать файлы ядра в его
/// домашнем каталоге. None — если не под элевацией (тогда --uid не нужен).
pub(super) fn invoking_user_ids() -> Option<(u32, u32)> {
    // sudo прокидывает оба числа.
    if let (Ok(u), Ok(g)) = (std::env::var("SUDO_UID"), std::env::var("SUDO_GID")) {
        if let (Ok(uid), Ok(gid)) = (u.parse::<u32>(), g.parse::<u32>()) {
            if uid != 0 {
                return Some((uid, gid));
            }
        }
    }
    // pkexec прокидывает только uid — gid берём из passwd.
    if let Ok(u) = std::env::var("PKEXEC_UID") {
        if let Ok(uid) = u.parse::<u32>() {
            if uid != 0 {
                let gid = passwd_field(&u, 3).and_then(|g| g.parse().ok()).unwrap_or(uid);
                return Some((uid, gid));
            }
        }
    }
    None
}

/// База данных приложения у ИСХОДНОГО пользователя: `<home>/.local/share/zaprust`.
///
/// Намеренно НЕ honor-им `$XDG_DATA_HOME`: этот каталог общий для GUI и
/// элевированных реинвоков (pkexec/systemd вычищают окружение, и переменная до них
/// не доезжает), поэтому фиксируем канонический фолбэк — обе стороны вычисляют
/// ОДИН И ТОТ ЖЕ путь. Домашний каталог берём у позвавшего (см. `invoking_user_home`),
/// чтобы под sudo/pkexec не уехать в `/root`.
pub(super) fn data_base() -> Option<PathBuf> {
    invoking_user_home().map(|h| h.join(".local").join("share").join("zaprust"))
}

/// База под ядро: `<data_base>/core` (`~/.local/share/zaprust/core`).
pub(super) fn core_base() -> Option<PathBuf> {
    data_base().map(|d| d.join("core"))
}

/// Каталог конфигурации: `$XDG_CONFIG_HOME/zaprust` (фолбэк `~/.config/zaprust`).
/// `$XDG_CONFIG_HOME` honor-им только в НЕэлевированном процессе: конфиг пишет
/// исключительно GUI (единственный писатель), а под элевацией окружение недостоверно.
fn config_dir_path() -> Option<PathBuf> {
    if !elevate::is_elevated() {
        if let Some(v) = std::env::var_os("XDG_CONFIG_HOME") {
            let p = PathBuf::from(v);
            if p.is_absolute() {
                return Some(p.join("zaprust"));
            }
        }
    }
    invoking_user_home().map(|h| h.join(".config").join("zaprust"))
}

/// Каталог логов: `~/.local/state/zaprust/logs` (XDG state). Как и `data_base`,
/// детерминирован (без `$XDG_STATE_HOME`) — лог общий для GUI и root-реинвока, обе
/// стороны должны указывать в один файл; права на файл правит `chown_to_invoker`.
pub(super) fn state_log_dir() -> Option<PathBuf> {
    invoking_user_home().map(|h| h.join(".local").join("state").join("zaprust").join("logs"))
}

/// Если процесс под root и известен исходный пользователь — выставить ему владельца
/// файла/каталога (рекурсивно для каталога). Чтобы лог/ядро, созданные root-реинвоком,
/// остались доступными неэлевированному GUI. Без libc — через системный `chown`.
/// Вне root или без известного позвавшего — no-op.
pub(super) fn chown_to_invoker(path: &Path) {
    if !elevate::is_elevated() {
        return;
    }
    let Some((uid, gid)) = invoking_user_ids() else {
        return;
    };
    let spec = format!("{uid}:{gid}");
    let _ = std::process::Command::new("chown")
        .arg("-R")
        .arg(&spec)
        .arg(path)
        .status();
    // Промежуточный каталог приложения (`.../zaprust`) мог быть создан root через
    // create_dir_all и остаться root:root — вернуть и его владельца пользователю,
    // иначе под `.local/state/zaprust` останется чужой узел в дереве пользователя.
    for anc in path.ancestors().skip(1) {
        if anc.file_name().and_then(|n| n.to_str()) == Some("zaprust") {
            let _ = std::process::Command::new("chown").arg(&spec).arg(anc).status();
            break;
        }
    }
}

/// Версия ядра Linux (из `/proc/sys/kernel/osrelease`).
fn kernel_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| "?".to_owned())
}

/// Эффективный uid процесса (из `/proc/self/status`, поле `Uid:` №2).
fn euid() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|r| r.split_whitespace().nth(1))
                .map(|v| v.to_owned())
        })
        .unwrap_or_else(|| "?".to_owned())
}

impl Paths for LinuxPlatform {
    fn core_dir(&self) -> Option<PathBuf> {
        core_base().filter(|p| p.is_dir())
    }
    fn preferred_core_dir(&self) -> PathBuf {
        core_base().unwrap_or_else(|| PathBuf::from("core"))
    }
    fn config_path(&self) -> Option<PathBuf> {
        config_dir_path().map(|d| d.join("config.json"))
    }
    fn log_dirs(&self) -> Vec<PathBuf> {
        // Единственный кандидат — XDG state-каталог исходного пользователя (общий
        // для GUI и root-реинвока). Если он недоступен, логгер сам уйдёт в /tmp.
        state_log_dir().into_iter().collect()
    }
    fn fixup_owner(&self, path: &Path) {
        chown_to_invoker(path);
    }
    fn os_version(&self) -> String {
        // PRETTY_NAME из /etc/os-release, иначе общий идентификатор ОС.
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|t| {
                t.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_owned())
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| std::env::consts::OS.to_owned())
    }
    fn diag_lines(&self) -> Vec<String> {
        let mut v = Vec::new();
        v.push(format!("ядро Linux: {}", kernel_release()));
        v.push(format!("arch: {}", std::env::consts::ARCH));
        v.push(format!(
            "systemd: {}",
            if Path::new("/run/systemd/system").is_dir() { "да" } else { "нет" }
        ));
        v.push(format!(
            "фаервол: {}",
            nft::detect().map(|b| b.label()).unwrap_or_else(|| "нет nft/iptables".to_owned())
        ));
        v.push(format!("euid: {}", euid()));
        match runtime::find_nfqws() {
            Some(p) => v.push(format!("nfqws: {}", p.display())),
            None => v.push("nfqws: не найден".to_owned()),
        }
        v.push(format!(
            "правила перехвата: {}",
            match nft::rules_present() {
                Some(true) => "есть",
                Some(false) => "нет",
                None => "?(нет прав на чтение)",
            }
        ));
        // Цель pkexec-реинвока (L4/L9): внутри AppImage current_exe указывает на
        // эфемерный /tmp/.mount_*, поэтому self_exe_path предпочитает $APPIMAGE.
        // Печатаем и то, и другое — это та самая «засада элевации из AppImage».
        v.push(format!(
            "APPIMAGE: {}",
            std::env::var("APPIMAGE").unwrap_or_else(|_| "(не задан)".to_owned())
        ));
        v.push(format!(
            "цель реинвока: {}",
            elevate::self_exe_path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|e| format!("?({e})"))
        ));
        v
    }
    fn open_path(&self, path: &Path) {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
    fn integrate_desktop(&self) {
        desktop::integrate();
    }
    fn set_clipboard(&self, text: &str) -> bool {
        // Пробуем доступные утилиты буфера обмена по сессии: Wayland (`wl-copy`),
        // затем X11 (`xclip`, `xsel`). Текст подаём в stdin. Если ни одной нет —
        // вернём false (GUI сообщит, что скопировать не удалось).
        use std::io::Write;
        use std::process::{Command, Stdio};
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let mut candidates: Vec<(&str, Vec<&str>)> = Vec::new();
        if wayland {
            candidates.push(("wl-copy", vec![]));
        }
        candidates.push(("xclip", vec!["-selection", "clipboard"]));
        candidates.push(("xsel", vec!["--clipboard", "--input"]));
        if !wayland {
            candidates.push(("wl-copy", vec![]));
        }
        for (prog, args) in candidates {
            let Ok(mut child) = Command::new(prog)
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                continue; // утилиты нет в PATH — пробуем следующую
            };
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            // wl-copy форкается и держит selection в фоне — wait вернётся сразу.
            if child.wait().map(|s| s.success()).unwrap_or(false) {
                return true;
            }
        }
        false
    }
}

impl Elevator for LinuxPlatform {
    fn is_elevated(&self) -> bool {
        elevate::is_elevated()
    }
    fn run_elevated_self(&self, args: &[&str]) -> Result<i32, String> {
        // L4: реинвок самого приложения от root через pkexec (polkit-диалог).
        elevate::run_elevated_self(args)
    }
}

impl StrategySource for LinuxPlatform {
    fn scan(&self) -> CoreScan {
        // L3: курируемый набор нативных Linux-стратегий (bol-van/zapret). Движок
        // L2 (nfqws + nftables) запускает любую выбранную; отсутствие nfqws не
        // прячет список и не роняет приложение (получение ядра — L8).
        strategy::scan()
    }
}

impl ServiceController for LinuxPlatform {
    fn state(&self) -> ServiceState {
        // L5: состояние systemd-юнита (read-only, прав не требует).
        service::state()
    }
    fn installed_strategy(&self) -> Option<String> {
        // Метка стратегии — из маркера в юнит-файле (аналог реестра на Windows).
        service::installed_strategy()
    }
    fn install(&self, strategy: &str, game_filter: bool) -> Result<(), String> {
        // Валидация заранее: понятная ошибка вместо юнита, который тут же упадёт.
        if runtime::find_nfqws().is_none() {
            return Err(format!("движок не готов: {}", runtime::diag()));
        }
        let scan = strategy::scan();
        if !scan.strategies.iter().any(|s| s.name == strategy) {
            return Err(format!("стратегия не найдена: {strategy}"));
        }
        // Тело службы реинвокает наш бинарь — нужен путь к себе и uid позвавшего
        // (чтобы при загрузке найти ядро в его доме и сбросить на него привилегии).
        let exe = elevate::self_exe_path()?;
        let uid = invoking_user_ids().map(|(u, _)| u);
        service::install(&exe, strategy, game_filter, uid)
    }
    fn start(&self) -> Result<(), String> {
        service::start()
    }
    fn stop(&self) -> Result<(), String> {
        service::stop()
    }
    fn remove(&self) -> Result<(), String> {
        service::remove(runtime::force_down)
    }
    fn uninstall(&self) -> Result<(), String> {
        // На Linux нет драйвера для выгрузки (как WinDivert на Windows) — полное
        // удаление = снять службу + гарантированно подчистить правила/демон.
        service::remove(runtime::force_down)
    }
    fn reset_engine(&self) {
        // Снять любые наши правила перехвата + заглушить nfqws (аварийная чистка).
        runtime::force_down();
    }
}

impl StatusProbe for LinuxPlatform {
    fn engine_alive(&self) -> Option<u32> {
        // PID живого nfqws (читается без root через pgrep).
        status::nfqws_pid()
    }
    fn authoritative_running(&self) -> bool {
        // L6 tri-check (аналог Windows «служба RUNNING + живой winws»). На Linux
        // движок не самодостаточен, поэтому три независимых сигнала:
        //   (a) systemd-юнит active, (b) живой nfqws, (c) правила перехвата на месте.
        let svc = service::state() == ServiceState::Running;
        let pid = status::nfqws_pid();
        let nfqws = pid.is_some();
        let rules = nft::rules_present(); // Option<bool>: None = прочитать нельзя

        // Никакого тихого рассинхрона: если сигналы расходятся — ГРОМКО в лог оба
        // (как на Windows). «Юнит active, а демона нет» — это НЕ «работает».
        if svc != nfqws {
            crate::logging::warn(
                "state",
                format!("расхождение: юнит active={svc}, nfqws жив={nfqws} (pid={pid:?})"),
            );
        }
        if svc && nfqws && rules == Some(false) {
            crate::logging::warn(
                "state",
                "расхождение: юнит active + nfqws жив, но правил перехвата НЕТ",
            );
        }

        // «Работает» = юнит active И живой nfqws И (правила есть ИЛИ прочитать их
        // нельзя). Правила — мягкий третий сигнал: вето только при достоверном
        // отсутствии (Some(false)); под обычным пользователем nft часто не читается
        // (None) — тогда хватает active-юнита и живого nfqws.
        svc && nfqws && rules != Some(false)
    }
}

impl BypassRuntime for LinuxPlatform {
    fn engine_command(&self, strategy: &Strategy, game_filter: bool) -> Option<EngineCommand> {
        runtime::engine_command(strategy, game_filter)
    }
    fn engine_installed(&self) -> bool {
        runtime::find_nfqws().is_some()
    }
    fn engine_diag(&self) -> String {
        runtime::diag()
    }
    fn prepare_sweep(&self, _ordered: &[Strategy], _game_filter: bool) {
        // Страховка: если root-реинвок свипа прибьют сигналом, эфемерный nfqws и
        // его правила не повиснут на сети (Drop пробы при сигнале не отработает).
        runtime::install_sweep_guard();
        // Перед свипом убираем возможный остаток прошлого прогона (правила + демон),
        // чтобы пробы стартовали с чистого состояния.
        runtime::force_down();
    }
    fn spawn_probe(
        &self,
        strategy: &Strategy,
        game_filter: bool,
    ) -> std::io::Result<Box<dyn ProbeHandle>> {
        runtime::spawn_probe(strategy, game_filter)
    }
    fn last_engine_output(&self) -> String {
        runtime::last_engine_output()
    }
    fn run_foreground(&self, strategy: &Strategy, game_filter: bool) -> Result<(), String> {
        runtime::run_foreground(strategy, game_filter)
    }
    fn run_service(&self, strategy: &Strategy, game_filter: bool) -> Result<(), String> {
        runtime::run_service(strategy, game_filter)
    }
}

impl Tester for LinuxPlatform {
    fn agent(&self) -> ureq::Agent {
        // L6: TLS-бэкенд — rustls (см. `tester.rs`). Общий код знает только трейт.
        tester::agent()
    }
    fn probe_agent(&self) -> ureq::Agent {
        tester::probe_agent()
    }
    fn download_agent(&self) -> ureq::Agent {
        tester::download_agent()
    }
}
