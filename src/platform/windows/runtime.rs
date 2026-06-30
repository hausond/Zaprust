// Движок обхода на Windows: построение команды winws, одиночные прогоны для
// проб автоподбора и подготовка свипа (сброс драйвера WinDivert, прайм).
//
// На Windows весь перехват трафика задаётся аргументами winws (WinDivert-фильтр
// внутри них), поэтому отдельных правил фаервола тут нет — в отличие от Linux,
// где движок (nfqws) сам ничего не перехватывает и правила nftables живут рядом.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::{service, strategy};
use crate::logging;
use crate::platform::{EngineCommand, ProbeHandle};
use crate::strategies::Strategy;

/// Команда запуска winws для стратегии: exe в core/bin, аргументы с разрешённым
/// Game Filter. None — если ядро не найдено.
pub fn engine_command(strategy: &Strategy, game_filter: bool) -> Option<EngineCommand> {
    let core_dir = strategy::find_core_dir()?;
    let bin = core_dir.join("bin");
    Some(EngineCommand {
        program: bin.join("winws.exe"),
        cwd: bin,
        args: strategy::resolve_game_filter(&strategy.args, game_filter),
    })
}

/// Создать недостающие пользовательские списки (`*-user.txt`), на которые ссылаются
/// аргументы winws. Оригинальные `.bat` Flowseal создают их пустыми через
/// `service.bat load_user_lists`; мы запускаем winws напрямую, поэтому делаем сами.
/// Без этого winws на отсутствующем `--ipset-exclude=…-user.txt` падает с кодом 1.
pub fn ensure_user_lists(args: &[String]) {
    for a in args {
        let path = a.rsplit('=').next().unwrap_or(a).trim_matches('"');
        if !path.ends_with("-user.txt") {
            continue;
        }
        let p = std::path::Path::new(path);
        if p.exists() {
            continue;
        }
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(p, b"") {
            Ok(_) => logging::info("lists", format!("создан недостающий список: {path}")),
            Err(e) => logging::warn("lists", format!("не создать список {path}: {e}")),
        }
    }
}

/// Файл, куда пишется вывод winws при пробах (для диагностики кода выхода).
fn winws_output_path() -> std::path::PathBuf {
    std::env::temp_dir().join("zaprust_winws.out")
}

/// Последние строки вывода winws (его собственная диагностика об ошибке).
pub fn last_engine_output() -> String {
    std::fs::read_to_string(winws_output_path())
        .map(|t| {
            t.lines()
                .filter(|l| !l.trim().is_empty())
                .rev()
                .take(12)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_default()
}

/// Спавн winws скрыто (CREATE_NO_WINDOW). stdout+stderr → temp-файл (безопасно,
/// в отличие от недренируемого пайпа), чтобы видеть причину кода выхода winws.
pub fn spawn_winws(core_dir: &Path, args: &[String]) -> std::io::Result<Child> {
    let bin_dir = core_dir.join("bin");
    let exe = bin_dir.join("winws.exe");
    let mut cmd = Command::new(&exe);
    cmd.args(args).current_dir(&bin_dir).stdin(Stdio::null());
    match std::fs::File::create(winws_output_path()) {
        Ok(file) => {
            let err = file.try_clone().map(Stdio::from).unwrap_or_else(|_| Stdio::null());
            cmd.stdout(Stdio::from(file)).stderr(err);
        }
        Err(_) => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.spawn()
}

/// Поднять winws одиночным прогоном для пробы стратегии.
pub fn spawn_probe(
    strategy: &Strategy,
    game_filter: bool,
) -> std::io::Result<Box<dyn ProbeHandle>> {
    let core_dir = strategy::find_core_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "ядро не найдено"))?;
    let args = strategy::resolve_game_filter(&strategy.args, game_filter);
    let child = spawn_winws(&core_dir, &args)?;
    Ok(Box::new(WinwsProbe { child }))
}

/// Хэндл прогона winws: при Drop глушит процесс (драйвер WinDivert НЕ выгружаем —
/// его сбрасывают раз на весь свип в prepare_sweep).
struct WinwsProbe {
    child: Child,
}

impl ProbeHandle for WinwsProbe {
    fn try_exit(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
            _ => None,
        }
    }
}

impl Drop for WinwsProbe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Подготовка к свипу автоподбора (под админом): создать недостающие списки для
/// всех стратегий, снять активную службу/winws, сбросить драйвер WinDivert и
/// прогреть его одним прогоном winws — иначе первые пробы падают из-за
/// непрогретого/занятого драйвера.
pub fn prepare_sweep(ordered: &[Strategy], game_filter: bool) {
    // Создаём недостающие *-user.txt для всех стратегий: иначе winws каждой пробы
    // падает с кодом 1 на отсутствующем ipset-файле (на свежем ядре их нет).
    for s in ordered {
        ensure_user_lists(&strategy::resolve_game_filter(&s.args, game_filter));
    }

    // КРИТИЧНО: снимаем уже стоящую службу/процесс winws перед подбором — иначе
    // их winws держит WinDivert, и winws КАЖДОЙ пробы конфликтует и падает (код 1).
    if service::query(service::SERVICE_NAME).installed() || service::winws_alive().is_some() {
        logging::info("autoselect", "снимаю активную службу/winws перед подбором");
        let _ = service::remove();
        std::thread::sleep(Duration::from_millis(600));
    }
    // И сбрасываем сам драйвер WinDivert: прошлый winws (в т.ч. упавший/убитый при
    // ручном старте) мог оставить драйвер в занятом состоянии — тогда КАЖДЫЙ
    // следующий winws падает с кодом 1. Чистый старт драйвера снимает это.
    logging::info("autoselect", "сброс драйвера WinDivert перед подбором");
    service::stop_driver();
    std::thread::sleep(Duration::from_millis(800));

    let Some(core_dir) = strategy::find_core_dir() else {
        return;
    };

    // Прайм драйвера WinDivert: на чистой машине ПЕРВЫЙ запуск winws ставит/грузит
    // драйвер заметно дольше прогрева — поднимаем winws один раз и ждём, иначе
    // первые (а то и все) пробы падают из-за непрогретого драйвера. Заодно ловим
    // вывод winws — если он сразу падает, тут будет видна настоящая причина.
    if let Some(first) = ordered.first() {
        let pargs = strategy::resolve_game_filter(&first.args, game_filter);
        match spawn_winws(&core_dir, &pargs) {
            Ok(mut c) => {
                logging::info("autoselect", "прайм WinDivert: winws поднят, жду 1800мс");
                std::thread::sleep(Duration::from_millis(1800));
                let early = c.try_wait().ok().flatten();
                let _ = c.kill();
                let _ = c.wait();
                std::thread::sleep(Duration::from_millis(300));
                let out = last_engine_output();
                match early {
                    Some(st) => logging::error(
                        "autoselect",
                        format!(
                            "прайм: winws сразу вышел (код {:?}){}",
                            st.code(),
                            if out.is_empty() { String::new() } else { format!(" · winws: {out}") }
                        ),
                    ),
                    None => logging::info(
                        "autoselect",
                        format!(
                            "прайм завершён, WinDivert: {:?}{}",
                            service::query("WinDivert"),
                            if out.is_empty() { String::new() } else { format!(" · winws: {out}") }
                        ),
                    ),
                }
            }
            Err(e) => logging::error("autoselect", format!("прайм: winws не запустился: {e}")),
        }
    }
}

/// Сборка аргументов выбранной стратегии и установка службы (под админом).
/// Воспроизводит прежний `install_service_elevated` из main.rs дословно.
pub fn install_service(strategy_name: &str, game_filter: bool) -> Result<(), String> {
    let scan = strategy::scan();
    let core_dir = scan.core_dir.ok_or_else(|| "ядро не найдено".to_owned())?;
    let strategy = scan
        .strategies
        .iter()
        .find(|s| s.name == strategy_name)
        .ok_or_else(|| format!("стратегия не найдена: {strategy_name}"))?;

    let exe = core_dir.join("bin").join("winws.exe");
    if !exe.exists() {
        return Err(format!("не найден {}", exe.display()));
    }
    let args = strategy::resolve_game_filter(&strategy.args, game_filter);
    ensure_user_lists(&args);

    // Сбрасываем драйвер WinDivert перед установкой: прошлый winws (упавший/убитый,
    // в т.ч. после неудачного автоподбора) мог оставить драйвер занятым — тогда
    // winws службы стартует и сразу падает (код 1), служба остаётся Stopped.
    logging::info("svc", "сброс драйвера WinDivert перед установкой службы");
    service::stop_driver();
    std::thread::sleep(Duration::from_millis(800));

    let res = service::install(&exe, &args, strategy_name);

    // Если служба установилась, но winws не поднялся — снимаем собственный вывод
    // winws (запуск напрямую, с захватом stdout/stderr), чтобы увидеть причину.
    if res.is_ok() && service::winws_alive().is_none() {
        logging::error("svc", "служба установлена, но winws не запущен — диагностический прогон winws");
        match spawn_winws(&core_dir, &args) {
            Ok(mut c) => {
                std::thread::sleep(Duration::from_millis(2000));
                let early = c.try_wait().ok().flatten();
                let _ = c.kill();
                let _ = c.wait();
                let out = last_engine_output();
                logging::error(
                    "svc",
                    format!(
                        "диагностика winws: {}{}",
                        match early {
                            Some(st) => format!("вышел код {:?}", st.code()),
                            None => "остался жив при прямом запуске (проблема в режиме службы)".to_owned(),
                        },
                        if out.is_empty() { String::new() } else { format!(" · winws: {out}") }
                    ),
                );
            }
            Err(e) => logging::error("svc", format!("диагностика: winws не запустился: {e}")),
        }
    }

    res
}
