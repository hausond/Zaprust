// L6: сигналы авторитетной проверки «обход работает» на Linux. Сама tri-check
// собирается в `mod.rs` (StatusProbe), здесь — её атомарные части, доступные
// неэлевированному GUI:
//   • живой процесс nfqws (через `pgrep` — читается без root);
//   • наличие правил перехвата живёт в `nft::rules_present` (там есть имена
//     таблицы/цепочки), это третий, мягкий сигнал.
//
// Аналог Windows (`service::winws_alive`): там PID winws через toolhelp-снимок;
// здесь — pgrep по точному имени процесса.

use std::process::Command;

/// PID живого nfqws (первый, если их несколько). None — процесс не запущен или
/// `pgrep` недоступен. `-x` = точное совпадение имени процесса (не подстрока).
pub fn nfqws_pid() -> Option<u32> {
    let out = Command::new("pgrep").args(["-x", "nfqws"]).output().ok()?;
    if !out.status.success() {
        return None; // код 1 = совпадений нет
    }
    parse_first_pid(&String::from_utf8_lossy(&out.stdout))
}

/// Первый PID из вывода `pgrep` (по одному на строку). Вынесено для теста.
fn parse_first_pid(stdout: &str) -> Option<u32> {
    stdout.split_whitespace().next().and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_first_pid_from_pgrep_output() {
        assert_eq!(parse_first_pid("1234\n"), Some(1234));
        // Несколько процессов — берём первый.
        assert_eq!(parse_first_pid("1234\n5678\n"), Some(1234));
        // Пустой вывод (нет совпадений) — None.
        assert_eq!(parse_first_pid(""), None);
        assert_eq!(parse_first_pid("\n"), None);
        // Мусор не парсится в PID.
        assert_eq!(parse_first_pid("nope"), None);
    }
}
