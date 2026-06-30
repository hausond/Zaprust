// TLS-бэкенд теста на Linux: rustls (ring) со встроенными корнями webpki-roots.
// В отличие от Windows-`tester.rs` (native-tls/SChannel) здесь нет зависимости от
// системного OpenSSL `.so` — корни сертификатов вкомпилированы, что важно для
// переносимого AppImage (L9). Сама логика проб (ureq/TCP-замер хендшейка) живёт в
// общем коде; здесь — только настройка HTTP-агента с нужными таймаутами.
//
// rustls включается фичей ureq "tls" (Cargo.toml, под cfg(not(windows))). При ней
// `AgentBuilder` берёт rustls по умолчанию — явный коннектор задавать не нужно
// (на Windows фича "native-tls" вынуждает выставлять `tls_connector` вручную).

use std::time::Duration;

/// HTTP-агент на rustls. connect/overall — таймауты в мс, redirects — число
/// редиректов (0 для замеров пинга, чтобы один round-trip = время хендшейка).
fn build_agent_with(connect_ms: u64, overall_ms: u64, redirects: u32) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(connect_ms))
        .timeout(Duration::from_millis(overall_ms))
        .redirects(redirects)
        .build()
}

/// Агент для кнопки «Тест» / апдейтера (умеренные таймауты, следует редиректам).
pub fn agent() -> ureq::Agent {
    build_agent_with(3000, 4000, 5)
}

/// Агент для автоподбора: тугие таймауты, без редиректов (чистый замер хендшейка).
pub fn probe_agent() -> ureq::Agent {
    build_agent_with(1500, 1800, 0)
}

/// Агент для скачивания ядра/обновлений: connect 10с, чтение 30с на сегмент, до 5
/// редиректов; БЕЗ общего таймаута — иначе многомегабайтный zip обрывался бы.
pub fn download_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(10_000))
        .timeout_read(Duration::from_millis(30_000))
        .redirects(5)
        .build()
}
