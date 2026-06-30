// Windows-реализация платформенного слоя. Все трейты `platform::*` сведены на
// один тип `WindowsPlatform`; методы делегируют в подмодули, куда переехал
// существующий код без изменения поведения.

mod elevate;
mod runtime;
mod service;
mod strategy;
mod sys;
mod tester;

use std::path::{Path, PathBuf};

use crate::logging;
use crate::platform::{
    BypassRuntime, Elevator, EngineCommand, Paths, Platform, ProbeHandle, ServiceController,
    ServiceState, StatusProbe, StrategySource, Tester,
};
use crate::strategies::{CoreScan, Strategy};

pub struct WindowsPlatform;

impl WindowsPlatform {
    pub fn new() -> Self {
        WindowsPlatform
    }
}

impl Platform for WindowsPlatform {}

impl Paths for WindowsPlatform {
    fn core_dir(&self) -> Option<PathBuf> {
        strategy::find_core_dir()
    }
    fn preferred_core_dir(&self) -> PathBuf {
        strategy::preferred_core_dir()
    }
    fn config_path(&self) -> Option<PathBuf> {
        sys::config_path()
    }
    fn log_dirs(&self) -> Vec<PathBuf> {
        sys::log_dirs()
    }
    fn fixup_owner(&self, _path: &Path) {
        // На Windows GUI и элевированные реинвоки — процессы одного пользователя,
        // смена владельца файла не нужна.
    }
    fn os_version(&self) -> String {
        sys::os_version()
    }
    fn diag_lines(&self) -> Vec<String> {
        sys::diag_lines()
    }
    fn open_path(&self, path: &Path) {
        sys::open_path(path)
    }
    fn set_clipboard(&self, text: &str) -> bool {
        sys::set_clipboard(text)
    }
}

impl Elevator for WindowsPlatform {
    fn is_elevated(&self) -> bool {
        elevate::is_elevated()
    }
    fn run_elevated_self(&self, args: &[&str]) -> Result<i32, String> {
        elevate::run_elevated_self(args)
    }
}

impl StrategySource for WindowsPlatform {
    fn scan(&self) -> CoreScan {
        strategy::scan()
    }
}

impl ServiceController for WindowsPlatform {
    fn state(&self) -> ServiceState {
        service::query(service::SERVICE_NAME)
    }
    fn installed_strategy(&self) -> Option<String> {
        service::installed_strategy()
    }
    fn install(&self, strategy: &str, game_filter: bool) -> Result<(), String> {
        runtime::install_service(strategy, game_filter)
    }
    fn start(&self) -> Result<(), String> {
        service::start()
    }
    fn stop(&self) -> Result<(), String> {
        service::stop()
    }
    fn remove(&self) -> Result<(), String> {
        service::remove()
    }
    fn uninstall(&self) -> Result<(), String> {
        let _ = service::remove(); // удалить службу (если есть)
        service::stop_driver(); // выгрузить WinDivert/WinDivert14
        Ok(())
    }
    fn reset_engine(&self) {
        service::stop_driver();
    }
}

impl StatusProbe for WindowsPlatform {
    fn engine_alive(&self) -> Option<u32> {
        service::winws_alive()
    }
    fn authoritative_running(&self) -> bool {
        let svc = service::query(service::SERVICE_NAME) == ServiceState::Running;
        let winws = service::winws_alive().is_some();
        if svc != winws {
            logging::warn(
                "state",
                format!("расхождение: служба RUNNING={svc}, winws жив={winws}"),
            );
        }
        svc && winws
    }
}

impl BypassRuntime for WindowsPlatform {
    fn engine_command(&self, strategy: &Strategy, game_filter: bool) -> Option<EngineCommand> {
        runtime::engine_command(strategy, game_filter)
    }
    fn engine_installed(&self) -> bool {
        strategy::find_core_dir()
            .map(|d| {
                let bin = d.join("bin");
                bin.join("winws.exe").exists()
                    && bin.join("WinDivert.dll").exists()
                    && bin.join("WinDivert64.sys").exists()
            })
            .unwrap_or(false)
    }
    fn engine_diag(&self) -> String {
        match strategy::find_core_dir() {
            Some(core) => {
                let bin = core.join("bin");
                format!(
                    "winws.exe={} WinDivert.dll={} WinDivert64.sys={} version={}",
                    bin.join("winws.exe").exists(),
                    bin.join("WinDivert.dll").exists(),
                    bin.join("WinDivert64.sys").exists(),
                    crate::updater::local_version(&core).unwrap_or_else(|| "нет".to_owned())
                )
            }
            None => "ядро не найдено".to_owned(),
        }
    }
    fn prepare_sweep(&self, ordered: &[Strategy], game_filter: bool) {
        runtime::prepare_sweep(ordered, game_filter);
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
    fn run_foreground(&self, _strategy: &Strategy, _game_filter: bool) -> Result<(), String> {
        // На Windows движок работает только службой (winws-as-service); отдельного
        // foreground-режима нет. Старт — через `--svc install`.
        Err("foreground-прогон движка только для Linux; на Windows движок работает службой".to_owned())
    }
    fn run_service(&self, _strategy: &Strategy, _game_filter: bool) -> Result<(), String> {
        // На Windows winws прописан прямо в binPath службы (winws-as-service) —
        // SCM запускает его сам, отдельного тела `ExecStart` нет (это специфика
        // systemd на Linux). Старт — через `--svc install`.
        Err("run_service только для Linux (systemd); на Windows winws запускает SCM".to_owned())
    }
}

impl Tester for WindowsPlatform {
    fn agent(&self) -> ureq::Agent {
        tester::agent()
    }
    fn probe_agent(&self) -> ureq::Agent {
        tester::probe_agent()
    }
    fn download_agent(&self) -> ureq::Agent {
        tester::download_agent()
    }
}
