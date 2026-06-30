// Управление службой Windows для winws (воспроизводим подход Flowseal:
// служба `zapret`, winws.exe прописывается напрямую в binPath со start=auto).
//
// Все функции, кроме query(), меняют систему и требуют прав администратора —
// их вызывает элевированный экземпляр приложения (режим `--svc`).

use std::path::Path;
use std::process::Command;

use crate::platform::ServiceState;

/// Имя службы (как у Flowseal).
pub const SERVICE_NAME: &str = "zapret";

fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
}

/// Состояние службы по имени (read-only, прав админа не требует).
pub fn query(name: &str) -> ServiceState {
    let mut cmd = Command::new("sc");
    cmd.args(["query", name]);
    no_window(&mut cmd);
    match cmd.output() {
        Ok(out) => {
            // Токены sc английские независимо от локали.
            let t = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
            .to_uppercase();
            if t.contains("RUNNING") {
                ServiceState::Running
            } else if t.contains("STOPPED") || t.contains("_PENDING") {
                ServiceState::Stopped
            } else if t.contains("1060") || t.contains("DOES NOT EXIST") {
                ServiceState::NotInstalled
            } else {
                ServiceState::Unknown
            }
        }
        Err(_) => ServiceState::Unknown,
    }
}

/// Запустить `sc` с аргументами, вернуть Err с текстом при ненулевом коде.
/// Вывод sc (stdout+stderr+код) логируется дословно — половина диагнозов там.
fn run_sc(args: &[&str]) -> Result<(), String> {
    let mut cmd = Command::new("sc");
    cmd.args(args);
    no_window(&mut cmd);
    match cmd.output() {
        Ok(out) if out.status.success() => {
            crate::logging::debug("sc", format!("sc {} → ok", args.join(" ")));
            Ok(())
        }
        Ok(out) => {
            let t = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let code = out.status.code().unwrap_or(-1);
            let msg = format!("sc {}: код={code} {}", args.join(" "), t.trim());
            crate::logging::warn("sc", &msg);
            Err(msg)
        }
        Err(e) => {
            let msg = format!("sc {}: {e}", args.join(" "));
            crate::logging::error("sc", &msg);
            Err(msg)
        }
    }
}

/// Включить TCP timestamps (как `tcp_enable` у Flowseal — нужно части стратегий).
fn tcp_enable() {
    let mut cmd = Command::new("netsh");
    cmd.args(["interface", "tcp", "set", "global", "timestamps=enabled"]);
    no_window(&mut cmd);
    let _ = cmd.output();
}

/// Собрать значение binPath: "exe" arg1 "arg with space" …
fn build_bin_path(exe: &Path, args: &[String]) -> String {
    let mut s = format!("\"{}\"", exe.display());
    for a in args {
        s.push(' ');
        if a.is_empty() || a.contains(' ') || a.contains('"') {
            s.push('"');
            s.push_str(&a.replace('"', "\\\""));
            s.push('"');
        } else {
            s.push_str(a);
        }
    }
    s
}

/// Установить службу: создать со start=auto, разрешить не-админам start/stop,
/// запустить, записать метку стратегии в реестр. Требует прав администратора.
pub fn install(exe: &Path, args: &[String], strategy_label: &str) -> Result<(), String> {
    // Не плодим дубли: чистим прежнюю (best-effort).
    let _ = remove();

    let bin_path = build_bin_path(exe, args);
    // Лимит командной строки службы.
    if bin_path.len() > 8000 {
        return Err(format!(
            "командная строка службы слишком длинная: {} символов (лимит ~8191)",
            bin_path.len()
        ));
    }

    tcp_enable();

    run_sc(&[
        "create",
        SERVICE_NAME,
        "binPath=",
        &bin_path,
        "DisplayName=",
        "zapret (Zaprust)",
        "start=",
        "auto",
    ])?;
    let _ = run_sc(&["description", SERVICE_NAME, "Zapret DPI bypass (Zaprust)"]);

    // Разрешаем интерактивным пользователям start/stop — чтобы GUI управлял
    // службой без повторного UAC (UAC только на установку/удаление).
    let _ = run_sc(&[
        "sdset",
        SERVICE_NAME,
        "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWRPWPLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)",
    ]);

    start()?;
    let _ = write_reg_marker(strategy_label);
    Ok(())
}

/// Удалить службу: остановить, удалить, добить winws, почистить WinDivert.
pub fn remove() -> Result<(), String> {
    let mut net = Command::new("net");
    net.args(["stop", SERVICE_NAME]);
    no_window(&mut net);
    let _ = net.output();

    run_sc(&["delete", SERVICE_NAME])?;

    let mut tk = Command::new("taskkill");
    tk.args(["/IM", "winws.exe", "/F"]);
    no_window(&mut tk);
    let _ = tk.output();
    Ok(())
}

/// Выгрузить драйвер WinDivert, чтобы освободить WinDivert64.sys/.dll для замены.
/// winws при следующем старте поставит/загрузит драйвер заново.
pub fn stop_driver() {
    for name in ["WinDivert", "WinDivert14"] {
        let mut net = Command::new("net");
        net.args(["stop", name]);
        no_window(&mut net);
        let _ = net.output();

        let mut del = Command::new("sc");
        del.args(["delete", name]);
        no_window(&mut del);
        let _ = del.output();
    }
}

/// PID живого процесса winws.exe (через tasklist), None — если не запущен.
pub fn winws_alive() -> Option<u32> {
    let mut cmd = Command::new("tasklist");
    cmd.args(["/FI", "IMAGENAME eq winws.exe", "/NH", "/FO", "CSV"]);
    no_window(&mut cmd);
    let out = cmd.output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if line.to_lowercase().contains("winws.exe") {
            // CSV: "winws.exe","1234","Console",...
            let pid = line
                .split(',')
                .nth(1)
                .map(|s| s.trim_matches(|c| c == '"' || c == ' '))
                .and_then(|s| s.parse::<u32>().ok());
            return pid.or(Some(0));
        }
    }
    None
}

pub fn start() -> Result<(), String> {
    run_sc(&["start", SERVICE_NAME])
}

pub fn stop() -> Result<(), String> {
    run_sc(&["stop", SERVICE_NAME])
}

/// Имя стратегии, с которой установлена служба (из реестра). None — если нет.
pub fn installed_strategy() -> Option<String> {
    let mut cmd = Command::new("reg");
    cmd.args([
        "query",
        &format!("HKLM\\System\\CurrentControlSet\\Services\\{SERVICE_NAME}"),
        "/v",
        "zapret-discord-youtube",
    ]);
    no_window(&mut cmd);
    let out = cmd.output().ok()?;
    parse_reg_sz(&String::from_utf8_lossy(&out.stdout))
}

/// Достать значение REG_SZ из вывода `reg query` (строка вида
/// "    zapret-discord-youtube    REG_SZ    general (ALT10)").
fn parse_reg_sz(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(pos) = line.find("REG_SZ") {
            let val = line[pos + "REG_SZ".len()..].trim();
            if !val.is_empty() {
                return Some(val.to_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reg_sz_value_with_spaces() {
        let out = "\r\nHKEY_LOCAL_MACHINE\\System\\CurrentControlSet\\Services\\zapret\r\n    zapret-discord-youtube    REG_SZ    general (ALT10)\r\n";
        assert_eq!(parse_reg_sz(out).as_deref(), Some("general (ALT10)"));
        assert_eq!(parse_reg_sz("no value here"), None);
    }
}

fn write_reg_marker(label: &str) -> Result<(), String> {
    let mut cmd = Command::new("reg");
    cmd.args([
        "add",
        &format!("HKLM\\System\\CurrentControlSet\\Services\\{SERVICE_NAME}"),
        "/v",
        "zapret-discord-youtube",
        "/t",
        "REG_SZ",
        "/d",
        label,
        "/f",
    ]);
    no_window(&mut cmd);
    cmd.output().map(|_| ()).map_err(|e| e.to_string())
}
