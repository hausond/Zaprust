// Адаптер стратегий Flowseal (`general*.bat`) под Linux/nfqws.
//
// Flowseal-батники вызывают `winws.exe` с длинной строкой аргументов (профили
// разделены `--new`). На Linux движок — `nfqws`, и набор desync-флагов у него
// тот же (winws — порт nfqws). Отличия, которые снимает этот адаптер:
//   • `--wf-tcp=` / `--wf-udp=` — на Windows это фильтр захвата WinDivert. На
//     Linux захват живёт в правилах nftables (модуль `nft`), поэтому эти флаги
//     ВЫНИМАЮТСЯ из аргументов в `wf_tcp`/`wf_udp` стратегии и потом задают порты
//     очереди NFQUEUE — БЕЗ них nfqws не получит, например, пакеты Discord-медиа
//     (tcp 2053/2083/8443, udp 19294-19344/50000-50100), и обхода для них не будет.
//   • Пути `%BIN%`/`%LISTS%`/`%~dp0` разворачиваются в каталог ядра, а виндовый
//     разделитель `\` меняется на `/`.
//   • `%GameFilter*%` ОСТАЁТСЯ литералом — его резолвит движок в момент запуска
//     (как на Windows), потому что значение зависит от тумблера Game Filter.
//
// Парсер намеренно «терпимый»: битый/непохожий батник возвращает Err, и источник
// стратегий его просто пропускает, а не роняет приложение.

use std::collections::HashMap;
use std::path::Path;

use crate::strategies::Strategy;

/// Распарсить один Flowseal `.bat` в Linux-`Strategy`. `core_dir` — каталог ядра,
/// куда указывают `%~dp0`/`%BIN%`/`%LISTS%`. Возвращает Err со строкой-причиной,
/// если файл не похож на стратегию (вызывающий пропустит его).
pub fn parse_flowseal(path: &Path, core_dir: &Path) -> Result<Strategy, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("чтение: {e}"))?;
    let text = String::from_utf8_lossy(&bytes);

    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("strategy")
        .to_owned();

    // dp0 = путь к ядру с завершающим слэшем (Linux-аналог %~dp0). Forward-slash.
    let dp0 = format!("{}/", core_dir.display());

    let logical = join_continuations(&strip_comments(&text));

    // set-переменные (BIN, LISTS, …) с подстановкой %~dp0.
    let mut vars: HashMap<String, String> = HashMap::new();
    for line in &logical {
        if let Some((k, v)) = parse_set(line) {
            vars.insert(k.to_uppercase(), v.replace("%~dp0", &dp0));
        }
    }

    let winws_line = logical
        .iter()
        .find(|l| l.to_lowercase().contains("winws.exe"))
        .ok_or_else(|| "вызов winws.exe не найден".to_owned())?;

    let raw_args = extract_args_after_winws(winws_line);
    if raw_args.trim().is_empty() {
        return Err("пустой список аргументов".to_owned());
    }

    let expanded = expand_vars(&raw_args.replace("%~dp0", &dp0), &vars);
    let tokens = split_args(&expanded);
    if tokens.is_empty() {
        return Err("аргументы не разобрались".to_owned());
    }

    // Разделяем: --wf-tcp/--wf-udp → порты захвата (для nft), всё прочее → args.
    // Виндовый разделитель пути `\` меняем на `/` (в путях к .bin/спискам).
    let mut wf_tcp: Vec<String> = Vec::new();
    let mut wf_udp: Vec<String> = Vec::new();
    let mut args: Vec<String> = Vec::new();
    for t in tokens {
        if let Some(v) = t.strip_prefix("--wf-tcp=") {
            wf_tcp = split_ports(v);
        } else if let Some(v) = t.strip_prefix("--wf-udp=") {
            wf_udp = split_ports(v);
        } else {
            args.push(t.replace('\\', "/"));
        }
    }
    if args.is_empty() {
        return Err("после удаления --wf-* не осталось аргументов".to_owned());
    }

    Ok(Strategy {
        group: group_of(&name),
        name,
        source_path: path.to_path_buf(),
        raw_args: args.join(" "),
        args,
        wf_tcp,
        wf_udp,
    })
}

/// Разбить значение `--wf-*` / список портов по запятым в непустые токены.
fn split_ports(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned())
        .collect()
}

/// Определить группу по имени файла стратегии (тот же таксономия, что на Windows).
pub fn group_of(name: &str) -> String {
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

/// Отсортировать стратегии по группам, затем по имени (натурально для ALT2<ALT10).
pub fn sort_strategies(items: &mut [Strategy]) {
    items.sort_by(|a, b| {
        group_rank(&a.group)
            .cmp(&group_rank(&b.group))
            .then_with(|| natural_key(&a.name).cmp(&natural_key(&b.name)))
    });
}

/// Ключ натуральной сортировки: числа сравниваются как числа, буквы — лексикой в
/// нижнем регистре. Пунктуация/пробелы игнорируются, иначе закрывающая скобка в
/// «(ALT)» сортировалась бы после «(ALT2)». Так «ALT» < «ALT2» < «ALT10».
fn natural_key(s: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    let mut chars = s.chars().filter(|c| c.is_ascii_alphanumeric()).peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut num = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    num.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push((num.parse::<u64>().unwrap_or(u64::MAX), String::new()));
        } else {
            let mut word = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    break;
                }
                word.push(d.to_ascii_lowercase());
                chars.next();
            }
            out.push((0, word));
        }
    }
    out
}

// ── Примитивы разбора .bat (формат Flowseal) ─────────────────────────────────
// Логика совпадает с виндовым парсером (`platform/windows/strategy.rs`); вынесена
// сюда для Linux-адаптера. Windows-парсер оставлен как есть, чтобы не трогать
// рабочее поведение Windows-сборки.

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
    if !t.to_lowercase().starts_with("set ") {
        return None;
    }
    let mut rest = t[4..].trim();
    if let Some(inner) = rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        rest = inner.trim();
    }
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
            after.trim_start().trim_start_matches('"').trim().to_string()
        }
        None => String::new(),
    }
}

/// Подставить %VAR% значениями (многопроходно). Нераспознанные (`%GameFilter*%`)
/// оставляем как есть — их резолвит движок при запуске.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const ALT_BAT: &str = r#"@echo off
chcp 65001 > nul
:: 65001 - UTF-8
cd /d "%~dp0"
set "BIN=%~dp0bin\"
set "LISTS=%~dp0lists\"
start "zapret: %~n0" /min "%BIN%winws.exe" --wf-tcp=80,443,2053,8443,%GameFilterTCP% --wf-udp=443,50000-50100,%GameFilterUDP% ^
--filter-udp=443 --hostlist="%LISTS%list-general.txt" --dpi-desync=fake --dpi-desync-fake-quic="%BIN%quic_initial_www_google_com.bin" --new ^
--filter-tcp=80,443 --dpi-desync=fake,fakedsplit --dpi-desync-fake-tls="%BIN%tls_clienthello_www_google_com.bin"
"#;

    fn write_bat(name: &str, body: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "zaprust_bat_{}_{}",
            std::process::id(),
            name.replace(['(', ')', ' '], "_")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        (path, dir)
    }

    #[test]
    fn extracts_wf_ports_and_strips_them() {
        let (path, core) = write_bat("general (ALT).bat", ALT_BAT);
        let s = parse_flowseal(&path, &core).unwrap();

        assert_eq!(s.group, "ALT");
        // --wf-* вынуты в порты и в args их больше нет.
        assert_eq!(s.wf_tcp, vec!["80", "443", "2053", "8443", "%GameFilterTCP%"]);
        assert_eq!(s.wf_udp, vec!["443", "50000-50100", "%GameFilterUDP%"]);
        assert!(!s.args.iter().any(|a| a.starts_with("--wf-")), "args: {:?}", s.args);

        // Пути развёрнуты в core/, backslash → forward slash.
        let joined = s.args.join(" ");
        assert!(!joined.contains("%BIN%") && !joined.contains("%LISTS%"));
        assert!(!joined.contains('\\'), "остался backslash: {joined}");
        let core_str = core.display().to_string();
        assert!(
            s.args.iter().any(|a| a.contains(&core_str) && a.ends_with("quic_initial_www_google_com.bin")),
            "ожидали абсолютный путь к payload: {:?}",
            s.args
        );
        // %GameFilter% в args (если был) остаётся литералом для рантайма.
        std::fs::remove_dir_all(&core).ok();
    }

    #[test]
    fn skips_bat_without_winws() {
        let (path, core) = write_bat("general (BAD).bat", "@echo off\nset BIN=%~dp0bin\\\necho hi\n");
        assert!(parse_flowseal(&path, &core).is_err());
        std::fs::remove_dir_all(&core).ok();
    }

    #[test]
    fn natural_sort_orders_alt_numerically() {
        let mk = |n: &str| Strategy {
            name: n.to_owned(),
            group: group_of(n),
            source_path: PathBuf::new(),
            args: vec!["--x".into()],
            raw_args: "--x".into(),
            wf_tcp: vec![],
            wf_udp: vec![],
        };
        let mut v = vec![mk("general (ALT10)"), mk("general (ALT2)"), mk("general"), mk("general (ALT)")];
        sort_strategies(&mut v);
        let names: Vec<_> = v.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["general", "general (ALT)", "general (ALT2)", "general (ALT10)"]);
    }
}
