// Перехват пакетов на Linux: правила фаервола, заворачивающие ИСХОДЯЩИЙ трафик в
// очередь NFQUEUE, откуда его читает nfqws по `--qnum`. Какие именно порты ловить
// — задаёт стратегия (на Windows это `--wf-tcp`/`--wf-udp` внутри winws; на Linux
// эти порты приходят сюда списком и программируют правило).
//
// Главное отличие от Windows: там весь фильтр трафика — в аргументах winws
// (WinDivert внутри них). На Linux движок (nfqws) сам НИЧЕГО не перехватывает —
// правила живут отдельно: поднимаются ДО демона, снимаются ПОСЛЕ.
//
// Флаг `bypass` (nft) / `--queue-bypass` (iptables) ОБЯЗАТЕЛЕН: при неподнятом
// или упавшем демоне пакеты идут мимо очереди, а не блокируются — иначе весь
// трафик встал бы. Номер очереди `--queue-num` должен совпадать с `--qnum` nfqws.

use std::process::Command;

use crate::logging;

/// Имя выделенной таблицы nftables. Свою таблицу сносим ЦЕЛИКОМ при teardown
/// (`delete table`) — не остаётся ни цепочки, ни правил.
const NFT_TABLE: &str = "zaprust";
/// Имя выделенной цепочки iptables (в таблице mangle). Teardown снимает переход
/// из POSTROUTING + чистит/удаляет цепочку — не зная, какие порты были (важно,
/// т.к. порты теперь зависят от стратегии).
const IPT_CHAIN: &str = "ZAPRUST";

/// Доступный на машине бэкенд фаервола.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Нативный nftables (`nft`) — предпочтительно (чистый teardown таблицей).
    Nft,
    /// iptables (под капотом nft или legacy); хранит имя бинаря (`iptables-nft`,
    /// `iptables-legacy`, `iptables`).
    Iptables(&'static str),
}

impl Backend {
    /// Человекочитаемая метка для лога/диагностики.
    pub fn label(self) -> String {
        match self {
            Backend::Nft => "nftables (nft)".to_owned(),
            Backend::Iptables(p) => format!("iptables ({p})"),
        }
    }
}

/// Запустить команду фаервола, вернуть Err с её выводом при ненулевом коде.
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        crate::logging::debug("nft", format!("{program} {} → ok", args.join(" ")));
        Ok(())
    } else {
        let t = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Err(format!("{program} {}: {}", args.join(" "), t.trim()))
    }
}

/// Доступна ли команда (бинарь есть и базовый list отрабатывает).
fn cmd_ok(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Подобрать доступный бэкенд: сперва нативный `nft`, затем варианты iptables.
pub fn detect() -> Option<Backend> {
    if cmd_ok("nft", &["list", "ruleset"]) {
        return Some(Backend::Nft);
    }
    for prog in ["iptables-nft", "iptables-legacy", "iptables"] {
        if cmd_ok(prog, &["-S"]) {
            return Some(Backend::Iptables(prog));
        }
    }
    None
}

/// Поднять правила перехвата под номер очереди `qnum` для заданных портов. Порты
/// приходят уже разрешёнными (Game Filter подставлен, дефолт применён в `runtime`).
/// `tcp_ports`/`udp_ports` — токены в формате nft (диапазоны через дефис).
pub fn up(backend: Backend, qnum: u16, tcp_ports: &[String], udp_ports: &[String]) -> Result<(), String> {
    match backend {
        Backend::Nft => up_nft(qnum, tcp_ports, udp_ports),
        Backend::Iptables(prog) => up_iptables(prog, qnum, tcp_ports, udp_ports),
    }
}

/// Снять правила, поднятые именно этим бэкендом (нормальный Стоп).
pub fn down(backend: Backend) -> Result<(), String> {
    match backend {
        Backend::Nft => down_nft(),
        Backend::Iptables(prog) => {
            down_iptables(prog);
            Ok(())
        }
    }
}

/// L6 (best-effort): присутствуют ли НАШИ правила перехвата. Точечно спрашиваем
/// nft-таблицу `zaprust` / iptables-цепочку `ZAPRUST`.
///   • `Some(true)`  — правила точно на месте;
///   • `Some(false)` — бэкенд прочитан, но наших правил нет (достоверный «нет»);
///   • `None`        — прочитать не удалось (нет прав / нет инструмента) → «не знаю».
/// `nft list` под обычным пользователем часто отдаёт «Operation not permitted» —
/// тогда возвращаем None, и в tri-check правила служат лишь мягким третьим
/// сигналом (вето только при достоверном `Some(false)`).
pub fn rules_present() -> Option<bool> {
    // nft: точечный list нашей таблицы.
    match Command::new("nft").args(["list", "table", "inet", NFT_TABLE]).output() {
        Ok(o) if o.status.success() => return Some(true),
        Ok(o) => {
            // Различаем «таблицы нет» (бэкенд читается) и «нет прав» (не знаем).
            let err = String::from_utf8_lossy(&o.stderr).to_lowercase();
            if err.contains("no such") || err.contains("does not exist") {
                return Some(false);
            }
            // иначе, скорее всего, нет прав — попробуем iptables ниже.
        }
        Err(_) => {} // nft нет вовсе
    }
    // iptables fallback: ищем нашу цепочку в mangle.
    for prog in ["iptables-nft", "iptables-legacy", "iptables"] {
        match Command::new(prog).args(["-t", "mangle", "-S", IPT_CHAIN]).output() {
            Ok(o) if o.status.success() => return Some(true),
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr).to_lowercase();
                if err.contains("no chain") || err.contains("no such") {
                    return Some(false);
                }
            }
            Err(_) => {}
        }
    }
    None
}

/// Снести ВСЕ возможные наши правила (best-effort), не зная, чем их ставили:
/// и nft-таблицу, и iptables-цепочку во всех вариантах. Для аварийной чистки.
pub fn down_all() {
    let _ = down_nft();
    for prog in ["iptables-nft", "iptables-legacy", "iptables"] {
        down_iptables(prog);
    }
}

// ── nftables ─────────────────────────────────────────────────────────────────

fn up_nft(qnum: u16, tcp_ports: &[String], udp_ports: &[String]) -> Result<(), String> {
    // На случай прежнего запуска — снести свою таблицу (best-effort), не плодим.
    let _ = down_nft();

    let q = qnum.to_string();
    run("nft", &["add", "table", "inet", NFT_TABLE])?;
    // inet-семейство покрывает IPv4 и IPv6 одной таблицей.
    run(
        "nft",
        &[
            "add",
            "chain",
            "inet",
            NFT_TABLE,
            "post",
            "{ type filter hook postrouting priority mangle; policy accept; }",
        ],
    )?;
    // Исходящие: совпадение по dport-набору. queue ... bypass — при упавшем демоне мимо.
    if !tcp_ports.is_empty() {
        let set = format!("{{{}}}", tcp_ports.join(","));
        run(
            "nft",
            &[
                "add", "rule", "inet", NFT_TABLE, "post", "tcp", "dport", &set, "queue", "num",
                &q, "bypass",
            ],
        )?;
    }
    if !udp_ports.is_empty() {
        let set = format!("{{{}}}", udp_ports.join(","));
        run(
            "nft",
            &[
                "add", "rule", "inet", NFT_TABLE, "post", "udp", "dport", &set, "queue", "num",
                &q, "bypass",
            ],
        )?;
    }
    Ok(())
}

fn down_nft() -> Result<(), String> {
    // delete table сносит цепочку и все её правила разом.
    run("nft", &["delete", "table", "inet", NFT_TABLE])
}

// ── iptables (fallback) ──────────────────────────────────────────────────────

/// Параллельный ip6tables-бинарь для выбранного iptables (если есть и работает).
fn ip6_variant(prog: &str) -> Option<&'static str> {
    let candidate = match prog {
        "iptables-nft" => "ip6tables-nft",
        "iptables-legacy" => "ip6tables-legacy",
        _ => "ip6tables",
    };
    cmd_ok(candidate, &["-S"]).then_some(candidate)
}

/// Порты в формате iptables multiport: диапазон через двоеточие (`19294:19344`),
/// в отличие от nft (дефис).
fn iptables_ports(ports: &[String]) -> String {
    ports
        .iter()
        .map(|p| p.replace('-', ":"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Создать выделенную цепочку ZAPRUST с правилами перехвата и переходом в неё из
/// POSTROUTING. Цепочка делает teardown НЕзависимым от набора портов.
fn setup_iptables_chain(
    prog: &str,
    q: &str,
    tcp_ports: &[String],
    udp_ports: &[String],
) -> Result<(), String> {
    let _ = run(prog, &["-t", "mangle", "-N", IPT_CHAIN]); // создать (ок, если уже есть)
    let _ = run(prog, &["-t", "mangle", "-F", IPT_CHAIN]); // очистить от прежних правил

    if !tcp_ports.is_empty() {
        let csv = iptables_ports(tcp_ports);
        run(
            prog,
            &[
                "-t", "mangle", "-A", IPT_CHAIN, "-p", "tcp", "-m", "multiport", "--dports", &csv,
                "-j", "NFQUEUE", "--queue-num", q, "--queue-bypass",
            ],
        )?;
    }
    if !udp_ports.is_empty() {
        let csv = iptables_ports(udp_ports);
        run(
            prog,
            &[
                "-t", "mangle", "-A", IPT_CHAIN, "-p", "udp", "-m", "multiport", "--dports", &csv,
                "-j", "NFQUEUE", "--queue-num", q, "--queue-bypass",
            ],
        )?;
    }
    // Переход из POSTROUTING — только если его ещё нет (без дублей).
    if run(prog, &["-t", "mangle", "-C", "POSTROUTING", "-j", IPT_CHAIN]).is_err() {
        run(prog, &["-t", "mangle", "-A", "POSTROUTING", "-j", IPT_CHAIN])?;
    }
    Ok(())
}

fn up_iptables(prog: &str, qnum: u16, tcp_ports: &[String], udp_ports: &[String]) -> Result<(), String> {
    down_iptables(prog); // best-effort чистка прежних
    let q = qnum.to_string();
    setup_iptables_chain(prog, &q, tcp_ports, udp_ports)?; // IPv4 — обязателен
    if let Some(ip6) = ip6_variant(prog) {
        if let Err(e) = setup_iptables_chain(ip6, &q, tcp_ports, udp_ports) {
            logging::warn("nft", format!("ip6tables правила не поставлены (не критично): {e}"));
        }
    }
    Ok(())
}

/// Снять нашу цепочку у одного бинаря: убрать переход(ы) из POSTROUTING, очистить
/// и удалить цепочку. Порт-независимо (в отличие от точечного -D по спеку).
fn teardown_iptables_chain(prog: &str) {
    // Переход мог добавиться несколько раз — снимаем, пока есть.
    for _ in 0..8 {
        if run(prog, &["-t", "mangle", "-C", "POSTROUTING", "-j", IPT_CHAIN]).is_err() {
            break;
        }
        let _ = run(prog, &["-t", "mangle", "-D", "POSTROUTING", "-j", IPT_CHAIN]);
    }
    let _ = run(prog, &["-t", "mangle", "-F", IPT_CHAIN]);
    let _ = run(prog, &["-t", "mangle", "-X", IPT_CHAIN]);
}

fn down_iptables(prog: &str) {
    teardown_iptables_chain(prog);
    if let Some(ip6) = ip6_variant(prog) {
        teardown_iptables_chain(ip6);
    }
}
