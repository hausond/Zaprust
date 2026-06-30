// Управление службой обхода на Linux через systemd — аналог Windows-`service.rs`
// (sc/реестр), другой примитив. Закрепляет service-only модель: Старт = установить
// и включить юнит, Стоп = выключить и удалить юнит.
//
// Почему свой юнит, а не `zapret.service` от bol-van/zapret: на Windows Flowseal
// тоже не переиспользуется — мы прописываем winws прямо в `binPath` своей службы
// `zapret` с выбранной стратегией. На Linux симметрично: `ExecStart` реинвокает
// НАШ бинарь (`--svc-run <стратегия> <gf>`), который из L2/L3 поднимает правила
// nftables + nfqws и держит их на переднем плане, а `ExecStop`/`ExecStopPost`
// снимает. Так выбранная стратегия (и парс Flowseal `.bat`, и встроенный набор)
// остаётся единственным источником истины — мы не дублируем конфиг zapret.
//
// Юнит-файл живёт в `/etc/systemd/system/zaprust.service` (root:root, всем читаем):
//   • `state()`/`installed_strategy()` — read-only, прав не требуют (читают файл +
//     `systemctl is-active`), их зовёт неэлевированный GUI;
//   • `install`/`remove`/`start`/`stop` меняют систему и идут элевированным
//     реинвоком `--svc …` (L4, один polkit-диалог).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::logging;
use crate::platform::ServiceState;

/// Имя systemd-юнита службы обхода.
pub const UNIT_NAME: &str = "zaprust.service";

/// Маркер строки с именем стратегии внутри юнит-файла (аналог записи в реестр на
/// Windows). Единственный источник истины для `installed_strategy()`.
const STRATEGY_MARKER: &str = "# zaprust-strategy=";

/// Путь юнит-файла в системном каталоге systemd.
fn unit_path() -> PathBuf {
    PathBuf::from("/etc/systemd/system").join(UNIT_NAME)
}

/// Присутствует ли systemd как init (не у всех дистрибутивов он есть). Канонический
/// признак — рабочий каталог `/run/systemd/system`.
fn systemd_present() -> bool {
    Path::new("/run/systemd/system").is_dir()
}

/// Запустить `systemctl <args>`, вернуть Err с выводом при ненулевом коде. Вывод
/// логируется дословно (как `run_sc` на Windows — половина диагнозов там).
fn run_systemctl(args: &[&str]) -> Result<(), String> {
    let out = Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl {}: {e}", args.join(" ")))?;
    if out.status.success() {
        logging::debug("systemd", format!("systemctl {} → ok", args.join(" ")));
        Ok(())
    } else {
        let t = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let code = out.status.code().unwrap_or(-1);
        let msg = format!("systemctl {}: код={code} {}", args.join(" "), t.trim());
        logging::warn("systemd", &msg);
        Err(msg)
    }
}

/// Состояние службы (read-only, прав не требует).
///   • нет юнит-файла → NotInstalled;
///   • `systemctl is-active`: active/activating → Running; inactive/failed → Stopped.
/// В нашей service-only модели Стоп удаляет юнит целиком, поэтому Stopped — редкое
/// переходное состояние (юнит есть, но не запущен).
pub fn state() -> ServiceState {
    if !unit_path().exists() {
        return ServiceState::NotInstalled;
    }
    match Command::new("systemctl").args(["is-active", UNIT_NAME]).output() {
        Ok(o) => match String::from_utf8_lossy(&o.stdout).trim() {
            "active" | "activating" => ServiceState::Running,
            "inactive" | "failed" | "deactivating" => ServiceState::Stopped,
            _ => ServiceState::Unknown,
        },
        Err(_) => ServiceState::Unknown,
    }
}

/// Имя стратегии, с которой установлена служба (из маркера в юнит-файле). None —
/// если юнита нет или маркер отсутствует.
pub fn installed_strategy() -> Option<String> {
    let text = std::fs::read_to_string(unit_path()).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(STRATEGY_MARKER))
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Экранировать значение для double-quote синтаксиса systemd (Exec-строки умеют
/// двойные кавычки; внутри — экранируем `\` и `"`).
fn sd_quote(s: &str) -> String {
    let inner = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{inner}\"")
}

/// Сформировать текст юнита. `exe` — путь к нашему бинарю (реинвокается как тело
/// службы), `strategy`/`game_filter` — выбранная стратегия, `pkexec_uid` — uid
/// ПОЗВАВШЕГО пользователя (под systemd при загрузке нет SUDO_USER/PKEXEC_UID,
/// поэтому пробрасываем его явно в `Environment` — тогда `invoking_user_home/ids`
/// при старте найдут ядро в его домашнем каталоге и nfqws сбросит привилегии на
/// него, а не на /root/nobody).
fn render_unit(exe: &Path, strategy: &str, game_filter: bool, pkexec_uid: Option<u32>) -> String {
    let exe_q = sd_quote(&exe.display().to_string());
    let strat_q = sd_quote(strategy);
    let gf = if game_filter { "1" } else { "0" };
    let env_line = match pkexec_uid {
        Some(uid) => format!("Environment=PKEXEC_UID={uid}\n"),
        None => String::new(),
    };
    format!(
        "# Сгенерировано Zaprust — не редактировать вручную (перезапишется при Старте).\n\
         {STRATEGY_MARKER}{strategy}\n\
         [Unit]\n\
         Description=Zaprust DPI bypass (nfqws + nftables)\n\
         Documentation=https://github.com/hausond/Zaprust\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         {env_line}\
         ExecStart={exe_q} --svc-run {strat_q} {gf}\n\
         ExecStopPost={exe_q} --engine-down\n\
         KillMode=mixed\n\
         Restart=on-failure\n\
         RestartSec=3\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

/// Установить и запустить службу с выбранной стратегией (требует root). Идемпотентно:
/// перезаписывает юнит и `restart`-ит — это же и обработка смены стратегии при
/// активной службе. `enable` ставит boot-symlink (автозапуск переживает перезагрузку).
pub fn install(
    exe: &Path,
    strategy: &str,
    game_filter: bool,
    pkexec_uid: Option<u32>,
) -> Result<(), String> {
    if !systemd_present() {
        return Err("systemd не обнаружен (/run/systemd/system отсутствует) — служба недоступна на этом init".to_owned());
    }

    let unit = render_unit(exe, strategy, game_filter, pkexec_uid);
    std::fs::write(unit_path(), unit).map_err(|e| format!("запись юнита {}: {e}", unit_path().display()))?;
    logging::info(
        "systemd",
        format!("юнит записан: {} · стратегия={strategy} · gf={game_filter} · uid={pkexec_uid:?}", unit_path().display()),
    );

    run_systemctl(&["daemon-reload"])?;
    // enable — автозапуск при загрузке; restart — старт (или перезапуск с новой
    // стратегией, если служба уже работала). restart покрывает оба случая.
    run_systemctl(&["enable", UNIT_NAME])?;
    run_systemctl(&["restart", UNIT_NAME])?;
    Ok(())
}

/// Остановить, выключить и удалить службу (требует root). Идемпотентно: если юнита
/// нет — просто аварийная чистка движка. `ExecStopPost` снимет правила, но на
/// всякий случай добиваем `force_down` (вдруг процесс умер по SIGKILL).
pub fn remove(force_down: impl Fn()) -> Result<(), String> {
    if !unit_path().exists() {
        // Юнита нет — снять возможный остаток движка и выйти успешно.
        force_down();
        return Ok(());
    }

    // disable --now: остановить + убрать boot-symlink одной командой.
    let disabled = run_systemctl(&["disable", "--now", UNIT_NAME]);
    if let Err(e) = &disabled {
        logging::warn("systemd", format!("disable --now не прошёл (продолжаю удаление): {e}"));
    }

    // Удаляем сам юнит-файл и перечитываем (без него systemctl его «забудет»).
    if let Err(e) = std::fs::remove_file(unit_path()) {
        logging::warn("systemd", format!("удаление юнит-файла: {e}"));
    }
    let _ = run_systemctl(&["daemon-reload"]);
    // Подчищаем неудачный статус, если он остался от падения.
    let _ = run_systemctl(&["reset-failed", UNIT_NAME]);

    // Гарантируем, что правила перехвата и nfqws точно сняты.
    force_down();
    Ok(())
}

/// Запустить уже установленную службу (требует root).
pub fn start() -> Result<(), String> {
    if !unit_path().exists() {
        return Err("служба не установлена (нет юнита) — сначала Старт/install".to_owned());
    }
    run_systemctl(&["start", UNIT_NAME])
}

/// Остановить службу, не удаляя юнит (требует root). `ExecStopPost` снимет правила.
pub fn stop() -> Result<(), String> {
    if !unit_path().exists() {
        return Ok(());
    }
    run_systemctl(&["stop", UNIT_NAME])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn unit_contains_strategy_marker_and_exec() {
        let exe = PathBuf::from("/opt/zaprust/zaprust");
        let unit = render_unit(&exe, "general (ALT12)", true, Some(1000));
        // Маркер стратегии присутствует и парсится обратно.
        let parsed = unit
            .lines()
            .find_map(|l| l.strip_prefix(STRATEGY_MARKER))
            .map(|v| v.trim().to_owned());
        assert_eq!(parsed.as_deref(), Some("general (ALT12)"));
        // ExecStart реинвокает наш бинарь в режиме тела службы с gf=1.
        assert!(unit.contains("--svc-run \"general (ALT12)\" 1"), "unit:\n{unit}");
        // Проброшен uid позвавшего (для поиска ядра при загрузке).
        assert!(unit.contains("Environment=PKEXEC_UID=1000"));
        // Автозапуск + зависимость от сети.
        assert!(unit.contains("WantedBy=multi-user.target"));
        assert!(unit.contains("After=network-online.target"));
        // Снятие правил гарантировано даже при падении.
        assert!(unit.contains("ExecStopPost=") && unit.contains("--engine-down"));
    }

    #[test]
    fn unit_without_uid_omits_environment() {
        let exe = PathBuf::from("/usr/bin/zaprust");
        let unit = render_unit(&exe, "general", false, None);
        assert!(!unit.contains("PKEXEC_UID"));
        assert!(unit.contains("--svc-run \"general\" 0"));
    }

    #[test]
    fn sd_quote_escapes_quotes_and_backslashes() {
        assert_eq!(sd_quote("plain"), "\"plain\"");
        assert_eq!(sd_quote(r#"a"b"#), "\"a\\\"b\"");
        assert_eq!(sd_quote(r"a\b"), "\"a\\\\b\"");
    }
}
