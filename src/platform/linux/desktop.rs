// Desktop-интеграция (Linux): регистрируем `.desktop`-entry и иконку приложения
// в пользовательских XDG-каталогах, чтобы окружение рисовало иконку приложения
// в доке/таскбаре, а не обобщённую заглушку («шестерёнку»).
//
// Зачем это нужно именно так: на Wayland (GNOME/KDE) окружение сопоставляет окно
// с `.desktop`-файлом ПО app_id (мы выставили `with_app_id("zaprust")` в
// ViewportBuilder), а вшитую в окно иконку (`_NET_WM_ICON`-аналог) ИГНОРИРУЕТ.
// Поэтому, пока в системе нет `zaprust.desktop` с `Icon=zaprust`, среда не знает,
// какую иконку показать, и подставляет дефолтную. Установка идёт в каталоги
// текущего пользователя — root не нужен.

use std::path::{Path, PathBuf};

use crate::logging;

// Иконки вшиты в бинарь (тот же источник, что и иконка окна). 256px-растр + SVG:
// растр гарантированно подхватывается, SVG даёт чёткость на hidpi.
const ICON_PNG_256: &[u8] = include_bytes!("../../../assets/icons/icon-app-256.png");
const ICON_SVG: &[u8] = include_bytes!("../../../assets/icons/icon-app.svg");

/// Имя — совпадает с app_id окна и базовым именем `.desktop` (по нему Wayland
/// сопоставляет окно с entry).
const APP_ID: &str = "zaprust";

/// Базовый каталог данных XDG (`$XDG_DATA_HOME` или `~/.local/share`).
fn data_home() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
        let p = PathBuf::from(d);
        if p.is_absolute() {
            return Some(p);
        }
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
}

/// Записать файл, только если содержимое отличается (чтобы не дёргать диск и
/// файловые мониторы среды на каждом старте). Создаёт родительские каталоги.
/// Возвращает true, если файл был создан/обновлён.
fn write_if_changed(path: &Path, bytes: &[u8]) -> bool {
    if std::fs::read(path).map(|cur| cur == bytes).unwrap_or(false) {
        return false; // уже актуально
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            logging::warn("desktop", format!("не создать {}: {e}", parent.display()));
            return false;
        }
    }
    match std::fs::write(path, bytes) {
        Ok(()) => {
            logging::info("desktop", format!("записан {}", path.display()));
            true
        }
        Err(e) => {
            logging::warn("desktop", format!("не записать {}: {e}", path.display()));
            false
        }
    }
}

/// Путь для запуска из `.desktop`. Внутри AppImage это `$APPIMAGE` (а не
/// эфемерный `/tmp/.mount_*`), вне — канонический путь к бинарю. Переиспользуем
/// ту же логику, что и pkexec-реинвок (elevate::self_exe_path).
fn exec_path() -> Option<PathBuf> {
    super::elevate::self_exe_path().ok()
}

/// Собрать содержимое `zaprust.desktop`. Путь в `Exec` оборачиваем в кавычки —
/// он может содержать пробелы (по спецификации .desktop так и положено).
fn desktop_entry(exec: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Zaprust\n\
         GenericName=DPI bypass\n\
         Comment=GUI для обхода DPI-блокировок (поверх zapret)\n\
         Comment[en]=Lightweight GUI for DPI bypass (zapret)\n\
         Exec=\"{exec}\"\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Network;\n\
         Keywords=dpi;zapret;nfqws;bypass;\n\
         StartupWMClass={APP_ID}\n",
        exec = exec.display(),
    )
}

/// Best-effort обновить кэш desktop-файлов, чтобы среда увидела новый entry
/// быстрее. Ошибки игнорируем (на Wayland GNOME ловит изменения файловым
/// монитором и так).
fn refresh_caches(app_dir: &Path) {
    let _ = std::process::Command::new("update-desktop-database")
        .arg(app_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Установить/обновить `.desktop` + иконки в пользовательских XDG-каталогах.
/// Идемпотентно. Под root (элевированный реинвок) — пропускаем: это
/// пользовательская интеграция, и писать в /root не нужно.
pub fn integrate() {
    if super::elevate::is_elevated() {
        return;
    }
    let Some(data) = data_home() else {
        logging::warn("desktop", "не определить XDG data-каталог (нет HOME)");
        return;
    };

    let mut changed = false;
    changed |= write_if_changed(
        &data.join("icons/hicolor/256x256/apps").join(format!("{APP_ID}.png")),
        ICON_PNG_256,
    );
    changed |= write_if_changed(
        &data.join("icons/hicolor/scalable/apps").join(format!("{APP_ID}.svg")),
        ICON_SVG,
    );

    let app_dir = data.join("applications");
    if let Some(exec) = exec_path() {
        changed |= write_if_changed(
            &app_dir.join(format!("{APP_ID}.desktop")),
            desktop_entry(&exec).as_bytes(),
        );
    } else {
        logging::warn("desktop", "не определить путь запуска для Exec= — entry не записан");
    }

    // Кэш дёргаем только если что-то реально изменилось.
    if changed {
        refresh_caches(&app_dir);
        logging::info("desktop", "desktop-интеграция обновлена");
    }
}
