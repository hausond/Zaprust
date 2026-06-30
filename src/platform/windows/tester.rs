// TLS-бэкенд теста на Windows: native-tls = SChannel (встроенный TLS, без сборки
// C/ring) — легче rustls и надёжнее на gnu-toolchain. Сама логика проб (ureq/TCP)
// живёт в общем коде; здесь только настройка HTTP-агента с нужным TLS.

use std::sync::Arc;
use std::time::Duration;

/// HTTP-агент с TLS Windows (SChannel). connect/overall — таймауты в мс,
/// redirects — число редиректов (0 для замеров пинга, чтобы один round-trip).
fn build_agent_with(connect_ms: u64, overall_ms: u64, redirects: u32) -> ureq::Agent {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(connect_ms))
        .timeout(Duration::from_millis(overall_ms))
        .redirects(redirects);
    if let Ok(connector) = native_tls::TlsConnector::new() {
        builder = builder.tls_connector(Arc::new(connector));
    }
    builder.build()
}

/// Агент для кнопки «Тест» / апдейтера (умеренные таймауты, следует редиректам).
pub fn agent() -> ureq::Agent {
    build_agent_with(3000, 4000, 5)
}

/// Агент для автоподбора: тугие таймауты, без редиректов (чистый замер).
pub fn probe_agent() -> ureq::Agent {
    build_agent_with(1500, 1800, 0)
}

/// Агент для скачивания ядра/обновлений: connect 10с, чтение 30с на сегмент, до 5
/// редиректов; БЕЗ общего таймаута — иначе многомегабайтный zip обрывался бы.
pub fn download_agent() -> ureq::Agent {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(10_000))
        .timeout_read(Duration::from_millis(30_000))
        .redirects(5);
    if let Ok(connector) = native_tls::TlsConnector::new() {
        builder = builder.tls_connector(Arc::new(connector));
    }
    builder.build()
}
