// Общая модель стратегий обхода — платформо-независимые типы, на которые
// опираются GUI, оркестрация и автоподбор. Откуда берутся стратегии (Windows —
// парсинг `.bat` Flowseal, Linux — нативный bol-van/zapret), решает платформенный
// слой через трейт `StrategySource`; здесь — только данные.

use std::path::PathBuf;

/// Одна стратегия = набор аргументов движка обхода с человекочитаемым именем.
#[derive(Clone, Debug)]
pub struct Strategy {
    /// Человекочитаемое имя (на Windows — имя `.bat` без расширения).
    pub name: String,
    /// Группа для дропдауна: general / ALT / FAKE TLS / Simple Fake / …
    pub group: String,
    /// Откуда взято (диагностика). Заполняется источником стратегий платформы.
    #[allow(dead_code)]
    pub source_path: PathBuf,
    /// Разобранные аргументы движка (на Windows — argv winws). Потребляются
    /// платформенным `BypassRuntime`; на Linux движок придёт в L2.
    #[allow(dead_code)]
    pub args: Vec<String>,
    /// Исходная строка аргументов (для отладки).
    #[allow(dead_code)]
    pub raw_args: String,
    /// Linux: TCP-порты для захвата в NFQUEUE (из winws `--wf-tcp`). На Windows
    /// перехват задаёт сам winws, поэтому поле не используется и остаётся пустым.
    /// Пусто ⇒ движок берёт порты по умолчанию (80,443). Токены — как в winws:
    /// одиночные порты и диапазоны (`19294-19344`), плюс `%GameFilter*%`.
    #[allow(dead_code)]
    pub wf_tcp: Vec<String>,
    /// Linux: UDP-порты для захвата в NFQUEUE (из winws `--wf-udp`). См. `wf_tcp`.
    #[allow(dead_code)]
    pub wf_udp: Vec<String>,
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
