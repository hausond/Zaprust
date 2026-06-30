// Элевация на Linux: реинвок самого приложения через `pkexec` (polkit) и
// проверка текущего процесса на root (euid==0).
//
// Аналог виндового `windows/elevate.rs` (ShellExecuteExW("runas")), но примитив
// другой. pkexec запускает указанную программу от root и показывает GUI-диалог
// аутентификации (через polkit-агент сессии — DISPLAY/окружение нам передавать
// не нужно). Хэндшейк результата — файловый, как на Windows: элевированный
// реинвок пишет result-файл в общедоступный /tmp, а неэлевированный GUI читает
// его после завершения процесса (см. `main::op_result_path`). Всё, что нужно
// реинвоку, передаём АРГУМENTАМИ — pkexec вычищает окружение, на env полагаться
// нельзя.

use std::path::PathBuf;
use std::process::Command;

use crate::logging;

/// Запущены ли мы от root (euid == 0).
///
/// Читаем эффективный uid из `/proc/self/status` (строка `Uid: real eff …`,
/// второе поле — эффективный), чтобы не плодить процесс и не тянуть libc. Под
/// pkexec реальный и эффективный uid оба 0. Фолбэк — `id -u`.
pub fn is_elevated() -> bool {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(euid) = status
            .lines()
            .find_map(|l| l.strip_prefix("Uid:"))
            .and_then(|rest| rest.split_whitespace().nth(1))
        {
            return euid == "0";
        }
    }
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim() == "0")
        .unwrap_or(false)
}

/// Абсолютный путь к собственному исполняемому файлу для реинвока.
///
/// Внутри AppImage `current_exe()` указывает на смонтированный squashfs во
/// временной папке (`/tmp/.mount_*/…`) — он живёт только пока процесс запущен,
/// поэтому для повторного запуска предпочитаем `$APPIMAGE` (канонический путь к
/// самому .AppImage). Вне AppImage берём `current_exe()` и канонизируем.
pub fn self_exe_path() -> Result<PathBuf, String> {
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        let p = PathBuf::from(appimage);
        if p.is_file() {
            return Ok(p);
        }
    }
    let exe = std::env::current_exe().map_err(|e| format!("не удалось определить путь к себе: {e}"))?;
    // canonicalize резолвит симлинки и относительные сегменты; при неудаче
    // (редко) отдаём как есть — пути из current_exe и так абсолютны.
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// Найти `pkexec` в PATH (а также в типичных системных каталогах на случай
/// урезанного PATH под некоторыми лаунчерами).
fn find_pkexec() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':').filter(|d| !d.is_empty()) {
            let cand = PathBuf::from(dir).join("pkexec");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    for p in ["/usr/bin/pkexec", "/bin/pkexec", "/usr/local/bin/pkexec"] {
        let cand = PathBuf::from(p);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Перезапустить наш бинарь от root через pkexec с подкомандой и дождаться
/// кода выхода. Возвращает код выхода реинвока (как на Windows — итог операции
/// дополнительно подтверждается result-файлом в /tmp).
///
/// Различаем исходы:
///   • pkexec недоступен (polkit не установлен) — отдельная ошибка;
///   • rc=126 — не авторизован / диалог отменён пользователем;
///   • rc=127 — pkexec не смог запустить процесс;
///   • убит сигналом без кода — реинвок умер, result-файла может не быть;
///   • иначе — пробрасываем код выхода реинвока.
pub fn run_elevated_self(args: &[&str]) -> Result<i32, String> {
    use std::os::unix::process::ExitStatusExt;

    let Some(pkexec) = find_pkexec() else {
        let msg = "pkexec недоступен (polkit не установлен?)".to_owned();
        logging::error("elevate", &msg);
        return Err(msg);
    };
    let exe = self_exe_path()?;

    logging::info(
        "elevate",
        format!("вызываю pkexec для: {} {}", exe.display(), args.join(" ")),
    );

    // pkexec <путь-к-себе> <args…>. Окружение НЕ прокидываем — pkexec его
    // вычищает; всё необходимое реинвок получает аргументами.
    let mut cmd = Command::new(&pkexec);
    cmd.arg(&exe);
    cmd.args(args);

    let status = match cmd.status() {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("не удалось запустить pkexec: {e}");
            logging::error("elevate", &msg);
            return Err(msg);
        }
    };

    if let Some(code) = status.code() {
        match code {
            126 => {
                let msg = "элевация отклонена/не авторизована (pkexec rc=126)".to_owned();
                logging::warn("elevate", &msg);
                Err(msg)
            }
            127 => {
                let msg = "pkexec не смог запустить реинвок (rc=127)".to_owned();
                logging::error("elevate", &msg);
                Err(msg)
            }
            c => {
                logging::info("elevate", format!("реинвок завершён, код={c}"));
                Ok(c)
            }
        }
    } else {
        // Убит сигналом — кода выхода нет, result-файл мог не записаться.
        let sig = status.signal().unwrap_or(0);
        let msg = format!("элевированный реинвок убит сигналом {sig} (без кода выхода)");
        logging::error("elevate", &msg);
        Err(msg)
    }
}
