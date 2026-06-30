// Движок обхода на Linux: демон `nfqws` (из bol-van/zapret, линуксовый родственник
// winws). В отличие от Windows, nfqws сам НИЧЕГО не перехватывает — трафик в него
// загоняют правила nftables/iptables (модуль `nft`) через очередь NFQUEUE.
//
// Поэтому жизненный цикл — ДВА действия в строгом порядке:
//   Старт = поднять правила перехвата → запустить nfqws (--qnum совпадает с правилом);
//   Стоп  = заглушить nfqws → снять правила (обратный порядок), без висящих правил.
//
// На шаге L2 запуск — из-под root ВРУЧНУЮ (foreground, чтобы видеть вывод). Службу
// (systemd) даст L5, элевацию (pkexec) — L4, источник стратегий bol-van — L3.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::nft::{self, Backend};
use super::strategy;
use crate::logging;
use crate::platform::{EngineCommand, ProbeHandle};
use crate::strategies::Strategy;

/// Номер очереди NFQUEUE. ДОЛЖЕН совпадать в правиле nftables/iptables и в
/// аргументе `--qnum` nfqws — иначе демон не получит ни одного пакета.
pub const QNUM: u16 = 200;

/// Возможные расположения бинаря nfqws (явные пути; PATH проверяем отдельно).
fn candidate_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    // Куда L8 будет ставить ядро: <core>/nfqws. core_base уважает SUDO_USER/PKEXEC_UID,
    // чтобы под root искать в домашнем каталоге позвавшего, а не в /root.
    if let Some(core) = super::core_base() {
        v.push(core.join("nfqws"));
    }
    for p in [
        "/usr/local/sbin/nfqws",
        "/usr/sbin/nfqws",
        "/usr/local/bin/nfqws",
        "/usr/bin/nfqws",
        "/opt/zapret/nfqws",
    ] {
        v.push(PathBuf::from(p));
    }
    v
}

/// Найти бинарь nfqws: сперва известные пути, затем PATH.
pub fn find_nfqws() -> Option<PathBuf> {
    for c in candidate_paths() {
        if c.is_file() {
            return Some(c);
        }
    }
    // PATH через `command -v` (надёжнее, чем полагаться на наличие `which`).
    if let Ok(out) = Command::new("sh").arg("-c").arg("command -v nfqws").output() {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    None
}

/// Создать недостающие файлы списков/ipset, на которые ссылаются аргументы
/// (`--hostlist=…`, `--hostlist-exclude=…`, `--ipset=…`, `--ipset-exclude=…`). Без
/// них nfqws падает при открытии. Создаём ПУСТЫМИ (пустой hostlist/ipset ничего не
/// матчит — безопасно). Аналог `ensure_user_lists` на Windows. Payload-файлы (.bin)
/// НЕ трогаем — они реальные и должны существовать.
fn ensure_lists(args: &[String]) {
    const KEYS: [&str; 4] = [
        "--hostlist=",
        "--hostlist-exclude=",
        "--ipset=",
        "--ipset-exclude=",
    ];
    for a in args {
        let Some(path) = KEYS.iter().find_map(|k| a.strip_prefix(k)) else {
            continue;
        };
        let p = Path::new(path);
        if p.exists() {
            continue;
        }
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(p, b"") {
            Ok(_) => logging::info("lists", format!("создан недостающий список: {path}")),
            Err(e) => logging::warn("lists", format!("не создать список {path}: {e}")),
        }
    }
}

/// Строка диагностики готовности движка (для шапки лога и «Диагностики»).
pub fn diag() -> String {
    match find_nfqws() {
        Some(p) => {
            let fw = nft::detect()
                .map(|b| b.label())
                .unwrap_or_else(|| "НЕТ (нет nft/iptables!)".to_owned());
            format!("nfqws={} · фаервол={} · qnum={QNUM}", p.display(), fw)
        }
        None => "nfqws не найден — установите bol-van/zapret (автоустановка в L8)".to_owned(),
    }
}

/// Команда запуска nfqws для стратегии: бинарь + `--qnum` + аргументы стратегии
/// (с разрешённым Game Filter). None — если nfqws не найден. `--qnum` добавляет
/// движок (совпадает с правилом nft), а НЕ сама стратегия — не дублируем.
/// (Потребляется systemd-юнитом в L5 и автоподбором.)
pub fn engine_command(strat: &Strategy, game_filter: bool) -> Option<EngineCommand> {
    let nfqws = find_nfqws()?;
    let cwd = nfqws
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/"));
    let mut args = vec![format!("--qnum={QNUM}")];
    args.extend(strategy::resolve_game_filter(&strat.args, game_filter));
    Some(EngineCommand {
        program: nfqws,
        cwd,
        args,
    })
}

/// Файл, куда пишется вывод nfqws при пробах (для разбора кода выхода).
fn output_path() -> PathBuf {
    std::env::temp_dir().join("zaprust_nfqws.out")
}

/// Последние строки вывода nfqws (его собственная диагностика).
pub fn last_engine_output() -> String {
    std::fs::read_to_string(output_path())
        .map(|t| {
            t.lines()
                .filter(|l| !l.trim().is_empty())
                .rev()
                .take(12)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_default()
}

/// Активный прогон: запущенный nfqws + поднятые ПОД НЕГО правила перехвата.
/// При `Drop` сворачивает всё в обратном порядке: глушит демон → снимает правила.
/// Не оставляет висящих правил даже если процесс роняют.
pub struct EngineRun {
    child: Child,
    backend: Backend,
}

impl EngineRun {
    fn shutdown(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Err(e) = nft::down(self.backend) {
            // Целевое снятие не прошло — сносим всё, что могли поставить, любым
            // бэкендом. Лучше снять лишнее, чем оставить правила висеть на сети.
            logging::warn("engine", format!("снятие правил перехвата: {e}; чищу все бэкенды"));
            nft::down_all();
        } else {
            logging::info("engine", "правила перехвата сняты");
        }
    }
}

impl Drop for EngineRun {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl ProbeHandle for EngineRun {
    fn try_exit(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(st)) => Some(st.code().unwrap_or(-1)),
            _ => None,
        }
    }
}

/// Поднять движок: правила перехвата → nfqws. Если демон не запустился, снять уже
/// поставленные правила (не оставлять систему в полу-состоянии). `inherit` —
/// прокинуть вывод nfqws в терминал (foreground-проверка L2); иначе писать в
/// temp-файл (диагностика проб автоподбора).
fn bring_up(strat: &Strategy, game_filter: bool, inherit: bool) -> Result<EngineRun, String> {
    let nfqws = find_nfqws().ok_or_else(|| {
        "nfqws не найден — установите bol-van/zapret (автоустановка в L8)".to_owned()
    })?;
    let backend = nft::detect()
        .ok_or_else(|| "не найден nft/iptables для правил перехвата".to_owned())?;

    // 0) ЧИСТЫЙ СТАРТ: убить любой осиротевший nfqws и снести все наши правила от
    //    прошлых прогонов. Иначе чужой демон остаётся привязан к очереди 200 и
    //    вердиктит трафик параллельно нашему — связка «живой сирота + правила»
    //    рвёт весь tcp 80/443 + udp 443 (вплоть до полной потери сети). Сирота
    //    появляется, если прошлый прогон завершили не штатно (закрыли терминал).
    force_down();

    // 1) Правила перехвата — ПЕРВЫМ действием. Порты захвата берём из стратегии
    //    (Flowseal: её --wf-*; встроенные: дефолт 80,443/443), с резолвом Game
    //    Filter. БЕЗ нужных портов nfqws не получит, например, пакеты Discord-медиа.
    let (tcp_ports, udp_ports) = strategy::capture_ports(strat, game_filter);
    nft::up(backend, QNUM, &tcp_ports, &udp_ports).map_err(|e| format!("правила перехвата: {e}"))?;
    logging::info(
        "engine",
        format!(
            "правила перехвата подняты ({}), qnum={QNUM}, tcp=[{}] udp=[{}]",
            backend.label(),
            tcp_ports.join(","),
            udp_ports.join(",")
        ),
    );

    // 2) Демон nfqws (аргументы стратегии с разрешённым Game Filter).
    let args = strategy::resolve_game_filter(&strat.args, game_filter);
    // Стратегии Flowseal ссылаются на пользовательские списки (*-user.txt), которые
    // обычно создаёт service.bat. Мы их создаём сами, иначе nfqws падает на
    // отсутствующем --hostlist/--ipset-файле.
    ensure_lists(&args);
    let mut cmd = Command::new(&nfqws);
    cmd.arg(format!("--qnum={QNUM}"));
    // nfqws после привязки очереди сбрасывает root-привилегии. По умолчанию — на
    // nobody, который НЕ прочитает файлы ядра в домашнем каталоге пользователя
    // (хостлисты, payload’ы). Сбрасываем на ПОЗВАВШЕГО пользователя — он владелец
    // этих файлов. (Под sudo/pkexec; иначе nfqws и так не root и --uid не нужен.)
    if let Some((uid, gid)) = super::invoking_user_ids() {
        cmd.arg(format!("--uid={uid}:{gid}"));
    }
    cmd.args(&args);
    cmd.stdin(Stdio::null());
    if inherit {
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        match std::fs::File::create(output_path()) {
            Ok(f) => {
                let err = f.try_clone().map(Stdio::from).unwrap_or_else(|_| Stdio::null());
                cmd.stdout(Stdio::from(f)).stderr(err);
            }
            Err(_) => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
    }

    match cmd.spawn() {
        Ok(child) => {
            logging::info("engine", format!("nfqws запущен: {}", nfqws.display()));
            Ok(EngineRun { child, backend })
        }
        Err(e) => {
            // Откат: снять уже поставленные правила, не оставлять полу-состояние.
            let _ = nft::down(backend);
            Err(format!("nfqws не запустился: {e} (правила сняты)"))
        }
    }
}

/// Поднять движок одиночным прогоном для пробы стратегии (автоподбор, L7).
pub fn spawn_probe(strat: &Strategy, game_filter: bool) -> std::io::Result<Box<dyn ProbeHandle>> {
    bring_up(strat, game_filter, false)
        .map(|r| Box::new(r) as Box<dyn ProbeHandle>)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
}

/// Аварийная/ручная чистка: заглушить любой nfqws + снять все наши правила.
/// Используется для `reset_engine` и `--engine-down`.
pub fn force_down() {
    let _ = Command::new("pkill").args(["-x", "nfqws"]).output();
    nft::down_all();
}

/// L7: страховка свипа автоподбора от внезапной гибели root-реинвока. Во время
/// свипа эфемерный nfqws каждой пробы — локальная переменная (`EngineRun`); её
/// `Drop` снимает демон и правила только при ШТАТНОМ выходе. Если же root-процесс
/// `--autoselect` прибьют сигналом (SIGTERM/SIGINT, или SIGHUP при закрытии
/// терминала-родителя), Drop не отработает — и останется висящий nfqws + правила
/// на tcp 80/443 + udp 443, что (см. инцидент L2) рвёт ВСЮ сеть до перезагрузки.
/// Поэтому на старте свипа ставим обработчик: при сигнале — снять движок и выйти.
/// Фича ctrlc "termination" ловит и SIGTERM, и SIGHUP. Кооперативная «Отмена»
/// (флаг-файл) — отдельный, штатный путь; это лишь сетка под аварию.
///
/// Идемпотентно по факту: `set_handler` можно звать раз на процесс, а реинвок
/// `--autoselect` — свежий процесс без других обработчиков, так что конфликта нет;
/// ошибку (если вдруг уже стоит) лишь логируем.
pub fn install_sweep_guard() {
    if let Err(e) = ctrlc::set_handler(|| {
        // Бежит в отдельном потоке ctrlc (не из raw-обработчика), поэтому звать
        // обычный код безопасно: глушим nfqws + сносим все наши правила и выходим.
        force_down();
        std::process::exit(2);
    }) {
        logging::warn("autoselect", format!("не удалось поставить страховочный обработчик сигналов: {e}"));
    }
}

/// L2: ручной foreground-прогон движка из-под root. Поднимает правила + nfqws
/// (вывод демона идёт в терминал), держит до Ctrl-C/Enter или раннего выхода
/// демона, затем сворачивает всё (демон + правила). Блокирующий.
pub fn run_foreground(strat: &Strategy, game_filter: bool) -> Result<(), String> {
    let mut run = bring_up(strat, game_filter, true)?;

    println!();
    println!("──────────────────────────────────────────────────────");
    println!("  Движок ЗАПУЩЕН: правила перехвата + nfqws подняты.");
    println!("  Стратегия: {}", strat.name);
    println!("  Проверьте YouTube / Discord — должны открываться.");
    println!("  Нажмите Enter или Ctrl-C, чтобы остановить и снять правила.");
    println!("──────────────────────────────────────────────────────");
    println!();

    let stop = Arc::new(AtomicBool::new(false));
    // Ctrl-C / SIGTERM / SIGHUP → флаг (чтобы снять правила в Drop, а не умереть по
    // умолчанию). Фича ctrlc "termination" ловит и закрытие терминала (SIGHUP), и
    // kill (SIGTERM) — иначе они оставляли бы живой nfqws + правила и рвали сеть.
    {
        let stop = stop.clone();
        if let Err(e) = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst)) {
            logging::warn("engine", format!("не удалось перехватить сигнал остановки: {e}"));
        }
    }
    // Enter в отдельном потоке → тот же флаг.
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::stdin().read_line(&mut s);
            stop.store(true, Ordering::SeqCst);
        });
    }

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if let Some(code) = run.try_exit() {
            println!("nfqws неожиданно завершился (код {code}). Снимаю правила…");
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    drop(run); // глушит nfqws + снимает правила
    println!("Остановлено: nfqws заглушён, правила перехвата сняты.");
    Ok(())
}

/// L5: тело systemd-службы (`ExecStart=… --svc-run`). Как `run_foreground`, но БЕЗ
/// интерактива: не читает stdin (под systemd он /dev/null — чтение тут же дало бы
/// EOF и мгновенный «стоп») и не печатает приглашений. Поднимает правила + nfqws
/// (вывод идёт в journald), держит на переднем плане, пока systemd не пришлёт
/// SIGTERM (Стоп) — фича ctrlc "termination" ловит его и роняет stop-флаг → Drop
/// снимает демон и правила. Если nfqws падает сам — выходим с его кодом (systemd
/// пометит юнит failed), правила всё равно снимаются Drop'ом. Блокирующий.
pub fn run_service(strat: &Strategy, game_filter: bool) -> Result<(), String> {
    let mut run = bring_up(strat, game_filter, true)?;
    logging::info("service", format!("служба поднята · стратегия={}", strat.name));

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        if let Err(e) = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst)) {
            logging::warn("service", format!("не удалось перехватить сигнал остановки: {e}"));
        }
    }

    loop {
        if stop.load(Ordering::SeqCst) {
            logging::info("service", "получен сигнал остановки — снимаю движок и правила");
            break;
        }
        if let Some(code) = run.try_exit() {
            // nfqws завершился сам — снять правила (Drop) и пробросить код наружу,
            // чтобы systemd честно пометил юнит (Restart=on-failure подхватит).
            drop(run);
            if code == 0 {
                return Ok(());
            }
            return Err(format!("nfqws завершился с кодом {code}; правила сняты"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    drop(run); // глушит nfqws + снимает правила
    logging::info("service", "служба остановлена: nfqws заглушён, правила сняты");
    Ok(())
}
