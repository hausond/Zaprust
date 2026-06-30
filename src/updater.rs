// Апдейтер ядра от Flowseal: проверка последнего релиза через GitHub API,
// скачивание zip и полная замена ядра (bin + стратегии + списки) с сохранением
// пользовательских списков (*-user.txt).
//
// Сеть/распаковка/файлы — всё синхронно внутри функций; вызывающий код держит
// их в фоновом потоке и шлёт прогресс в UI через канал.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const API_LATEST: &str =
    "https://api.github.com/repos/Flowseal/zapret-discord-youtube/releases/latest";

pub struct LatestRelease {
    pub tag: String,
    pub zip_url: String,
}

/// Запросить последний релиз Flowseal. User-Agent обязателен для GitHub API.
pub fn check_latest(agent: &ureq::Agent) -> Result<LatestRelease, String> {
    let resp = agent
        .get(API_LATEST)
        .set("User-Agent", "Zaprust")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("запрос к GitHub: {e}"))?;
    let body = resp
        .into_string()
        .map_err(|e| format!("чтение ответа GitHub: {e}"))?;

    let tag = json_str(&body, "tag_name").ok_or("в ответе нет tag_name")?;
    let zip_url = find_zip_url(&body).ok_or("в релизе нет .zip-ассета")?;
    Ok(LatestRelease { tag, zip_url })
}

/// Скачать файл по URL с колбэком прогресса (скачано, всего).
pub fn download(
    agent: &ureq::Agent,
    url: &str,
    dest: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<(), String> {
    let resp = agent
        .get(url)
        .set("User-Agent", "Zaprust")
        .call()
        .map_err(|e| format!("скачивание: {e}"))?;
    let total = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok());

    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(dest).map_err(|e| format!("создать файл: {e}"))?;
    let mut buf = [0u8; 65536];
    let mut done: u64 = 0;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("чтение: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("запись: {e}"))?;
        done += n as u64;
        progress(done, total);
    }
    Ok(())
}

/// Локальная версия ядра из core/version.txt (None — если файла нет).
pub fn local_version(core_dir: &Path) -> Option<String> {
    std::fs::read_to_string(core_dir.join("version.txt"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

pub fn write_version(core_dir: &Path, tag: &str) -> Result<(), String> {
    std::fs::write(core_dir.join("version.txt"), tag).map_err(|e| format!("version.txt: {e}"))
}

/// Распаковать архив во временную папку и заменить ядро целиком.
/// Пользовательские списки (*-user.txt) сохраняются.
pub fn apply(core_dir: &Path, zip_path: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("открыть zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("zip: {e}"))?;

    let tmp = std::env::temp_dir().join(format!("zaprust_core_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("временная папка: {e}"))?;
    archive
        .extract(&tmp)
        .map_err(|e| format!("распаковка: {e}"))?;

    // Корень ядра внутри архива — папка, где лежит bin/winws.exe.
    let root = find_core_root(&tmp).ok_or("в архиве не найден bin/winws.exe")?;

    copy_over(&root, core_dir)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// Скачать актуальный релиз Flowseal и достать из него один файл `lists/<name>`
/// (для отката конкретного списка к стандартному).
pub fn fetch_default_list(agent: &ureq::Agent, file_name: &str) -> Result<Vec<u8>, String> {
    let latest = check_latest(agent)?;
    let zip_path = std::env::temp_dir().join("zaprust_list_revert.zip");
    download(agent, &latest.zip_url, &zip_path, |_, _| {})?;

    let file = std::fs::File::open(&zip_path).map_err(|e| format!("открыть zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("zip: {e}"))?;
    let target = format!("lists/{file_name}");
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("zip entry: {e}"))?;
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        if name == target || name.ends_with(&format!("/{target}")) {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| format!("чтение из zip: {e}"))?;
            return Ok(buf);
        }
    }
    Err(format!("в релизе нет lists/{file_name}"))
}

// ── Движок nfqws (bol-van/zapret) — только Linux ─────────────────────────────
//
// На Linux ядро = ДВА источника (см. L8): ассеты Flowseal (стратегии/.bin/списки)
// + движок `nfqws`. Движок берём из релизов bol-van/zapret — там же, откуда родом
// winws (winws — порт nfqws). У релиза есть `.zip`-ассет с прекомпилированными
// бинарями под все платформы в `binaries/<платформа>/nfqws`, поэтому достаточно
// уже подключённого крейта `zip` (без tar/gzip). Версия движка ведётся отдельно от
// Flowseal-ассетов (`nfqws-version.txt` vs `version.txt`) — у каждого свой релиз.

#[cfg(not(windows))]
const API_LATEST_NFQWS: &str = "https://api.github.com/repos/bol-van/zapret/releases/latest";

/// Каталог в `binaries/` релиза bol-van под текущую архитектуру.
#[cfg(not(windows))]
fn nfqws_platform_dir() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "linux-x86_64",
        "aarch64" => "linux-arm64",
        "arm" => "linux-arm",
        "powerpc" => "linux-ppc",
        // Прочие (mips*, …) встречаются у роутеров, не у десктопа — фолбэк на x86_64.
        _ => "linux-x86_64",
    }
}

/// Последний релиз bol-van/zapret (движок nfqws). Парс — как у Flowseal: tag_name +
/// первый `.zip`-ассет.
#[cfg(not(windows))]
pub fn check_latest_nfqws(agent: &ureq::Agent) -> Result<LatestRelease, String> {
    let resp = agent
        .get(API_LATEST_NFQWS)
        .set("User-Agent", "Zaprust")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("запрос к GitHub (bol-van): {e}"))?;
    let body = resp
        .into_string()
        .map_err(|e| format!("чтение ответа GitHub (bol-van): {e}"))?;
    let tag = json_str(&body, "tag_name").ok_or("в ответе bol-van нет tag_name")?;
    let zip_url = find_zip_url(&body).ok_or("в релизе bol-van нет .zip-ассета")?;
    Ok(LatestRelease { tag, zip_url })
}

/// Достать `nfqws` под текущую архитектуру из релизного zip bol-van в `core/nfqws`
/// и сделать исполняемым (0755). Ищем запись `…/binaries/<платформа>/nfqws`.
#[cfg(not(windows))]
pub fn extract_nfqws(zip_path: &Path, core_dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("открыть zip nfqws: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("zip nfqws: {e}"))?;

    let want = format!("binaries/{}/nfqws", nfqws_platform_dir());
    let mut bytes: Option<Vec<u8>> = None;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("zip entry: {e}"))?;
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        // Имя обычно с префиксом релиза: `zapret-vXX/binaries/linux-x86_64/nfqws`.
        if name == want || name.ends_with(&format!("/{want}")) {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).map_err(|e| format!("чтение nfqws из zip: {e}"))?;
            bytes = Some(buf);
            break;
        }
    }
    let bytes = bytes.ok_or_else(|| format!("в релизе bol-van нет {want}"))?;

    std::fs::create_dir_all(core_dir).map_err(|e| format!("создать {}: {e}", core_dir.display()))?;
    let dest = core_dir.join("nfqws");
    std::fs::write(&dest, &bytes).map_err(|e| format!("запись nfqws: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod nfqws: {e}"))?;
    }
    Ok(())
}

/// Локальная версия движка nfqws из `core/nfqws-version.txt` (None — если нет).
#[cfg(not(windows))]
pub fn local_nfqws_version(core_dir: &Path) -> Option<String> {
    std::fs::read_to_string(core_dir.join("nfqws-version.txt"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

#[cfg(not(windows))]
pub fn write_nfqws_version(core_dir: &Path, tag: &str) -> Result<(), String> {
    std::fs::write(core_dir.join("nfqws-version.txt"), tag)
        .map_err(|e| format!("nfqws-version.txt: {e}"))
}

// ── Вспомогательное ──────────────────────────────────────────────────────────

/// Извлечь строковое значение поля JSON: "key":"value".
fn json_str(body: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = body.find(&pat)? + pat.len();
    let rest = &body[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

/// Первый browser_download_url, оканчивающийся на .zip.
fn find_zip_url(body: &str) -> Option<String> {
    let key = "\"browser_download_url\":\"";
    let mut search = body;
    while let Some(p) = search.find(key) {
        let s = &search[p + key.len()..];
        let end = s.find('"')?;
        let url = &s[..end];
        if url.ends_with(".zip") {
            return Some(url.to_owned());
        }
        search = &s[end..];
    }
    None
}

/// Найти папку ядра (содержит bin/winws.exe) на глубине 0 или 1.
fn find_core_root(base: &Path) -> Option<PathBuf> {
    if base.join("bin").join("winws.exe").exists() {
        return Some(base.to_path_buf());
    }
    for entry in std::fs::read_dir(base).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() && p.join("bin").join("winws.exe").exists() {
            return Some(p);
        }
    }
    None
}

/// Рекурсивно скопировать src → dst, перезаписывая, но сохраняя существующие
/// файлы вида *-user.txt (их юзер мог редактировать).
fn copy_over(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {e}", dst.display()))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("read_dir: {e}"))?.flatten() {
        let from = entry.path();
        let name = entry.file_name();
        let to = dst.join(&name);

        if from.is_dir() {
            copy_over(&from, &to)?;
        } else {
            let lower = name.to_string_lossy().to_lowercase();
            if lower.ends_with("-user.txt") && to.exists() {
                continue; // не затираем пользовательские списки
            }
            // На Linux выкидываем виндовые артефакты Flowseal: движок и драйвер
            // платформозависимы (winws.exe, WinDivert*.dll/.sys, cygwin1.dll, *.exe).
            // Всё прочее (.bat, bin/*.bin, lists/*.txt) работает с nfqws как есть.
            if is_windows_artifact(&lower) {
                continue;
            }
            copy_with_retry(&from, &to)?;
        }
    }
    Ok(())
}

/// Виндовый артефакт сборки Flowseal, который на Linux не нужен (имя в нижнем
/// регистре). На Windows ничего не выкидываем — там это и есть движок/драйвер.
#[cfg(windows)]
fn is_windows_artifact(_lower_name: &str) -> bool {
    false
}
#[cfg(not(windows))]
fn is_windows_artifact(lower_name: &str) -> bool {
    lower_name.ends_with(".exe")
        || lower_name.ends_with(".dll")
        || lower_name.ends_with(".sys")
        || lower_name.starts_with("windivert")
}

/// Копирование с ретраями — на случай ещё не отпущенного файлового лока
/// (драйвер/процесс выгружаются асинхронно).
fn copy_with_retry(from: &Path, to: &Path) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 0..6 {
        match std::fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(std::time::Duration::from_millis(400 * (attempt + 1)));
            }
        }
    }
    Err(format!("copy {}: {last}", to.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tag_and_zip_url() {
        let body = r#"{"tag_name":"1.8.2","assets":[{"browser_download_url":"https://x/notes.txt"},{"browser_download_url":"https://x/zapret-discord-youtube-1.8.2.zip"}]}"#;
        assert_eq!(json_str(body, "tag_name").as_deref(), Some("1.8.2"));
        assert_eq!(
            find_zip_url(body).as_deref(),
            Some("https://x/zapret-discord-youtube-1.8.2.zip")
        );
    }
}
