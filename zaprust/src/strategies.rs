// Core-слой: поиск папки ядра zapret (сборка Flowseal) и парсинг стратегий
// из `general*.bat`. Парсер намеренно «терпимый»: формат батников Flowseal
// меняется между релизами, поэтому битый файл просто пропускается, а не роняет
// приложение.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Одна стратегия = набор аргументов для winws.exe, вытащенный из .bat.
#[derive(Clone, Debug)]
pub struct Strategy {
    /// Человекочитаемое имя (из имени файла без расширения).
    pub name: String,
    /// Группа для дропдауна: general / ALT / FAKE TLS / Simple Fake / …
    pub group: String,
    /// Откуда взято (используется на следующих шагах — запуск/отладка).
    #[allow(dead_code)]
    pub source_path: PathBuf,
    /// Разобранные аргументы winws (понадобятся на шаге 3 для запуска).
    pub args: Vec<String>,
    /// Исходная строка аргументов (для отладки).
    #[allow(dead_code)]
    pub raw_args: String,
}

/// Результат сканирования папки ядра.
#[derive(Clone, Debug, Default)]
pub struct CoreScan {
    /// Найденная папка ядра (если есть).
    pub core_dir: Option<PathBuf>,
    /// Успешно распарсенные стратегии, отсортированные по группам.
    pub strategies: Vec<Strategy>,
    /// Сообщения о проблемах (пропущенные файлы и т.п.) — для лога.
    pub messages: Vec<String>,
}

/// Найти папку ядра. Приоритет — рядом с исполняемым файлом (релизный layout),
/// затем dev-фолбэки (рядом с Cargo.toml и в рабочей директории).
pub fn find_core_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("core"));
        }
    }
    // dev: папка проекта (зашита на этапе компиляции)
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("core"));
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("core"));
    }

    candidates.into_iter().find(|p| p.is_dir())
}

/// Куда устанавливать ядро, если его ещё нет (рядом с exe; в dev — у Cargo.toml).
pub fn preferred_core_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // В dev (exe в target/…) кладём к проекту, иначе — рядом с exe.
            if dir.ends_with("debug") || dir.ends_with("release") {
                return PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("core");
            }
            return dir.join("core");
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("core")
}

/// Просканировать ядро: найти папку, перечислить и распарсить стратегии.
pub fn scan() -> CoreScan {
    let mut out = CoreScan::default();

    let Some(core_dir) = find_core_dir() else {
        out.messages
            .push("ядро не найдено — положите сборку Flowseal в папку core/".to_owned());
        return out;
    };
    out.messages
        .push(format!("ядро: {}", core_dir.display()));

    let entries = match std::fs::read_dir(&core_dir) {
        Ok(e) => e,
        Err(e) => {
            out.messages
                .push(format!("не удалось прочитать core/: {e}"));
            out.core_dir = Some(core_dir);
            return out;
        }
    };

    let mut bats: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_general_bat(p))
        .collect();
    bats.sort();

    for path in bats {
        match parse_bat(&path, &core_dir) {
            Ok(strategy) => out.strategies.push(strategy),
            Err(why) => out.messages.push(format!(
                "пропущен {}: {why}",
                path.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }

    sort_strategies(&mut out.strategies);
    out.messages
        .push(format!("стратегий найдено: {}", out.strategies.len()));
    out.core_dir = Some(core_dir);
    out
}

/// Файл вида `general*.bat` (без учёта регистра расширения).
fn is_general_bat(p: &Path) -> bool {
    let is_bat = p
        .extension()
        .map(|e| e.eq_ignore_ascii_case("bat"))
        .unwrap_or(false);
    let name_ok = p
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_lowercase().starts_with("general"))
        .unwrap_or(false);
    is_bat && name_ok
}

/// Распарсить один .bat в стратегию. Возвращает Err со строкой-причиной,
/// если файл не похож на стратегию (вызывающий просто пропустит его).
pub fn parse_bat(path: &Path, core_dir: &Path) -> Result<Strategy, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("чтение: {e}"))?;
    let text = String::from_utf8_lossy(&bytes);

    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("strategy")
        .to_owned();

    // dp0 = путь к папке ядра с завершающим слэшем (как %~dp0 в cmd).
    let dp0 = format!("{}{}", core_dir.display(), std::path::MAIN_SEPARATOR);

    let logical = join_continuations(&strip_comments(&text));

    // Собираем set-переменные.
    let mut vars: HashMap<String, String> = HashMap::new();
    for line in &logical {
        if let Some((k, v)) = parse_set(line) {
            let value = v.replace("%~dp0", &dp0);
            vars.insert(k.to_uppercase(), value);
        }
    }

    // Ищем строку с вызовом winws.exe.
    let winws_line = logical
        .iter()
        .find(|l| l.to_lowercase().contains("winws.exe"))
        .ok_or_else(|| "вызов winws.exe не найден".to_owned())?;

    let raw_args = extract_args_after_winws(winws_line);
    if raw_args.trim().is_empty() {
        return Err("пустой список аргументов".to_owned());
    }

    // Подстановка %~dp0 и %VAR%, затем разбивка с учётом кавычек.
    let expanded = expand_vars(&raw_args.replace("%~dp0", &dp0), &vars);
    let args = split_args(&expanded);
    if args.is_empty() {
        return Err("аргументы не разобрались".to_owned());
    }

    Ok(Strategy {
        group: group_of(&name),
        name,
        source_path: path.to_path_buf(),
        args,
        raw_args,
    })
}

/// Убрать строки-комментарии (`rem`, `::`).
fn strip_comments(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| {
            let t = line.trim_start();
            let lower = t.to_lowercase();
            !(t.starts_with("::") || lower == "rem" || lower.starts_with("rem "))
        })
        .map(|l| l.to_string())
        .collect()
}

/// Склеить строки, разорванные символом продолжения `^` в конце.
fn join_continuations(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut acc = String::new();
    let mut pending = false;

    for line in lines {
        let trimmed = line.trim_end();
        if let Some(stripped) = trimmed.strip_suffix('^') {
            acc.push_str(stripped);
            acc.push(' ');
            pending = true;
        } else {
            acc.push_str(trimmed);
            out.push(std::mem::take(&mut acc));
            pending = false;
        }
    }
    if pending && !acc.is_empty() {
        out.push(acc);
    }
    out
}

/// Разобрать `set NAME=VALUE` (в т.ч. форму `set "NAME=VALUE"`).
fn parse_set(line: &str) -> Option<(String, String)> {
    let t = line.trim();
    let lower = t.to_lowercase();
    if !lower.starts_with("set ") {
        return None;
    }
    let mut rest = t[4..].trim();
    // форма set "NAME=VALUE"
    if let Some(inner) = rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        rest = inner.trim();
    }
    // отбрасываем флаги вроде /a, /p — для них значение не парсим терпимо
    let eq = rest.find('=')?;
    let name = rest[..eq].trim().to_string();
    let value = rest[eq + 1..].trim().trim_matches('"').to_string();
    if name.is_empty() {
        return None;
    }
    Some((name, value))
}

/// Взять всё после токена `winws.exe` как строку аргументов.
fn extract_args_after_winws(line: &str) -> String {
    let lower = line.to_lowercase();
    match lower.find("winws.exe") {
        Some(pos) => {
            let after = &line[pos + "winws.exe".len()..];
            // отрезаем возможную закрывающую кавычку пути к exe
            after.trim_start().trim_start_matches('"').trim().to_string()
        }
        None => String::new(),
    }
}

/// Подставить %VAR% значениями (многопроходно, для вложенных ссылок).
/// Нераспознанные переменные оставляем как есть — это терпимо.
fn expand_vars(input: &str, vars: &HashMap<String, String>) -> String {
    let mut cur = input.to_string();
    for _ in 0..10 {
        let (next, changed) = expand_once(&cur, vars);
        cur = next;
        if !changed {
            break;
        }
    }
    cur
}

fn expand_once(s: &str, vars: &HashMap<String, String>) -> (String, bool) {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut changed = false;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' {
            if let Some(j) = (i + 1..chars.len()).find(|&k| chars[k] == '%') {
                let name: String = chars[i + 1..j].iter().collect();
                if let Some(val) = vars.get(&name.to_uppercase()) {
                    out.push_str(val);
                    changed = true;
                    i = j + 1;
                    continue;
                } else {
                    // не нашли — оставляем %name% как есть
                    out.extend(&chars[i..=j]);
                    i = j + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    (out, changed)
}

/// Разбить строку аргументов в Vec, уважая кавычки.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut has_token = false;

    for c in s.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quote => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}

/// Разрешить рантайм-переменные Game Filter в аргументах стратегии.
///
/// В сборке Flowseal `%GameFilterTCP%` / `%GameFilterUDP%` / `%GameFilter%`
/// подставляет `service.bat`: при выключенном фильтре — порт-заглушка `12`,
/// при включённом («all») — диапазон `1024-65535`. Наш парсер оставляет эти
/// переменные как литералы, а конкретное значение зависит от UI-тумблера,
/// поэтому подстановку делаем в момент запуска.
pub fn resolve_game_filter(args: &[String], game_filter_on: bool) -> Vec<String> {
    let val = if game_filter_on { "1024-65535" } else { "12" };
    args.iter()
        .map(|a| {
            a.replace("%GameFilterTCP%", val)
                .replace("%GameFilterUDP%", val)
                .replace("%GameFilter%", val)
        })
        .collect()
}

/// Определить группу по имени файла стратегии.
fn group_of(name: &str) -> String {
    let n = name.to_lowercase();
    if n.contains("fake tls") {
        "FAKE TLS".to_owned()
    } else if n.contains("simple fake") {
        "Simple Fake".to_owned()
    } else if n.contains("alt") {
        "ALT".to_owned()
    } else {
        "general".to_owned()
    }
}

/// Порядок групп в дропдауне.
fn group_rank(group: &str) -> u8 {
    match group {
        "general" => 0,
        "ALT" => 1,
        "FAKE TLS" => 2,
        "Simple Fake" => 3,
        _ => 4,
    }
}

fn sort_strategies(items: &mut [Strategy]) {
    items.sort_by(|a, b| {
        group_rank(&a.group)
            .cmp(&group_rank(&b.group))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // Записать временный .bat и вернуть его путь (+ папку-«ядро»).
    fn write_temp_bat(name: &str, body: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "zaprust_test_{}_{}",
            std::process::id(),
            name.replace(['(', ')', ' '], "_")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        (path, dir)
    }

    const SAMPLE_BAT: &str = r#"@echo off
chcp 65001 > nul
:: комментарий
cd /d "%~dp0"
set "BIN=%~dp0bin\"
set "LISTS=%~dp0lists\"
start "zapret: %~n0" /min "%BIN%winws.exe" --wf-tcp=80,443,%GameFilterTCP% ^
--filter-tcp=443 --hostlist="%LISTS%list-general.txt" --dpi-desync=fake --dpi-desync-fake-tls="%BIN%tls.bin" --new ^
--filter-tcp=%GameFilterTCP% --dpi-desync=multisplit
"#;

    #[test]
    fn parses_real_format_and_substitutes_vars() {
        let (path, core) = write_temp_bat("general.bat", SAMPLE_BAT);
        let s = parse_bat(&path, &core).unwrap();

        let joined = s.args.join(" ");
        // %BIN%/%LISTS%/%~dp0 развёрнуты; GameFilter оставлен парсером как литерал.
        assert!(!joined.contains("%BIN%"), "%BIN% не подставлен: {joined}");
        assert!(!joined.contains("%LISTS%"), "%LISTS% не подставлен");
        assert!(!joined.contains("%~dp0"), "%~dp0 не подставлен");
        assert!(joined.contains("%GameFilterTCP%"), "GameFilter должен остаться для рантайма");

        // Путь к hostlist стал абсолютным внутри core/.
        let core_str = core.display().to_string();
        assert!(
            s.args.iter().any(|a| a.contains(&core_str) && a.contains("list-general.txt")),
            "ожидали абсолютный путь к list-general.txt: {:?}",
            s.args
        );
        // Первый аргумент — реальный флаг winws, а не остаток `start`/exe.
        assert!(s.args[0].starts_with("--wf-tcp"), "первый аргумент: {:?}", s.args.first());

        std::fs::remove_dir_all(&core).ok();
    }

    #[test]
    fn skips_bat_without_winws() {
        let (path, core) = write_temp_bat(
            "general (BROKEN).bat",
            "@echo off\nset BIN=%~dp0bin\\\necho nothing here\n",
        );
        let res = parse_bat(&path, &core);
        assert!(res.is_err(), "битый батник без winws должен дать Err");
        std::fs::remove_dir_all(&core).ok();
    }

    #[test]
    fn resolve_game_filter_substitutes_by_toggle() {
        let args = vec![
            "--wf-tcp=80,443,%GameFilterTCP%".to_owned(),
            "--filter-udp=%GameFilterUDP%".to_owned(),
        ];
        let off = resolve_game_filter(&args, false);
        assert_eq!(off[0], "--wf-tcp=80,443,12");
        assert_eq!(off[1], "--filter-udp=12");

        let on = resolve_game_filter(&args, true);
        assert_eq!(on[0], "--wf-tcp=80,443,1024-65535");
        assert_eq!(on[1], "--filter-udp=1024-65535");
    }

    #[test]
    fn groups_by_filename() {
        assert_eq!(group_of("general"), "general");
        assert_eq!(group_of("general (ALT2)"), "ALT");
        assert_eq!(group_of("general (FAKE TLS AUTO)"), "FAKE TLS");
        assert_eq!(group_of("general (Simple Fake)"), "Simple Fake");
    }

    #[test]
    fn split_args_respects_quotes() {
        let v = split_args(r#"--a=1 --host="C:\path with space\l.txt" --b"#);
        assert_eq!(v, vec!["--a=1", r#"--host=C:\path with space\l.txt"#, "--b"]);
    }
}
