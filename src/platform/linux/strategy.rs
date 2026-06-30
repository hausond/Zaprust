// Источник стратегий на Linux. Два источника, в порядке приоритета:
//
//   1. Стратегии Flowseal из каталога ядра (`general*.bat`) — РЕАЛЬНЫЕ боевые
//      стратегии, те же, что на Windows. Парсятся адаптером `crate::bat`, который
//      снимает виндовую специфику: `--wf-tcp/--wf-udp` (фильтр WinDivert) вынимает
//      в порты захвата NFQUEUE (правила nft), правит пути `%BIN%`/`%LISTS%`, а
//      `%GameFilter%` оставляет на резолв в момент запуска. Набор desync-флагов у
//      nfqws тот же, что у winws (winws — порт nfqws), а payload’ы (.bin) и списки
//      платформо-независимы — works as-is.
//
//   2. ВСТРОЕННЫЙ курируемый набор (этот файл) — фолбэк, когда ядра Flowseal нет.
//      Самодостаточен: использует встроенные fake-пейлоады nfqws (md5sig/badseq/
//      ttl, fake без внешних .bin), поэтому работает сразу после установки одного
//      бинаря nfqws, без файлов-списков и payload’ов.
//
// Общее правило: номер очереди `--qnum` добавляет движок (`runtime`), а НЕ
// стратегия — в аргументах его здесь нет; перехват портов задаёт `nft`, а не
// аргументы демона.

use std::path::{Path, PathBuf};

use crate::platform::Paths;
use crate::strategies::{CoreScan, Strategy};
use super::runtime;

/// Диапазон «игровых» портов для Game Filter (высокие порты, куда ходят игры).
/// Совпадает по смыслу с тем, что подставлял `%GameFilter%` на Windows.
pub const GAME_PORTS: &str = "1024-65535";

/// Одна запись курируемого набора: имя, группа и аргументы `nfqws` (без `--qnum`).
struct Preset {
    name: &'static str,
    group: &'static str,
    args: &'static [&'static str],
}

/// Курируемый набор Linux-стратегий (нативный bol-van/zapret).
///
/// Каждая стратегия — три профиля, разделённые `--new`: HTTP (tcp/80),
/// TLS (tcp/443) и QUIC (udp/443). Порядок в списке = порядок групп в дропдауне
/// (general → ALT → FAKE TLS → Simple Fake), как на Windows.
const PRESETS: &[Preset] = &[
    // ── general ──────────────────────────────────────────────────────────────
    Preset {
        name: "general",
        group: "general",
        args: &[
            "--filter-tcp=80", "--dpi-desync=fake,multisplit",
            "--dpi-desync-split-pos=method+2", "--dpi-desync-fooling=md5sig", "--new",
            "--filter-tcp=443", "--dpi-desync=fake,multidisorder",
            "--dpi-desync-split-pos=1,midsld", "--dpi-desync-repeats=6",
            "--dpi-desync-fooling=md5sig", "--new",
            "--filter-udp=443", "--dpi-desync=fake", "--dpi-desync-repeats=6",
        ],
    },
    // ── TTL (fake) ─────────────────────────────────────────────────────────────
    // Семейство «низкий TTL у fake-пакета»: фейк доходит до DPI, но НЕ до сервера
    // (умирает по TTL). Лучше всего против TTL-чувствительных DPI (МГТС и родня).
    // На тест-провайдере из всего набора пробил YouTube только этот подход —
    // поэтому вариаций здесь несколько: разный TTL и обработка QUIC для Discord.
    Preset {
        name: "TTL fake (ttl=1)",
        group: "TTL (fake)",
        args: &[
            "--filter-tcp=80", "--dpi-desync=fake,fakedsplit", "--dpi-desync-ttl=1", "--new",
            "--filter-tcp=443", "--dpi-desync=fake,multidisorder",
            "--dpi-desync-split-pos=1,midsld", "--dpi-desync-ttl=1", "--new",
            "--filter-udp=443", "--dpi-desync=fake", "--dpi-desync-ttl=1",
            "--dpi-desync-repeats=6",
        ],
    },
    Preset {
        name: "TTL fake (ttl=2)",
        group: "TTL (fake)",
        args: &[
            "--filter-tcp=80", "--dpi-desync=fake,fakedsplit", "--dpi-desync-ttl=2", "--new",
            "--filter-tcp=443", "--dpi-desync=fake,multidisorder",
            "--dpi-desync-split-pos=1,midsld", "--dpi-desync-ttl=2", "--new",
            "--filter-udp=443", "--dpi-desync=fake", "--dpi-desync-ttl=2",
            "--dpi-desync-repeats=6",
        ],
    },
    Preset {
        name: "TTL fake (autottl)",
        group: "TTL (fake)",
        args: &[
            "--filter-tcp=80", "--dpi-desync=fake,fakedsplit", "--dpi-desync-autottl", "--new",
            "--filter-tcp=443", "--dpi-desync=fake,multidisorder",
            "--dpi-desync-split-pos=1,midsld", "--dpi-desync-autottl", "--new",
            "--filter-udp=443", "--dpi-desync=fake", "--dpi-desync-autottl",
            "--dpi-desync-repeats=6",
        ],
    },
    // Discord-профиль: TTL=1 как у рабочего YouTube + спрятать SNI у TLS-фейка
    // (fake-tls-mod) + явная обработка QUIC/STUN по L7-фильтру (Discord активно
    // ходит по QUIC и голосу/STUN). Голос Discord часто блокируют по IP — это
    // desync не лечит, нужен ipset (вне рамок текущего шага).
    Preset {
        name: "Discord (ttl=1 + quic/stun)",
        group: "TTL (fake)",
        args: &[
            "--filter-tcp=80", "--dpi-desync=fake,fakedsplit", "--dpi-desync-ttl=1", "--new",
            "--filter-tcp=443", "--dpi-desync=fake,multidisorder",
            "--dpi-desync-split-pos=1,midsld", "--dpi-desync-ttl=1",
            "--dpi-desync-fake-tls-mod=rnd,dupsid", "--new",
            "--filter-udp=443", "--filter-l7=quic,discord,stun",
            "--dpi-desync=fake", "--dpi-desync-ttl=1", "--dpi-desync-repeats=8",
        ],
    },
    Preset {
        name: "general (ALT)",
        group: "ALT",
        args: &[
            "--filter-tcp=80", "--dpi-desync=fake,split2",
            "--dpi-desync-autottl=2", "--dpi-desync-fooling=badseq", "--new",
            "--filter-tcp=443", "--dpi-desync=fake,disorder2",
            "--dpi-desync-autottl=2", "--dpi-desync-fooling=badseq",
            "--dpi-desync-repeats=6", "--new",
            "--filter-udp=443", "--dpi-desync=fake", "--dpi-desync-repeats=8",
        ],
    },
    Preset {
        name: "general (ALT2)",
        group: "ALT",
        args: &[
            "--filter-tcp=80", "--dpi-desync=fakedsplit",
            "--dpi-desync-split-pos=2", "--dpi-desync-fooling=md5sig", "--new",
            "--filter-tcp=443", "--dpi-desync=fake,multidisorder",
            "--dpi-desync-split-pos=midsld", "--dpi-desync-fooling=md5sig",
            "--dpi-desync-repeats=8", "--new",
            "--filter-udp=443", "--dpi-desync=fake", "--dpi-desync-repeats=8",
        ],
    },
    Preset {
        name: "general (MGTS)",
        group: "ALT",
        args: &[
            "--filter-tcp=80", "--dpi-desync=fake,fakedsplit",
            "--dpi-desync-ttl=1", "--new",
            "--filter-tcp=443", "--dpi-desync=fake,multidisorder",
            "--dpi-desync-split-pos=midsld", "--dpi-desync-ttl=1", "--new",
            "--filter-udp=443", "--dpi-desync=fake", "--dpi-desync-repeats=6",
        ],
    },
    // ── FAKE TLS ───────────────────────────────────────────────────────────────
    Preset {
        name: "general (FAKE TLS)",
        group: "FAKE TLS",
        args: &[
            "--filter-tcp=80", "--dpi-desync=fake,multisplit",
            "--dpi-desync-split-pos=method+2", "--dpi-desync-fooling=md5sig", "--new",
            "--filter-tcp=443", "--dpi-desync=fake,multisplit",
            "--dpi-desync-split-pos=1,midsld", "--dpi-desync-fooling=md5sig",
            "--dpi-desync-repeats=6", "--new",
            "--filter-udp=443", "--dpi-desync=fake", "--dpi-desync-repeats=6",
        ],
    },
    // ── Simple Fake ─────────────────────────────────────────────────────────────
    Preset {
        name: "Simple Fake (TTL)",
        group: "Simple Fake",
        args: &[
            "--filter-tcp=80", "--dpi-desync=fake", "--dpi-desync-ttl=4", "--new",
            "--filter-tcp=443", "--dpi-desync=fake", "--dpi-desync-ttl=4", "--new",
            "--filter-udp=443", "--dpi-desync=fake", "--dpi-desync-ttl=4",
        ],
    },
    Preset {
        name: "Simple Fake (badseq)",
        group: "Simple Fake",
        args: &[
            "--filter-tcp=80", "--dpi-desync=fake", "--dpi-desync-fooling=badseq", "--new",
            "--filter-tcp=443", "--dpi-desync=fake", "--dpi-desync-fooling=badseq",
            "--dpi-desync-repeats=6", "--new",
            "--filter-udp=443", "--dpi-desync=fake", "--dpi-desync-repeats=6",
        ],
    },
];

/// Превратить запись набора в общий `Strategy` (тот же тип, что на Windows).
fn to_strategy(p: &Preset) -> Strategy {
    let args: Vec<String> = p.args.iter().map(|s| (*s).to_owned()).collect();
    Strategy {
        name: p.name.to_owned(),
        group: p.group.to_owned(),
        // На Linux источник встроенный, файла-исходника нет.
        source_path: std::path::PathBuf::new(),
        raw_args: args.join(" "),
        args,
        // Встроенные стратегии используют порты по умолчанию (80,443 / 443):
        // пустые списки ⇒ движок подставит дефолт.
        wf_tcp: Vec::new(),
        wf_udp: Vec::new(),
    }
}

/// Весь курируемый набор как `Vec<Strategy>` (для движка и автоподбора).
pub fn curated() -> Vec<Strategy> {
    PRESETS.iter().map(to_strategy).collect()
}

/// Файл вида `general*.bat` (без учёта регистра расширения).
fn is_general_bat(p: &Path) -> bool {
    let is_bat = p.extension().map(|e| e.eq_ignore_ascii_case("bat")).unwrap_or(false);
    let name_ok = p
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_lowercase().starts_with("general"))
        .unwrap_or(false);
    is_bat && name_ok
}

/// Найденный каталог ядра (если есть). Туда L8 ставит nfqws + payloads + lists +
/// стратегии Flowseal; пока пользователь/скрипт кладёт их сам.
fn core_dir() -> Option<PathBuf> {
    <super::LinuxPlatform as Paths>::core_dir(&super::LinuxPlatform)
}

/// Просканировать источник стратегий:
///   • если в каталоге ядра есть `general*.bat` Flowseal — распарсить их адаптером
///     `bat` (реальные стратегии с payload’ами/списками, как на Windows);
///   • иначе — встроенный курируемый набор (самодостаточный, без внешних файлов).
/// Отсутствие nfqws/ядра НЕ роняет приложение и НЕ прячет список (получение ядра —
/// L8): стратегии видны всегда, статус движка идёт сообщением.
pub fn scan() -> CoreScan {
    let mut messages = Vec::new();
    match runtime::find_nfqws() {
        Some(p) => messages.push(format!("движок: nfqws {}", p.display())),
        None => messages.push(
            "nfqws не найден — установите bol-van/zapret (автоустановка в L8)".to_owned(),
        ),
    }

    let core = core_dir();
    let mut strategies: Vec<Strategy> = Vec::new();

    if let Some(dir) = &core {
        let mut bats: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_general_bat(p))
            .collect();
        bats.sort();
        for p in &bats {
            match crate::bat::parse_flowseal(p, dir) {
                Ok(s) => strategies.push(s),
                Err(why) => messages.push(format!(
                    "пропущен {}: {why}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                )),
            }
        }
        crate::bat::sort_strategies(&mut strategies);
    }

    if strategies.is_empty() {
        strategies = curated();
        messages.push(format!(
            "ядро Flowseal не найдено — встроенные стратегии: {}",
            strategies.len()
        ));
    } else {
        messages.push(format!("стратегии Flowseal: {}", strategies.len()));
    }

    CoreScan {
        core_dir: core,
        strategies,
        messages,
    }
}

/// Значение вместо `%GameFilter*%`: диапазон игровых портов (вкл) либо порт-
/// заглушка `12` (выкл) — ровно как подставляет `service.bat` на Windows.
fn game_filter_value(on: bool) -> &'static str {
    if on {
        GAME_PORTS
    } else {
        "12"
    }
}

/// Резолв Game Filter в АРГУМЕНТАХ стратегии — подстановка `%GameFilterTCP/UDP%` /
/// `%GameFilter%` (как на Windows). У встроенных стратегий плейсхолдеров нет → no-op.
/// Перехват игровых портов задаётся отдельно правилами nft (см. `capture_ports`).
pub fn resolve_game_filter(args: &[String], on: bool) -> Vec<String> {
    let val = game_filter_value(on);
    args.iter()
        .map(|a| {
            a.replace("%GameFilterTCP%", val)
                .replace("%GameFilterUDP%", val)
                .replace("%GameFilter%", val)
        })
        .collect()
}

/// Порты захвата для NFQUEUE (правила nft) под выбранную стратегию:
///   • стратегия Flowseal (`wf_tcp`/`wf_udp` заданы) → её порты, с резолвом
///     `%GameFilter*%` (выкл → игровой токен выбрасывается);
///   • встроенная стратегия (wf пусты) → дефолт 80,443 / 443, а Game Filter
///     добавляет игровой диапазон.
/// Возвращает токены в формате nft (диапазоны через дефис).
pub fn capture_ports(strat: &Strategy, game_filter: bool) -> (Vec<String>, Vec<String>) {
    let resolve = |ports: &[String]| -> Vec<String> {
        ports
            .iter()
            .filter_map(|p| {
                if p.contains("%GameFilter") {
                    game_filter.then(|| GAME_PORTS.to_owned())
                } else {
                    Some(p.clone())
                }
            })
            .collect()
    };

    if strat.wf_tcp.is_empty() && strat.wf_udp.is_empty() {
        let mut tcp = vec!["80".to_owned(), "443".to_owned()];
        let mut udp = vec!["443".to_owned()];
        if game_filter {
            tcp.push(GAME_PORTS.to_owned());
            udp.push(GAME_PORTS.to_owned());
        }
        (tcp, udp)
    } else {
        (resolve(&strat.wf_tcp), resolve(&strat.wf_udp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flowseal(name: &str, wf_tcp: &[&str], wf_udp: &[&str]) -> Strategy {
        Strategy {
            name: name.to_owned(),
            group: "ALT".to_owned(),
            source_path: PathBuf::new(),
            args: vec!["--filter-tcp=%GameFilterTCP%".to_owned(), "--dpi-desync=fake".to_owned()],
            raw_args: String::new(),
            wf_tcp: wf_tcp.iter().map(|s| s.to_string()).collect(),
            wf_udp: wf_udp.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn curated_is_nonempty_and_well_formed() {
        let all = curated();
        assert!(all.len() >= 5, "ожидали несколько стратегий, есть {}", all.len());
        for s in &all {
            assert!(!s.name.is_empty());
            assert!(!s.group.is_empty(), "пустая группа у {}", s.name);
            assert!(!s.args.is_empty(), "пустые аргументы у {}", s.name);
            assert!(
                !s.args.iter().any(|a| a.contains("--qnum")),
                "стратегия {} не должна нести --qnum",
                s.name
            );
            assert!(
                s.args.iter().any(|a| a.starts_with("--dpi-desync")),
                "стратегия {} без desync-флагов",
                s.name
            );
        }
    }

    #[test]
    fn builtin_capture_defaults_and_game_filter() {
        let s = curated().into_iter().next().unwrap(); // встроенная: wf пусты
        let (tcp, udp) = capture_ports(&s, false);
        assert_eq!(tcp, vec!["80", "443"]);
        assert_eq!(udp, vec!["443"]);
        let (tcp_g, udp_g) = capture_ports(&s, true);
        assert!(tcp_g.contains(&GAME_PORTS.to_owned()) && udp_g.contains(&GAME_PORTS.to_owned()));
    }

    #[test]
    fn flowseal_capture_resolves_game_filter_token() {
        let s = flowseal("general (ALT)", &["80", "443", "%GameFilterTCP%"], &["443", "%GameFilterUDP%"]);
        // выкл → игровой токен выброшен.
        let (tcp_off, udp_off) = capture_ports(&s, false);
        assert_eq!(tcp_off, vec!["80", "443"]);
        assert_eq!(udp_off, vec!["443"]);
        // вкл → игровой токен → диапазон.
        let (tcp_on, _udp_on) = capture_ports(&s, true);
        assert_eq!(tcp_on, vec!["80", "443", GAME_PORTS]);
    }

    #[test]
    fn resolve_game_filter_substitutes_placeholders() {
        let args = vec!["--filter-tcp=%GameFilterTCP%".to_owned()];
        assert_eq!(resolve_game_filter(&args, false), vec!["--filter-tcp=12"]);
        assert_eq!(resolve_game_filter(&args, true), vec![format!("--filter-tcp={GAME_PORTS}")]);
        // Без плейсхолдеров — без изменений.
        let plain = vec!["--filter-tcp=443".to_owned()];
        assert_eq!(resolve_game_filter(&plain, true), plain);
    }
}
