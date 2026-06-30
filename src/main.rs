// Zaprust — лёгкий нативный GUI поверх сборки Flowseal/zapret-discord-youtube.
//
// Шаг 1: только визуальный каркас. Виджеты переключают локальное состояние,
// но никакой системной логики (процессы, файлы, сеть) здесь нет.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Адаптер стратегий Flowseal (.bat) под nfqws — только на Linux (Windows парсит
// .bat своим путём в platform/windows/strategy.rs).
#[cfg(not(windows))]
mod bat;
mod config;
mod logging;
mod platform;
mod strategies;
mod updater;

// host() возвращает `&dyn Platform`; методы трейтов-слоёв (ServiceController,
// BypassRuntime, Tester, …) доступны на объекте-трейте без отдельного импорта.
use platform::{host, ServiceState};

use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Align, Color32, Layout, Margin, RichText, Rounding, Vec2};

// ── Палитра ────────────────────────────────────────────────────────────────
const ACCENT: Color32 = Color32::from_rgb(56, 142, 60); // зелёный — «работает»
const DANGER: Color32 = Color32::from_rgb(198, 64, 64); // красный — «стоп»
const OK: Color32 = Color32::from_rgb(46, 160, 67);
const MUTED: Color32 = Color32::from_rgb(96, 100, 108); // серый — «неактивно»
const PANEL_BG: Color32 = Color32::from_rgb(24, 26, 30);
const FIELD_BG: Color32 = Color32::from_rgb(32, 35, 40);
const LOG_BG: Color32 = Color32::from_rgb(18, 19, 22);

// ── Иконки интерфейса (PNG → egui-текстуры) ──────────────────────────────────

/// Загруженные текстуры иконок.
struct Icons {
    app: egui::TextureHandle,
    play: egui::TextureHandle,
    stop: egui::TextureHandle,
    download: egui::TextureHandle,
    refresh: egui::TextureHandle,
    settings: egui::TextureHandle,
    back: egui::TextureHandle,
    chevron_down: egui::TextureHandle,
    chevron_up: egui::TextureHandle,
    cancel: egui::TextureHandle,
    test: egui::TextureHandle,
}

fn load_icon_texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> egui::TextureHandle {
    let img = image::load_from_memory(bytes)
        .map(|i| i.to_rgba8())
        .unwrap_or_else(|_| image::RgbaImage::new(1, 1));
    let (w, h) = img.dimensions();
    let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
    ctx.load_texture(name, color, egui::TextureOptions::LINEAR)
}

impl Icons {
    fn load(ctx: &egui::Context) -> Self {
        macro_rules! ic {
            ($f:literal) => {
                load_icon_texture(ctx, $f, include_bytes!(concat!("../assets/icons/", $f)))
            };
        }
        Self {
            app: ic!("icon-app-64.png"),
            play: ic!("icon-play.png"),
            stop: ic!("icon-stop.png"),
            download: ic!("icon-download.png"),
            refresh: ic!("icon-refresh.png"),
            settings: ic!("icon-settings.png"),
            back: ic!("icon-back.png"),
            chevron_down: ic!("icon-chevron-down.png"),
            chevron_up: ic!("icon-chevron-up.png"),
            cancel: ic!("icon-cancel.png"),
            test: ic!("icon-test.png"),
        }
    }
}

/// SizedTexture для иконки точного размера (px×px).
fn sized(h: &egui::TextureHandle, px: f32) -> egui::load::SizedTexture {
    egui::load::SizedTexture::new(h.id(), egui::vec2(px, px))
}

/// Иконка окна/таскбара из icon-app-256.png.
fn load_window_icon() -> egui::IconData {
    let bytes = include_bytes!("../assets/icons/icon-app-256.png");
    match image::load_from_memory(bytes).map(|i| i.to_rgba8()) {
        Ok(img) => {
            let (width, height) = img.dimensions();
            egui::IconData {
                rgba: img.into_raw(),
                width,
                height,
            }
        }
        Err(_) => egui::IconData {
            rgba: vec![0, 0, 0, 0],
            width: 1,
            height: 1,
        },
    }
}

fn main() -> eframe::Result<()> {
    let cli: Vec<String> = std::env::args().collect();

    // Логгер + panic-hook — ПЕРВОЙ строкой, до любой логики, чтобы даже
    // мгновенная смерть процесса оставила след. Роль — по подкоманде.
    let role = match cli.get(1).map(|s| s.as_str()) {
        Some("--svc") | Some("--svc-run") | Some("--write-list") => "svc",
        Some("--autoselect") => "autoselect",
        Some("--apply-update") => "update",
        Some(s) if s.starts_with("--") => "diag",
        _ => "gui",
    };
    logging::init(role);
    log_env_header();

    // Диагностический режим: `zaprust --dump-args "general (ALT10)"`
    // печатает итоговый argv winws (Game Filter выкл) и выходит.
    if cli.get(1).map(|s| s == "--dump-args").unwrap_or(false) {
        dump_args(cli.get(2).map(|s| s.as_str()));
        return Ok(());
    }
    // `zaprust --test-run "general (ALT10)"` — реально спавнит winws,
    // ждёт ~4с, глушит и печатает его вывод (диагностика запуска).
    if cli.get(1).map(|s| s == "--test-run").unwrap_or(false) {
        test_run(cli.get(2).map(|s| s.as_str()));
        return Ok(());
    }
    // `zaprust --test-net` — детект WinDivert + прогон теста доменов в консоль.
    if cli.get(1).map(|s| s == "--test-net").unwrap_or(false) {
        test_net();
        return Ok(());
    }
    // `zaprust --diag` — печать той же диагностики, что и кнопка «Скопировать
    // диагностику», в консоль (без GUI). Полезно для проверки сборки, в т.ч.
    // внутри AppImage: показывает $APPIMAGE и цель pkexec-реинвока (L9-засада).
    if cli.get(1).map(|s| s == "--diag").unwrap_or(false) {
        for line in host().diag_lines() {
            println!("{line}");
        }
        return Ok(());
    }
    // `zaprust --install-desktop` — установить/обновить .desktop-entry и иконку
    // приложения в пользовательских XDG-каталогах (то же, что делается при старте
    // GUI). Полезно для переустановки иконки в доке/таскбаре без запуска окна.
    if cli.get(1).map(|s| s == "--install-desktop").unwrap_or(false) {
        host().integrate_desktop();
        println!("desktop-интеграция выполнена (см. лог)");
        return Ok(());
    }
    // `zaprust --update-dry` — безопасная проверка апдейтера: скачать и применить
    // замену ядра во временную папку (рабочее ядро не трогаем).
    if cli.get(1).map(|s| s == "--update-dry").unwrap_or(false) {
        update_dry();
        return Ok(());
    }
    // `zaprust --fetch-core [dir]` — L8-диагностика получения ядра: выполнить тот же
    // путь, что кнопка «Скачать ядро» (Linux: ассеты Flowseal + движок nfqws bol-van),
    // без прав и без GUI. Без аргумента ставит в реальную папку ядра; с аргументом —
    // в указанную (для безопасной проверки, не трогая рабочее ядро).
    if cli.get(1).map(|s| s == "--fetch-core").unwrap_or(false) {
        fetch_core_diag(cli.get(2).map(|s| s.as_str()));
        return Ok(());
    }
    // `zaprust --engine-up [стратегия]` — L2 (Linux): ручной foreground-прогон
    // движка из-под root. Поднимает правила перехвата (nftables/iptables) + nfqws,
    // держит до Ctrl-C/Enter, затем снимает всё. Доказывает жизнеспособность обхода
    // до появления pkexec (L4) и systemd-службы (L5). На Windows вернёт ошибку
    // (там движок работает только службой).
    if cli.get(1).map(|s| s == "--engine-up").unwrap_or(false) {
        std::process::exit(engine_up_command(cli.get(2).map(|s| s.as_str())));
    }
    // `zaprust --engine-down` — аварийно снять правила перехвата и заглушить nfqws
    // (если процесс --engine-up убили жёстко и правила повисли).
    if cli.get(1).map(|s| s == "--engine-down").unwrap_or(false) {
        std::process::exit(engine_down_command());
    }
    // `zaprust --svc-run <стратегия> <gf>` — L5 (Linux): ТЕЛО systemd-службы
    // (`ExecStart`). Запускается самим systemd от root. Поднимает правила перехвата
    // + nfqws с выбранной стратегией и держит на переднем плане, пока systemd не
    // пришлёт SIGTERM, затем снимает всё. На Windows вернёт ошибку (там движок
    // запускает SCM напрямую через binPath).
    if cli.get(1).map(|s| s == "--svc-run").unwrap_or(false) {
        std::process::exit(svc_run_command(cli.get(2).map(|s| s.as_str()), cli.get(3).map(|s| s.as_str())));
    }
    // `zaprust --test-elevate [args…]` — L4-диагностика: со стороны GUI вызвать
    // реальный Elevator (pkexec-диалог), дождаться реинвока от root и прочитать
    // result-файл обратно. Без args реинвок делает `--svc start` (на Linux пока
    // упрётся в заглушку systemd → ok=false — это норм, проверяем сам механизм
    // элевации + хэндшейк, а не успех операции). Это проверка критерия L4.
    if cli.get(1).map(|s| s == "--test-elevate").unwrap_or(false) {
        std::process::exit(test_elevate(&cli[2..]));
    }
    // `zaprust --svc <install|remove|start|stop> …` — элевированные операции
    // со службой. Запускается из GUI через ShellExecute runas.
    if cli.get(1).map(|s| s == "--svc").unwrap_or(false) {
        std::process::exit(run_service_command(&cli[2..]));
    }
    // `zaprust --apply-update <zip> <tag> [strategy] [gf]` — элевированная
    // замена ядра: стоп службы → распаковка → перезапуск службы → версия.
    if cli.get(1).map(|s| s == "--apply-update").unwrap_or(false) {
        std::process::exit(apply_update_command(&cli[2..]));
    }
    // `zaprust --autoselect <gf> [last-known-good]` — элевированный автоподбор
    // рабочей стратегии (один UAC на весь свип), установка победителя службой.
    if cli.get(1).map(|s| s == "--autoselect").unwrap_or(false) {
        std::process::exit(autoselect_command(&cli[2..]));
    }
    // `zaprust --write-list <dest> <src>` — элевированная запись файла списка
    // (копирует src→dest), когда папка ядра непишема без прав.
    if cli.get(1).map(|s| s == "--write-list").unwrap_or(false) {
        std::process::exit(write_list_command(&cli[2..]));
    }

    // Зарегистрировать desktop-entry + иконку в системе, чтобы окружение
    // показывало иконку приложения в доке/таскбаре (на Wayland окно
    // сопоставляется с .desktop по app_id, см. ниже). Делается ДО создания окна,
    // идемпотентно и без прав root. На Windows — no-op.
    host().integrate_desktop();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([484.0, 644.0])
            .with_min_inner_size([484.0, 644.0])
            .with_max_inner_size([484.0, 644.0])
            .with_resizable(false)
            .with_icon(load_window_icon())
            // app_id фиксируем явно: на Wayland по нему окно сопоставляется с
            // .desktop-файлом (StartupWMClass=zaprust в AppImage), иначе иконка
            // в таскбаре/доке не подхватывается. На X11 winit использует его как
            // WM_CLASS. Значение совпадает с именем .desktop в образе.
            .with_app_id("zaprust")
            .with_title("Zaprust"),
        ..Default::default()
    };

    eframe::run_native(
        "Zaprust",
        native_options,
        Box::new(|cc| Ok(Box::new(ZaprustApp::new(cc)))),
    )
}

/// Диагностика: вывести итоговую команду движка для стратегии.
fn dump_args(name: Option<&str>) {
    let scan = host().scan();
    for m in &scan.messages {
        eprintln!("# {m}");
    }
    let target = name.unwrap_or("general");
    match scan.strategies.iter().find(|s| s.name == target) {
        Some(strat) => match host().engine_command(strat, false) {
            Some(cmd) => {
                println!("exe:  {}", cmd.program.display());
                println!("cwd:  {}", cmd.cwd.display());
                println!("argc: {}", cmd.args.len());
                for (i, a) in cmd.args.iter().enumerate() {
                    println!("[{i:02}] {a}");
                }
            }
            None => eprintln!("команда движка недоступна (ядро не найдено или не реализовано)"),
        },
        None => {
            eprintln!("стратегия не найдена: {target}");
            for s in &scan.strategies {
                eprintln!("  есть: {}", s.name);
            }
        }
    }
}

/// Диагностика апдейтера: скачать последний релиз и применить замену во
/// временную папку (не трогая рабочее ядро), проверить результат.
fn update_dry() {
    let agent = host().agent();
    let latest = match updater::check_latest(&agent) {
        Ok(l) => l,
        Err(e) => {
            println!("check_latest: {e}");
            return;
        }
    };
    println!("latest tag: {}", latest.tag);
    println!("zip url:    {}", latest.zip_url);

    let zip = std::env::temp_dir().join("zaprust_diag.zip");
    print!("скачиваю… ");
    let dl = host().download_agent();
    if let Err(e) = updater::download(&dl, &latest.zip_url, &zip, |_, _| {}) {
        println!("download: {e}");
        return;
    }
    let size = std::fs::metadata(&zip).map(|m| m.len()).unwrap_or(0);
    println!("ok, {size} байт");

    // Готовим временный «core» с фейковым пользовательским списком.
    let dest = std::env::temp_dir().join("zaprust_dry_core");
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(dest.join("lists")).unwrap();
    std::fs::write(
        dest.join("lists").join("list-general-user.txt"),
        "MY-CUSTOM-DOMAIN.example\n",
    )
    .unwrap();

    match updater::apply(&dest, &zip) {
        Ok(()) => {
            let _ = updater::write_version(&dest, &latest.tag);
            let bats = std::fs::read_dir(&dest)
                .map(|r| {
                    r.flatten()
                        .filter(|e| {
                            e.path().extension().map(|x| x == "bat").unwrap_or(false)
                        })
                        .count()
                })
                .unwrap_or(0);
            let user = std::fs::read_to_string(dest.join("lists").join("list-general-user.txt"))
                .unwrap_or_default();
            println!("apply → {}", dest.display());
            println!("  winws.exe:           {}", dest.join("bin").join("winws.exe").exists());
            println!("  *.bat в корне:        {bats}");
            println!("  version.txt:          {:?}", updater::local_version(&dest));
            println!("  user-list сохранён:   {}", user.contains("MY-CUSTOM-DOMAIN"));
        }
        Err(e) => println!("apply: {e}"),
    }
}

/// L8-диагностика: выполнить получение ядра (как кнопка «Скачать ядро») без GUI.
/// На Linux проверяет оба источника (Flowseal + nfqws). `dir` — куда ставить
/// (по умолчанию реальная папка ядра).
fn fetch_core_diag(dir: Option<&str>) {
    let target = dir
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| host().preferred_core_dir());
    println!("ставлю ядро в: {}", target.display());

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let printer = std::thread::spawn(move || {
        while let Ok(line) = rx.recv() {
            println!("  {line}");
        }
    });
    let result = download_core_impl(&target, &tx);
    drop(tx);
    let _ = printer.join();

    match result {
        Ok(msg) => {
            println!("итог: {msg}");
            println!("проверка раскладки core/:");
            println!("  nfqws:        {}", target.join("nfqws").is_file());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let exec = std::fs::metadata(target.join("nfqws"))
                    .map(|m| m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false);
                println!("  nfqws +x:     {exec}");
            }
            let bats = std::fs::read_dir(&target)
                .map(|r| r.flatten().filter(|e| e.path().extension().map(|x| x == "bat").unwrap_or(false)).count())
                .unwrap_or(0);
            println!("  *.bat в корне: {bats}");
            println!("  bin/*.bin:    {}", target.join("bin").is_dir());
            println!("  winws.exe выкинут: {}", !target.join("bin").join("winws.exe").exists());
            println!("  version.txt:       {:?}", updater::local_version(&target));
            #[cfg(not(windows))]
            println!("  nfqws-version.txt: {:?}", updater::local_nfqws_version(&target));
        }
        Err(e) => println!("ошибка: {e}"),
    }
}

/// Диагностика: состояние службы + прогон теста доменов в консоль.
fn test_net() {
    println!("Служба обхода: {:?}", host().state());
    println!("Движок: {:?}", host().engine_alive());

    let scan = host().scan();
    let targets: Vec<Probe> = scan
        .core_dir
        .as_ref()
        .map(|c| c.join("utils").join("targets.txt"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| text.lines().filter_map(Probe::parse_line).collect())
        .filter(|v: &Vec<Probe>| !v.is_empty())
        .unwrap_or_else(|| {
            ["https://www.youtube.com", "https://discord.com"]
                .iter()
                .filter_map(|s| Probe::parse_value(s))
                .collect()
        });

    println!("целей: {}", targets.len());
    let agent = host().agent();
    let mut ok = 0;
    for p in &targets {
        let res = p.check(&agent);
        if res {
            ok += 1;
        }
        println!("  {} — {}", p.label(), if res { "ok" } else { "FAIL" });
    }
    println!("итог: {ok}/{} доступно", targets.len());
}

/// Диагностика: спавн движка со стратегией, ожидание ~4с, печать его вывода.
fn test_run(name: Option<&str>) {
    let scan = host().scan();
    let target = name.unwrap_or("general");
    let Some(strat) = scan.strategies.iter().find(|s| s.name == target) else {
        eprintln!("стратегия не найдена: {target}");
        return;
    };
    let Some(cmd_spec) = host().engine_command(strat, false) else {
        eprintln!("команда движка недоступна (ядро не найдено или не реализовано)");
        return;
    };

    let log_path = std::env::temp_dir().join("zaprust_engine_test.log");
    let file = std::fs::File::create(&log_path).expect("create log");
    let err = file.try_clone().map(Stdio::from).unwrap_or_else(|_| Stdio::null());

    let mut cmd = Command::new(&cmd_spec.program);
    cmd.args(&cmd_spec.args)
        .current_dir(&cmd_spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(err);

    println!("spawn: {} ({} args)", cmd_spec.program.display(), cmd_spec.args.len());
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::sleep(Duration::from_secs(4));
            match child.try_wait() {
                Ok(Some(status)) => println!("движок завершился сам, код {:?}", status.code()),
                Ok(None) => {
                    println!("движок ещё жив через 4с — глушу");
                    let _ = child.kill();
                    let _ = child.wait();
                }
                Err(e) => println!("try_wait error: {e}"),
            }
        }
        Err(e) => println!("spawn error: {e}"),
    }

    println!("---- вывод движка ({}) ----", log_path.display());
    match std::fs::read(&log_path) {
        Ok(bytes) => println!("{}", String::from_utf8_lossy(&bytes)),
        Err(e) => println!("не прочитать лог: {e}"),
    }
}

/// L2 (Linux): ручной запуск движка из-под root. Поднимает правила перехвата +
/// nfqws с выбранной (по умолчанию — единственной хардкод) стратегией, держит до
/// Ctrl-C/Enter, затем всё снимает. Это и есть проверка критерия L2.
fn engine_up_command(name: Option<&str>) -> i32 {
    if !host().is_elevated() {
        eprintln!("Ошибка: --engine-up требует root. Запустите через sudo.");
        return 1;
    }
    if !host().engine_installed() {
        eprintln!("Движок не найден: {}", host().engine_diag());
        eprintln!("Установите bol-van/zapret (бинарь nfqws) — автоустановку даст L8.");
        return 1;
    }

    let scan = host().scan();
    for m in &scan.messages {
        eprintln!("# {m}");
    }
    let strat = match name {
        Some(n) => scan.strategies.iter().find(|s| s.name == n),
        None => scan.strategies.first(),
    };
    let Some(strat) = strat else {
        match name {
            Some(n) => eprintln!("Стратегия не найдена: {n}"),
            None => eprintln!("Стратегий нет"),
        }
        return 1;
    };

    println!("Движок: {}", host().engine_diag());
    match host().run_foreground(strat, false) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Ошибка: {e}");
            1
        }
    }
}

/// `--engine-down`: аварийно снять правила перехвата и заглушить nfqws.
fn engine_down_command() -> i32 {
    if !host().is_elevated() {
        eprintln!("Ошибка: --engine-down требует root. Запустите через sudo.");
        return 1;
    }
    host().reset_engine();
    println!("Готово: nfqws заглушён, правила перехвата сняты (best-effort).");
    0
}

/// L5: тело systemd-службы (`ExecStart=… --svc-run <стратегия> <gf>`). Запускается
/// самим systemd от root: поднимает правила перехвата + nfqws и держит на переднем
/// плане до SIGTERM (Стоп службы), затем снимает всё. Вывод движка идёт в journald.
fn svc_run_command(name: Option<&str>, gf: Option<&str>) -> i32 {
    if !host().is_elevated() {
        eprintln!("Ошибка: --svc-run запускается systemd от root.");
        return 1;
    }
    let game_filter = gf == Some("1");
    let scan = host().scan();
    for m in &scan.messages {
        logging::info("service", m);
    }
    let strat = match name {
        Some(n) => scan.strategies.iter().find(|s| s.name == n),
        None => scan.strategies.first(),
    };
    let Some(strat) = strat else {
        let msg = match name {
            Some(n) => format!("стратегия не найдена: {n}"),
            None => "стратегий нет".to_owned(),
        };
        logging::error("service", &msg);
        eprintln!("Ошибка: {msg}");
        return 1;
    };
    logging::info("service", format!("движок: {}", host().engine_diag()));
    match host().run_service(strat, game_filter) {
        Ok(()) => 0,
        Err(e) => {
            logging::error("service", format!("служба: {e}"));
            eprintln!("Ошибка: {e}");
            1
        }
    }
}

/// L4-диагностика: проверка критерия элевации. Со стороны НЕэлевированного GUI
/// зовём `run_elevated_self` (→ pkexec покажет один polkit-диалог), реинвок
/// исполняется от root, пишет result-файл в /tmp, а мы читаем его обратно.
fn test_elevate(args: &[String]) -> i32 {
    if host().is_elevated() {
        eprintln!("Запускайте БЕЗ sudo/root — смысл теста в том, чтобы pkexec поднял права сам.");
        return 2;
    }
    // По умолчанию реинвок делает `--svc start` (на Linux пока заглушка systemd).
    let argv: Vec<&str> = if args.is_empty() {
        vec!["--svc", "start"]
    } else {
        args.iter().map(|s| s.as_str()).collect()
    };

    println!("вызываю элевацию: {} (сейчас должен появиться polkit-диалог)…", argv.join(" "));
    let _ = std::fs::remove_file(op_result_path());

    match host().run_elevated_self(&argv) {
        Ok(code) => {
            println!("реинвок завершён, код выхода = {code}");
            match read_op_result() {
                Some(r) => {
                    println!(
                        "result-файл получен: op={} ok={} service_state={} winws_pid={:?} error={:?}",
                        r.op, r.ok, r.service_state, r.winws_pid, r.error
                    );
                    println!("✔ КРИТЕРИЙ L4: один диалог → реинвок от root → результат дошёл до GUI через файл.");
                    0
                }
                None => {
                    println!("⚠ код выхода есть, но result-файла нет (реинвок не дописал итог).");
                    1
                }
            }
        }
        Err(e) => {
            // Отмена диалога (rc=126), pkexec недоступен, смерть по сигналу — всё сюда.
            println!("элевация не выполнена: {e}");
            println!("(отмена диалога = rc=126; это корректный, различимый исход — UI не виснет.)");
            1
        }
    }
}

/// Состояние UI. Стратегии теперь реальные (из папки core/), остальное —
/// пока локальные тумблеры и фейковый статус (живая логика — следующие шаги).
struct ZaprustApp {
    icons: Icons,
    core: strategies::CoreScan,
    selected_strategy: usize,
    game_filter: bool,
    ipset: bool,
    log_lines: Vec<String>,
    /// Канал для строк вывода winws из фоновых потоков-читателей.
    /// UI-поток только читает из rx; потоки-читатели только пишут в tx.
    log_tx: Sender<String>,
    log_rx: Receiver<String>,
    /// Состояние службы zapret (источник статуса «работает/выключен»).
    service: ServiceState,
    service_tx: Sender<ServiceState>,
    service_rx: Receiver<ServiceState>,
    /// Общий таймер периодических проверок состояния.
    last_status_check: Instant,
    /// Идёт ли сейчас тест доступности доменов.
    test_running: Arc<AtomicBool>,
    /// Идёт ли элевированная операция со службой (блокируем кнопки).
    service_busy: Arc<AtomicBool>,
    /// Последняя операция отчиталась ok, но обход по факту не поднялся (рассинхрон).
    op_failed: Arc<AtomicBool>,
    /// Живой рассинхрон: служба «active», но tri-check движка не подтверждает обход
    /// (nfqws мёртв / правил нет). Ставится периодической проверкой — чтобы статус
    /// не врал «Работает» при тихо умершем движке.
    status_desync: Arc<AtomicBool>,
    /// Идёт ли проверка/установка/скачивание ядра.
    update_busy: Arc<AtomicBool>,
    /// Фоновый поток просит переcканировать ядро (после загрузки/обновления).
    reload_requested: Arc<AtomicBool>,
    /// Простой режим (статус + одна кнопка) vs расширенный.
    simple_mode: bool,
    /// Выбран ли в дропдауне виртуальный пункт «smart» (автоподбор).
    smart_selected: bool,
    /// Last-known-good: имя последнего подобранного победителя.
    auto_best: Option<String>,
    /// Идёт ли автоподбор стратегии.
    autoselect_running: Arc<AtomicBool>,
    /// Автоподбор успешно завершился — применить победителя в состояние.
    autoselect_applied: Arc<AtomicBool>,
    /// Последний прочитанный прогресс автоподбора (для UI).
    autoselect_progress: Option<AutoProgress>,
    /// Итог прошлого подбора «рабочая не найдена».
    autoselect_no_result: bool,
    /// Открыт ли экран редактора листов (по ⚙).
    lists_open: bool,
    /// Имена файлов в core/lists.
    lists_files: Vec<String>,
    /// Индекс выбранного списка.
    lists_sel: usize,
    /// Редактируемый текст выбранного списка.
    lists_text: String,
    /// Короткий статус (сохранено/откатано/ошибка).
    lists_status: String,
    /// Идёт ли запись/откат (блокируем кнопки).
    lists_busy: Arc<AtomicBool>,
    /// Канал событий редактора листов из фоновых потоков.
    lists_tx: Sender<ListsEvent>,
    lists_rx: Receiver<ListsEvent>,
}

/// Событие редактора листов из фонового потока в UI.
enum ListsEvent {
    SetText(String),
    Status(String),
}

/// Максимум строк в буфере лога (чтобы не рос бесконечно).
const LOG_CAP: usize = 500;

impl ZaprustApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        // Чуть крупнее и причёсаннее: единый масштаб (текст+иконки+отступы).
        cc.egui_ctx.set_zoom_factor(1.15);
        let icons = Icons::load(&cc.egui_ctx);

        // Сканируем ядро при старте (источник стратегий платформы).
        let core = host().scan();
        let mut log_lines: Vec<String> = core.messages.iter().cloned().collect();
        log_lines.push("zaprust: интерфейс запущен".to_owned());

        // Загружаем сохранённые настройки.
        let cfg = config::Config::load();

        // Синхронно узнаём состояние службы и её стратегию (быстро, один раз).
        let service0 = host().state();

        // Режим smart выбран, если в конфиге спец-значение.
        let smart_selected = cfg.strategy.as_deref() == Some(config::SMART);

        // Last-known-good победитель: из конфига, иначе — из метки активной службы.
        let auto_best = cfg.auto_best.clone().or_else(|| {
            if service0.installed() {
                host().installed_strategy()
            } else {
                None
            }
        });

        // Восстанавливаем выбранную РЕАЛЬНУЮ стратегию по имени (если не smart):
        // приоритет — реально запущенная служба, затем конфиг.
        let pick_name = if service0.installed() {
            host().installed_strategy()
        } else {
            cfg.strategy.clone().filter(|s| s != config::SMART)
        };
        let mut selected_strategy = 0;
        if let Some(name) = &pick_name {
            if let Some(idx) = core.strategies.iter().position(|s| &s.name == name) {
                selected_strategy = idx;
            }
        }
        if service0.installed() {
            if let Some(name) = &pick_name {
                log_lines.push(format!("служба уже активна · стратегия: {name}"));
            }
        }

        let (log_tx, log_rx) = std::sync::mpsc::channel();
        let (service_tx, service_rx) = std::sync::mpsc::channel();
        let (lists_tx, lists_rx) = std::sync::mpsc::channel();

        // Поддерживаем состояние службы свежим (вместе с tri-check рассинхрона).
        let status_desync = Arc::new(AtomicBool::new(false));
        check_service(service_tx.clone(), status_desync.clone());

        Self {
            icons,
            core,
            selected_strategy,
            game_filter: cfg.game_filter,
            ipset: cfg.ipset,
            log_lines,
            log_tx,
            log_rx,
            service: service0,
            service_tx,
            service_rx,
            last_status_check: Instant::now(),
            test_running: Arc::new(AtomicBool::new(false)),
            service_busy: Arc::new(AtomicBool::new(false)),
            op_failed: Arc::new(AtomicBool::new(false)),
            status_desync,
            update_busy: Arc::new(AtomicBool::new(false)),
            reload_requested: Arc::new(AtomicBool::new(false)),
            simple_mode: cfg.simple_mode,
            smart_selected,
            auto_best,
            autoselect_running: Arc::new(AtomicBool::new(false)),
            autoselect_applied: Arc::new(AtomicBool::new(false)),
            autoselect_progress: None,
            autoselect_no_result: false,
            lists_open: false,
            lists_files: Vec::new(),
            lists_sel: 0,
            lists_text: String::new(),
            lists_status: String::new(),
            lists_busy: Arc::new(AtomicBool::new(false)),
            lists_tx,
            lists_rx,
        }
    }

    /// Собрать и сохранить текущие настройки.
    fn save_config(&self) {
        let strategy = if self.smart_selected {
            Some(config::SMART.to_owned())
        } else {
            self.current_strategy().map(|s| s.name.clone())
        };
        let cfg = config::Config {
            strategy,
            auto_best: self.auto_best.clone(),
            game_filter: self.game_filter,
            ipset: self.ipset,
            simple_mode: self.simple_mode,
        };
        cfg.save();
    }


    /// Готово ли ядро к работе: версия + установленный движок + распарсенные стратегии.
    fn core_ready(&self) -> bool {
        let Some(dir) = &self.core.core_dir else {
            return false;
        };
        !self.core.strategies.is_empty()
            && updater::local_version(dir).is_some()
            && host().engine_installed()
    }

    /// Пересканировать ядро (после загрузки/обновления/подбора).
    fn reload_core(&mut self) {
        self.core = host().scan();
        if self.selected_strategy >= self.core.strategies.len() {
            self.selected_strategy = 0;
        }
        // Если служба установлена — подставить её стратегию (победителя подбора).
        if let Some(name) = host().installed_strategy() {
            if let Some(idx) = self.core.strategies.iter().position(|s| s.name == name) {
                self.selected_strategy = idx;
                self.save_config();
            }
        }
        for m in self.core.messages.clone() {
            self.log(m);
        }
    }

    /// Скачать и установить ядро Flowseal целиком (первая установка).
    fn download_core(&mut self) {
        if self.update_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let target = self
            .core
            .core_dir
            .clone()
            .unwrap_or_else(|| host().preferred_core_dir());
        let log_tx = self.log_tx.clone();
        let busy = self.update_busy.clone();
        let reload = self.reload_requested.clone();

        std::thread::spawn(move || {
            match download_core_impl(&target, &log_tx) {
                Ok(msg) => {
                    let _ = log_tx.send(format!("ядро: {msg}"));
                    reload.store(true, Ordering::SeqCst);
                }
                Err(e) => {
                    let _ = log_tx.send(format!("ядро: {e}"));
                }
            }
            busy.store(false, Ordering::SeqCst);
        });
    }

    /// Проверить обновления ядра и при наличии — скачать и установить (в фоне).
    fn check_and_update(&mut self) {
        if self.update_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let core_dir = self.core.core_dir.clone();
        let strategy = self.current_strategy().map(|s| s.name.clone());
        let gf = self.game_filter;
        let log_tx = self.log_tx.clone();
        let svc_tx = self.service_tx.clone();
        let busy = self.update_busy.clone();

        std::thread::spawn(move || {
            match run_update(core_dir, strategy, gf, &log_tx) {
                Ok(msg) => {
                    let _ = log_tx.send(format!("обновление: {msg}"));
                }
                Err(e) => {
                    let _ = log_tx.send(format!("обновление: {e}"));
                }
            }
            let _ = svc_tx.send(host().state());
            busy.store(false, Ordering::SeqCst);
        });
    }

    /// Запустить тест доступности контрольных доменов (в фоне).
    fn start_test(&mut self) {
        if self.test_running.swap(true, Ordering::SeqCst) {
            return; // уже идёт
        }
        let targets = self.load_targets();
        let tx = self.log_tx.clone();
        let flag = self.test_running.clone();
        self.log(format!("тест: проверяю {} целей…", targets.len()));

        std::thread::spawn(move || {
            let agent = host().agent();
            let mut ok_count = 0usize;
            for probe in &targets {
                let ok = probe.check(&agent);
                if ok {
                    ok_count += 1;
                }
                let _ = tx.send(format!(
                    "тест: {} — {}",
                    probe.label(),
                    if ok { "ok" } else { "fail" }
                ));
            }
            let _ = tx.send(format!("тест завершён: {ok_count}/{} доступно", targets.len()));
            flag.store(false, Ordering::SeqCst);
        });
    }

    /// Контрольные домены: из core/utils/targets.txt либо дефолт.
    fn load_targets(&self) -> Vec<Probe> {
        if let Some(core) = &self.core.core_dir {
            let path = core.join("utils").join("targets.txt");
            if let Ok(text) = std::fs::read_to_string(&path) {
                let probes: Vec<Probe> = text.lines().filter_map(Probe::parse_line).collect();
                if !probes.is_empty() {
                    return probes;
                }
            }
        }
        ["https://www.youtube.com", "https://discord.com", "https://www.google.com"]
            .iter()
            .filter_map(|s| Probe::parse_value(s))
            .collect()
    }

    /// Перелить строки из канала в буфер лога и ограничить его размер.
    fn drain_log(&mut self) {
        while let Ok(line) = self.log_rx.try_recv() {
            self.log_lines.push(line);
        }
        if self.log_lines.len() > LOG_CAP {
            let overflow = self.log_lines.len() - LOG_CAP;
            self.log_lines.drain(0..overflow);
        }
    }

    /// Текущая выбранная стратегия (если есть).
    fn current_strategy(&self) -> Option<&strategies::Strategy> {
        self.core.strategies.get(self.selected_strategy)
    }

    /// Запущен ли обход. Служба «active» И tri-check без рассинхрона: при тихо
    /// умершем движке (служба active, nfqws мёртв) НЕ показываем «Работает».
    fn is_running(&self) -> bool {
        self.service == ServiceState::Running && !self.status_desync.load(Ordering::Relaxed)
    }

    fn log(&mut self, msg: impl Into<String>) {
        self.log_lines.push(msg.into());
    }

    // ── Управление обходом через службу ──────────────────────────────────────

    /// Старт: установить службу (start=auto) с выбранной стратегией и запустить.
    /// Элевируется отдельной операцией (UAC).
    fn start_bypass(&mut self) {
        let Some(strategy) = self.current_strategy().cloned() else {
            self.log("ошибка: стратегия не выбрана");
            return;
        };
        let gf = if self.game_filter { "1" } else { "0" };
        self.run_service_elevated(
            vec![
                "--svc".into(),
                "install".into(),
                strategy.name.clone(),
                gf.into(),
            ],
            format!("запуск обхода · {}", strategy.name),
        );
    }

    /// Стоп: остановить и удалить службу. Элевируется (UAC).
    fn stop_bypass(&mut self) {
        self.run_service_elevated(
            vec!["--svc".into(), "remove".into()],
            "остановка обхода".to_owned(),
        );
    }

    /// Запустить автоподбор (один UAC на весь свип).
    /// `use_lkg=false` — игнорировать прошлого победителя, полный прогон.
    /// `game_filter` — резолв GF для проб и установки (простой режим = off,
    /// smart = по тумблеру).
    fn start_autoselect(&mut self, use_lkg: bool, game_filter: bool) {
        if self.autoselect_running.swap(true, Ordering::SeqCst) {
            return;
        }
        if !self.core_ready() {
            self.autoselect_running.store(false, Ordering::SeqCst);
            return;
        }
        self.autoselect_no_result = false;
        self.autoselect_progress = None;
        // Чистим возможный стоп-флаг от прошлого раза.
        let _ = std::fs::remove_file(autoselect_cancel_path());
        let _ = std::fs::remove_file(autoselect_progress_path());

        // last-known-good: имя победителя из памяти (если есть среди стратегий).
        let lkg = if use_lkg {
            self.auto_best
                .clone()
                .filter(|n| self.core.strategies.iter().any(|s| &s.name == n))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let gf = if game_filter { "1" } else { "0" };

        self.log("автоподбор: запрашиваю права администратора (UAC)…");
        let log_tx = self.log_tx.clone();
        let svc_tx = self.service_tx.clone();
        let applied = self.autoselect_applied.clone();
        let running = self.autoselect_running.clone();

        std::thread::spawn(move || {
            let res = host().run_elevated_self(&["--autoselect", gf, &lkg]);
            match res {
                Ok(0) => {
                    let _ = log_tx.send("автоподбор: победитель установлен".to_owned());
                    applied.store(true, Ordering::SeqCst);
                }
                Ok(2) => {
                    let _ = log_tx.send("автоподбор: отменён".to_owned());
                }
                Ok(3) => {
                    let _ = log_tx.send("автоподбор: рабочая стратегия не найдена".to_owned());
                }
                Ok(code) => {
                    let _ = log_tx.send(format!("автоподбор: ошибка (код {code})"));
                    for line in read_svc_log() {
                        let _ = log_tx.send(format!("автоподбор: {line}"));
                    }
                }
                Err(e) => {
                    let _ = log_tx.send(format!("автоподбор: {e}"));
                }
            }
            let _ = svc_tx.send(host().state());
            running.store(false, Ordering::SeqCst);
        });
    }

    /// Отмена автоподбора — кооперативно через флаг-файл.
    fn cancel_autoselect(&mut self) {
        let _ = std::fs::write(autoselect_cancel_path(), b"1");
        self.log("автоподбор: отмена…");
    }

    /// Сбросить прошлого победителя из памяти и прогнать полный свип заново.
    fn reset_and_rescan(&mut self, game_filter: bool) {
        self.auto_best = None; // забываем last-known-good
        self.save_config();
        self.log("автоподбор: сброшена прошлая стратегия, полный прогон");
        self.start_autoselect(false, game_filter);
    }

    /// Прочитать последнюю строку прогресса автоподбора из temp-файла.
    fn poll_autoselect_progress(&mut self) {
        if let Ok(text) = std::fs::read_to_string(autoselect_progress_path()) {
            if let Some(last) = text.lines().rev().find(|l| !l.trim().is_empty()) {
                if let Ok(p) = serde_json::from_str::<AutoProgress>(last) {
                    if p.stage == "none" {
                        self.autoselect_no_result = true;
                    }
                    self.autoselect_progress = Some(p);
                }
            }
        }
    }

    // ── Редактор листов (по ⚙) ───────────────────────────────────────────────

    fn lists_dir(&self) -> Option<std::path::PathBuf> {
        self.core.core_dir.as_ref().map(|c| c.join("lists"))
    }

    /// Открыть экран листов: перечитать файлы lists/ и загрузить текущий.
    fn open_lists(&mut self) {
        self.lists_open = true;
        self.lists_status.clear();
        self.lists_files.clear();
        if let Some(dir) = self.lists_dir() {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                let mut files: Vec<String> = rd
                    .flatten()
                    .filter(|e| e.path().is_file())
                    .filter_map(|e| e.file_name().to_str().map(|s| s.to_owned()))
                    .filter(|n| n.to_lowercase().ends_with(".txt"))
                    .collect();
                files.sort();
                self.lists_files = files;
            }
        }
        if self.lists_sel >= self.lists_files.len() {
            self.lists_sel = 0;
        }
        self.load_selected_list();
    }

    /// Загрузить текст выбранного списка с диска.
    fn load_selected_list(&mut self) {
        self.lists_text.clear();
        if let (Some(dir), Some(name)) = (self.lists_dir(), self.lists_files.get(self.lists_sel)) {
            match std::fs::read(dir.join(name)) {
                Ok(bytes) => self.lists_text = String::from_utf8_lossy(&bytes).into_owned(),
                Err(e) => self.lists_status = format!("ошибка чтения: {e}"),
            }
        }
    }

    /// Сохранить текущий текст в выбранный файл (фолбэк на элевацию).
    fn save_list(&mut self) {
        if self.lists_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (Some(dir), Some(name)) =
            (self.lists_dir(), self.lists_files.get(self.lists_sel).cloned())
        else {
            self.lists_busy.store(false, Ordering::SeqCst);
            return;
        };
        let path = dir.join(&name);
        let content = self.lists_text.clone().into_bytes();
        let tx = self.lists_tx.clone();
        let busy = self.lists_busy.clone();
        self.lists_status = "сохраняю…".to_owned();
        std::thread::spawn(move || {
            let msg = match write_list_with_fallback(&path, &content) {
                Ok(()) => format!("сохранено: {name}"),
                Err(e) => format!("ошибка: {e}"),
            };
            let _ = tx.send(ListsEvent::Status(msg));
            busy.store(false, Ordering::SeqCst);
        });
    }

    /// Откатить выбранный список к стандартному из свежего релиза Flowseal.
    fn revert_list(&mut self) {
        if self.lists_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (Some(dir), Some(name)) =
            (self.lists_dir(), self.lists_files.get(self.lists_sel).cloned())
        else {
            self.lists_busy.store(false, Ordering::SeqCst);
            return;
        };
        let path = dir.join(&name);
        let tx = self.lists_tx.clone();
        let busy = self.lists_busy.clone();
        self.lists_status = "откат: скачиваю стандартный…".to_owned();
        std::thread::spawn(move || {
            let agent = host().agent();
            match updater::fetch_default_list(&agent, &name) {
                Ok(bytes) => match write_list_with_fallback(&path, &bytes) {
                    Ok(()) => {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        let _ = tx.send(ListsEvent::SetText(text));
                        let _ = tx.send(ListsEvent::Status(format!("откатан стандартный: {name}")));
                    }
                    Err(e) => {
                        let _ = tx.send(ListsEvent::Status(format!("ошибка записи: {e}")));
                    }
                },
                Err(e) => {
                    let _ = tx.send(ListsEvent::Status(format!("откат не удался: {e}")));
                }
            }
            busy.store(false, Ordering::SeqCst);
        });
    }

    // ── Диагностика и обслуживание (D/E) ──────────────────────────────────────

    fn open_logs_folder(&mut self) {
        match logging::log_dir() {
            Some(dir) => {
                host().open_path(&dir);
                self.log(format!("логи: {}", dir.display()));
            }
            None => self.log("папка логов не определена"),
        }
    }

    fn copy_diagnostics(&mut self) {
        if host().set_clipboard(&diagnostics_text()) {
            self.log("диагностика скопирована в буфер обмена");
        } else {
            self.log("не удалось скопировать диагностику");
        }
    }

    /// Полное удаление: стоп + sc delete + выгрузка драйвера (для чистого удаления папки).
    fn uninstall_all(&mut self) {
        self.run_service_elevated(
            vec!["--svc".into(), "uninstall".into()],
            "удаление службы и драйвера".to_owned(),
        );
    }

    // ── Ряд диагностики (расширенный режим) ──────────────────────────────────
    fn diag_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let link = |ui: &mut egui::Ui, text: &str, hover: &str| -> bool {
                ui.add(egui::Button::new(RichText::new(text).size(11.0).color(MUTED)).frame(false))
                    .on_hover_text(hover)
                    .clicked()
            };
            if link(ui, "Логи", "Открыть папку логов") {
                self.open_logs_folder();
            }
            ui.label(RichText::new("·").size(11.0).color(MUTED));
            if link(ui, "Диагностика", "Скопировать диагностику в буфер обмена") {
                self.copy_diagnostics();
            }
            ui.label(RichText::new("·").size(11.0).color(MUTED));
            if link(ui, "Удалить службу", "Стоп + удалить службу + выгрузить драйвер (перед удалением папки)") {
                self.uninstall_all();
            }
        });
    }

    /// Запустить элевированную операцию со службой в фоне (ShellExecute runas).
    /// После операции — авторитетный поллинг состояния (служба RUNNING И winws жив).
    fn run_service_elevated(&mut self, args: Vec<String>, what: String) {
        if self.service_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.op_failed.store(false, Ordering::SeqCst);
        let _ = std::fs::remove_file(op_result_path());
        self.log(format!("{what}: запрашиваю права администратора (UAC)…"));
        logging::info("gui", format!("{what}: реинвок {}", args.join(" ")));

        let expect_running = args
            .get(1)
            .map(|a| a == "install" || a == "start")
            .unwrap_or(false);
        let expect_removed = args
            .get(1)
            .map(|a| a == "remove" || a == "stop" || a == "uninstall")
            .unwrap_or(false);

        let log_tx = self.log_tx.clone();
        let svc_tx = self.service_tx.clone();
        let busy = self.service_busy.clone();
        let op_failed = self.op_failed.clone();

        std::thread::spawn(move || {
            let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let code = host().run_elevated_self(&argv);
            match &code {
                Ok(0) => {
                    let _ = log_tx.send(format!("{what}: готово"));
                    if let Some(r) = read_op_result() {
                        logging::info(
                            "gui",
                            format!("прочитан итог: state={} winws_pid={:?}", r.service_state, r.winws_pid),
                        );
                    }
                }
                Ok(c) => {
                    let _ = log_tx.send(format!("{what}: ошибка (код {c})"));
                    for line in read_svc_log() {
                        let _ = log_tx.send(line);
                    }
                }
                Err(e) => {
                    let _ = log_tx.send(format!("{what}: {e}"));
                }
            }

            // Авторитетный поллинг ~5с (учитываем START_PENDING и квирк движка-как-службы).
            let mut authoritative = false;
            for _ in 0..16 {
                let st = host().state();
                let _ = svc_tx.send(st);
                authoritative = host().authoritative_running();
                if expect_running && authoritative {
                    break;
                }
                if expect_removed
                    && st == ServiceState::NotInstalled
                    && host().engine_alive().is_none()
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
            }

            // Реинвок отчитался ok, но обход не поднялся → явная ошибка состояния.
            if matches!(code, Ok(0)) && expect_running && !authoritative {
                let st = host().state();
                let engine = host().engine_alive();
                let msg = format!(
                    "обход НЕ поднялся: служба={st:?}, движок_pid={engine:?}. Подробности в логах."
                );
                logging::error("gui", &msg);
                let _ = log_tx.send(format!("ОШИБКА: {msg}"));
                op_failed.store(true, Ordering::SeqCst);
            }

            let _ = svc_tx.send(host().state());
            busy.store(false, Ordering::SeqCst);
        });
    }
}

impl eframe::App for ZaprustApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Сообщения службы/теста из каналов в буфер лога.
        self.drain_log();
        // События редактора листов (откат вернул текст, статусы).
        while let Ok(ev) = self.lists_rx.try_recv() {
            match ev {
                ListsEvent::SetText(t) => self.lists_text = t,
                ListsEvent::Status(s) => self.lists_status = s,
            }
        }
        // Если фоновая загрузка ядра завершилась — пересканировать.
        if self.reload_requested.swap(false, Ordering::SeqCst) {
            self.reload_core();
        }
        // Применяем результаты проверок службы; периодически запускаем новую.
        while let Ok(state) = self.service_rx.try_recv() {
            self.service = state;
        }
        if self.last_status_check.elapsed() >= Duration::from_secs(3) {
            self.last_status_check = Instant::now();
            check_service(self.service_tx.clone(), self.status_desync.clone());
        }

        // Автоподбор успешно завершился — запомнить победителя в auto_best.
        if self.autoselect_applied.swap(false, Ordering::SeqCst) {
            if let Some(name) = host().installed_strategy() {
                self.auto_best = Some(name);
                self.save_config();
            }
        }

        // Во время автоподбора/операций с листами/службой поллим и рисуем часто.
        if self.autoselect_running.load(Ordering::Relaxed) {
            self.poll_autoselect_progress();
            ctx.request_repaint_after(Duration::from_millis(200));
        } else if self.lists_busy.load(Ordering::Relaxed)
            || self.service_busy.load(Ordering::Relaxed)
        {
            ctx.request_repaint_after(Duration::from_millis(200));
        } else {
            // Периодическая перерисовка — чтобы статусы/сообщения обновлялись в простое.
            ctx.request_repaint_after(Duration::from_secs(1));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&ctx.style()).fill(PANEL_BG).inner_margin(14.0))
            .show(ctx, |ui| {
                if !self.simple_mode && self.lists_open {
                    // Экран редактора листов (поверх расширенного режима).
                    self.render_lists(ui);
                    return;
                }
                // Переключатель режима резервируем у нижней кромки.
                egui::TopBottomPanel::bottom("mode_switch_panel")
                    .frame(egui::Frame::none())
                    .show_inside(ui, |ui| {
                        ui.add_space(4.0);
                        self.mode_switch(ui);
                        ui.add_space(2.0);
                    });

                self.title_bar(ui);
                if self.simple_mode {
                    self.render_simple(ui);
                } else {
                    ui.add_space(18.0);
                    self.strategy_row(ui);
                    ui.add_space(16.0);
                    self.action_buttons(ui);
                    ui.add_space(14.0);
                    self.toggles(ui);
                    ui.add_space(10.0);
                    self.diag_row(ui);
                    ui.add_space(12.0);
                    self.log_box(ui);
                }
            });
    }
}

impl ZaprustApp {
    // ── Верхняя строка: бренд + единственный статус ──────────────────────────
    fn title_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(egui::Image::new(sized(&self.icons.app, 22.0)));
            ui.add_space(2.0);
            ui.label(RichText::new("Zaprust").size(20.0).strong().color(Color32::WHITE));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.op_failed.load(Ordering::Relaxed)
                    || self.status_desync.load(Ordering::Relaxed)
                {
                    // Рассинхрон tri-check (служба active, но движок мёртв) — честная
                    // «Ошибка», а не тихий «Выключен»/«Работает».
                    pill(ui, "Ошибка", DANGER);
                } else if self.is_running() {
                    pill(ui, "Работает", OK);
                } else {
                    pill(ui, "Выключен", MUTED);
                }
            });
        });
    }

    // ── Простой режим: статус + одна кнопка ──────────────────────────────────
    fn render_simple(&mut self, ui: &mut egui::Ui) {
        let running = self.is_running();
        let busy = self.autoselect_running.load(Ordering::Relaxed);
        let updating = self.update_busy.load(Ordering::Relaxed);
        let ready = self.core_ready();

        ui.add_space(48.0);
        ui.vertical_centered(|ui| {
            if busy {
                let (idx, total, strat) = self
                    .autoselect_progress
                    .as_ref()
                    .map(|p| (p.idx, p.total, p.strategy.clone()))
                    .unwrap_or((0, 0, String::new()));
                ui.label(RichText::new("Подбираю стратегию…").size(20.0).strong().color(Color32::WHITE));
                ui.add_space(6.0);
                let line = if total > 0 {
                    format!("Проверяю {idx}/{total}{}", if strat.is_empty() { String::new() } else { format!(" · {strat}") })
                } else {
                    "запуск…".to_owned()
                };
                ui.label(RichText::new(line).size(13.0).color(MUTED));
            } else if running {
                let name = self.auto_best.clone().unwrap_or_default();
                ui.label(RichText::new("Обход работает").size(20.0).strong().color(OK));
                ui.add_space(6.0);
                if !name.is_empty() {
                    ui.label(RichText::new(name).size(13.0).color(MUTED));
                }
            } else if !ready {
                ui.label(RichText::new("Ядро не установлено").size(18.0).strong().color(Color32::WHITE));
                ui.add_space(6.0);
                ui.label(RichText::new("Скачайте актуальную сборку Flowseal").size(12.0).color(MUTED));
            } else {
                ui.label(RichText::new("Обход выключен").size(20.0).strong().color(Color32::WHITE));
                ui.add_space(6.0);
                if self.autoselect_no_result {
                    ui.label(
                        RichText::new("Рабочая стратегия не найдена — попробуйте ещё раз или расширенный режим")
                            .size(12.0)
                            .color(Color32::from_rgb(220, 170, 90)),
                    );
                } else {
                    ui.label(RichText::new("Нажмите «Старт» — подберём рабочую автоматически").size(12.0).color(MUTED));
                }
            }
        });

        ui.add_space(26.0);

        let w = ui.available_width();
        if !ready {
            let label = if updating { "Скачивание…" } else { "Скачать ядро" };
            if ui
                .add_enabled(
                    !updating,
                    egui::Button::image_and_text(
                        sized(&self.icons.download, 18.0),
                        RichText::new(label).size(16.0).strong().color(Color32::WHITE),
                    )
                    .fill(ACCENT)
                    .min_size(Vec2::new(w, 48.0)),
                )
                .clicked()
            {
                self.download_core();
            }
        } else if busy {
            if ui
                .add(
                    egui::Button::image_and_text(
                        sized(&self.icons.cancel, 18.0),
                        RichText::new("Отмена").size(16.0).strong().color(Color32::WHITE),
                    )
                    .fill(DANGER)
                    .min_size(Vec2::new(w, 48.0)),
                )
                .clicked()
            {
                self.cancel_autoselect();
            }
        } else if running {
            if ui
                .add(
                    egui::Button::image_and_text(
                        sized(&self.icons.stop, 18.0),
                        RichText::new("Стоп").size(16.0).strong().color(Color32::WHITE),
                    )
                    .fill(DANGER)
                    .min_size(Vec2::new(w, 48.0)),
                )
                .clicked()
            {
                self.stop_bypass();
            }
        } else {
            let retry = self.autoselect_no_result;
            let label = if retry { "Попробовать снова" } else { "Старт" };
            let icon = if retry { &self.icons.refresh } else { &self.icons.play };
            if ui
                .add(
                    egui::Button::image_and_text(
                        sized(icon, 18.0),
                        RichText::new(label).size(16.0).strong().color(Color32::WHITE),
                    )
                    .fill(ACCENT)
                    .min_size(Vec2::new(w, 48.0)),
                )
                .clicked()
            {
                self.start_autoselect(true, false);
            }
        }

        // Вторичная кнопка: сброс прошлой стратегии + полный прогон.
        // Только когда служба выключена, ядро готово, есть что сбрасывать.
        if ready && !running && !busy && !updating && self.auto_best.is_some() {
            ui.add_space(8.0);
            if ui
                .add(
                    egui::Button::new(
                        RichText::new("Сбросить и пересканировать").size(13.0).color(MUTED),
                    )
                    .fill(FIELD_BG)
                    .min_size(Vec2::new(w, 36.0)),
                )
                .on_hover_text("Забыть прошлого победителя и прогнать все стратегии заново")
                .clicked()
            {
                self.reset_and_rescan(false);
            }
        }
    }

    // ── Переключатель простой/расширенный режим (внизу) ──────────────────────
    fn mode_switch(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            let (icon, label) = if self.simple_mode {
                (&self.icons.chevron_down, "Расширенный режим")
            } else {
                (&self.icons.chevron_up, "Простой режим")
            };
            let resp = ui.add(
                egui::Button::image_and_text(
                    sized(icon, 12.0),
                    RichText::new(label).size(12.0).color(MUTED),
                )
                .frame(false),
            );
            // Переключение режима недоступно во время подбора.
            if resp.clicked() && !self.autoselect_running.load(Ordering::Relaxed) {
                self.simple_mode = !self.simple_mode;
                self.save_config();
            }
        });
    }

    // ── Экран редактора списков (по ⚙) ───────────────────────────────────────
    fn render_lists(&mut self, ui: &mut egui::Ui) {
        let busy = self.lists_busy.load(Ordering::Relaxed);

        // Шапка: Назад + заголовок.
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::image_and_text(
                        sized(&self.icons.back, 14.0),
                        RichText::new("Назад").size(13.0).color(MUTED),
                    )
                    .frame(false),
                )
                .clicked()
            {
                self.lists_open = false;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new("Списки").size(16.0).strong().color(Color32::WHITE));
            });
        });
        ui.add_space(10.0);

        if self.lists_files.is_empty() {
            ui.label(RichText::new("В core/lists нет файлов").size(13.0).color(MUTED));
            return;
        }

        // Нижняя панель (статус + кнопки) — резервируется у нижней кромки окна.
        egui::TopBottomPanel::bottom("lists_actions")
            .frame(egui::Frame::none())
            .show_inside(ui, |ui| {
                ui.add_space(8.0);
                // Статус — всегда строка (резерв, чтобы вёрстка не прыгала).
                let status = if self.lists_status.is_empty() {
                    " ".to_owned()
                } else {
                    self.lists_status.clone()
                };
                ui.label(RichText::new(status).size(11.0).color(MUTED));
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    let gap = ui.spacing().item_spacing.x;
                    let bw = (ui.available_width() - gap) / 2.0;
                    let save_label = if busy { "…" } else { "Сохранить" };
                    if ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(
                                RichText::new(save_label).size(14.0).strong().color(Color32::WHITE),
                            )
                            .fill(ACCENT)
                            .min_size(Vec2::new(bw, 40.0)),
                        )
                        .clicked()
                    {
                        self.save_list();
                    }
                    if ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(RichText::new("Откатить стандартный").size(13.0))
                                .fill(FIELD_BG)
                                .min_size(Vec2::new(bw, 40.0)),
                        )
                        .on_hover_text("Вернуть выбранный список к версии из свежего релиза Flowseal")
                        .clicked()
                    {
                        self.revert_list();
                    }
                });
                ui.add_space(2.0);
            });

        // Дропдаун выбора файла.
        ui.label(RichText::new("Файл списка").color(MUTED).size(12.0));
        ui.add_space(4.0);
        let prev = self.lists_sel;
        let current = self.lists_files.get(self.lists_sel).cloned().unwrap_or_default();
        egui::ComboBox::from_id_salt("lists_file")
            .selected_text(RichText::new(current).size(14.0))
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for i in 0..self.lists_files.len() {
                    let name = self.lists_files[i].clone();
                    ui.selectable_value(&mut self.lists_sel, i, name);
                }
            });
        if self.lists_sel != prev {
            self.lists_status.clear();
            self.load_selected_list();
        }

        ui.add_space(10.0);

        // Редактор — занимает всю оставшуюся высоту (между комбо и нижней панелью).
        let editor_h = ui.available_height().max(120.0);
        egui::Frame::none()
            .fill(LOG_BG)
            .rounding(Rounding::same(8.0))
            .inner_margin(Margin::same(8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height((editor_h - 16.0).max(80.0))
                    .show(ui, |ui| {
                        ui.add_enabled(
                            !busy,
                            egui::TextEdit::multiline(&mut self.lists_text)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY)
                                .desired_rows((editor_h / 16.0) as usize)
                                .frame(false),
                        );
                    });
            });
    }

    // ── Дропдаун стратегии (реальные данные из core/) ────────────────────────
    fn strategy_row(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Стратегия").color(MUTED).size(12.0));
        ui.add_space(4.0);

        if self.core.strategies.is_empty() {
            // Понятное сообщение вместо падения.
            egui::Frame::none()
                .fill(FIELD_BG)
                .rounding(Rounding::same(8.0))
                .inner_margin(Margin::symmetric(10.0, 8.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("Ядро не установлено")
                            .size(14.0)
                            .color(Color32::from_rgb(220, 170, 90)),
                    );
                    ui.label(
                        RichText::new("Нажмите «Скачать ядро» — Zaprust сам подтянет актуальную сборку Flowseal")
                            .size(11.0)
                            .color(MUTED),
                    );
                });
            return;
        }

        // Клампим индекс на случай пересканирования.
        if self.selected_strategy >= self.core.strategies.len() {
            self.selected_strategy = 0;
        }

        // Подпись smart: smart() или smart(Победитель).
        let smart_label = format!("smart({})", self.auto_best.clone().unwrap_or_default());
        let selected_text = if self.smart_selected {
            smart_label.clone()
        } else {
            self.core.strategies[self.selected_strategy].name.clone()
        };

        let mut changed = false;
        egui::ComboBox::from_id_salt("strategy")
            .selected_text(RichText::new(selected_text).size(14.0))
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                // Виртуальный пункт «smart» — всегда первым.
                ui.label(RichText::new("АВТО").size(10.0).strong().color(MUTED));
                if ui
                    .selectable_label(
                        self.smart_selected,
                        RichText::new(smart_label.clone()).size(14.0).color(OK),
                    )
                    .clicked()
                    && !self.smart_selected
                {
                    self.smart_selected = true;
                    changed = true;
                }
                ui.separator();

                let mut last_group: Option<String> = None;
                for i in 0..self.core.strategies.len() {
                    let group = self.core.strategies[i].group.clone();
                    if last_group.as_deref() != Some(group.as_str()) {
                        if last_group.is_some() {
                            ui.separator();
                        }
                        ui.label(
                            RichText::new(group.to_uppercase())
                                .size(10.0)
                                .strong()
                                .color(MUTED),
                        );
                        last_group = Some(group);
                    }
                    let label = self.core.strategies[i].name.clone();
                    let selected = !self.smart_selected && self.selected_strategy == i;
                    if ui.selectable_label(selected, label).clicked() {
                        self.selected_strategy = i;
                        self.smart_selected = false;
                        changed = true;
                    }
                }
            });
        if changed {
            self.save_config(); // запоминаем выбор (smart или реальная стратегия)
        }
    }

    // ── Кнопки: Старт/Стоп + Тест + настройки ────────────────────────────────
    fn action_buttons(&mut self, ui: &mut egui::Ui) {
        // Ядра нет — вместо Старта показываем «Скачать ядро».
        if !self.core_ready() {
            let updating = self.update_busy.load(Ordering::Relaxed);
            let label = if updating { "Скачивание…" } else { "Скачать ядро" };
            let btn = ui.add_enabled(
                !updating,
                egui::Button::image_and_text(
                    sized(&self.icons.download, 16.0),
                    RichText::new(label).size(16.0).strong().color(Color32::WHITE),
                )
                .fill(ACCENT)
                .min_size(Vec2::new(ui.available_width(), 46.0)),
            );
            if btn.clicked() {
                self.download_core();
            }
            return;
        }

        // Идёт автоподбор (smart) — показываем прогресс + Отмена на всю ширину.
        if self.autoselect_running.load(Ordering::Relaxed) {
            let (idx, total, strat) = self
                .autoselect_progress
                .as_ref()
                .map(|p| (p.idx, p.total, p.strategy.clone()))
                .unwrap_or((0, 0, String::new()));
            let line = if total > 0 {
                format!("Подбираю {idx}/{total}{}", if strat.is_empty() { String::new() } else { format!(" · {strat}") })
            } else {
                "Подбираю…".to_owned()
            };
            ui.label(RichText::new(line).size(13.0).color(MUTED));
            ui.add_space(4.0);
            if ui
                .add(
                    egui::Button::image_and_text(
                        sized(&self.icons.cancel, 16.0),
                        RichText::new("Отмена").size(16.0).strong().color(Color32::WHITE),
                    )
                    .fill(DANGER)
                    .min_size(Vec2::new(ui.available_width(), 46.0)),
                )
                .clicked()
            {
                self.cancel_autoselect();
            }
            return;
        }

        let gf = self.game_filter;
        let smart = self.smart_selected;

        ui.horizontal(|ui| {
            let running = self.is_running();
            let busy = self.service_busy.load(Ordering::Relaxed);
            let (label, color) = if busy {
                ("…", FIELD_BG)
            } else if running {
                ("Стоп", DANGER)
            } else {
                ("Старт", ACCENT)
            };

            // Старт недоступен, если нет выбранной стратегии или идёт операция.
            let can_act = !busy && (running || smart || self.current_strategy().is_some());

            // Растягиваем Старт на всю ширину, оставив место для Тест/⟳/⚙.
            let h = 46.0;
            let test_w = 72.0;
            let icon_w = 46.0;
            let gap = ui.spacing().item_spacing.x;
            let start_w =
                (ui.available_width() - test_w - icon_w * 2.0 - gap * 3.0).max(96.0);

            let rt = RichText::new(label).size(16.0).strong().color(Color32::WHITE);
            let btn = if busy {
                egui::Button::new(rt)
            } else if running {
                egui::Button::image_and_text(sized(&self.icons.stop, 16.0), rt)
            } else {
                egui::Button::image_and_text(sized(&self.icons.play, 16.0), rt)
            };
            let start = ui.add_enabled(
                can_act,
                btn.fill(color).min_size(Vec2::new(start_w, h)),
            );
            if start.hovered() && can_act {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if start.clicked() {
                if running {
                    self.stop_bypass(); // остановить + удалить службу
                } else if smart {
                    self.start_autoselect(true, gf); // smart: автоподбор с учётом GF
                } else {
                    self.start_bypass(); // ручная стратегия → служба
                }
            }

            let testing = self.test_running.load(Ordering::Relaxed);
            let test_label = if testing { "Тест…" } else { "Тест" };
            if ui
                .add_enabled(
                    !testing,
                    egui::Button::image_and_text(
                        sized(&self.icons.test, 14.0),
                        RichText::new(test_label).size(14.0),
                    )
                    .fill(FIELD_BG)
                    .min_size(Vec2::new(test_w, h)),
                )
                .clicked()
            {
                self.start_test();
            }

            // Проверить/установить обновление ядра.
            let updating = self.update_busy.load(Ordering::Relaxed);
            let upd = ui.add_enabled(
                !updating,
                egui::Button::image(sized(&self.icons.refresh, 18.0))
                    .fill(FIELD_BG)
                    .min_size(Vec2::new(icon_w, h)),
            );
            if upd.on_hover_text("Проверить обновления ядра").clicked() {
                self.check_and_update();
            }

            if ui
                .add(
                    egui::Button::image(sized(&self.icons.settings, 18.0))
                        .fill(FIELD_BG)
                        .min_size(Vec2::new(icon_w, h)),
                )
                .on_hover_text("Редактор списков")
                .clicked()
            {
                self.open_lists();
            }
        });

        // Кнопка сброса для smart: когда есть победитель и служба выключена.
        if smart && self.auto_best.is_some() && !self.is_running()
            && !self.service_busy.load(Ordering::Relaxed)
        {
            ui.add_space(8.0);
            if ui
                .add(
                    egui::Button::new(
                        RichText::new("Сбросить и пересканировать").size(13.0).color(MUTED),
                    )
                    .fill(FIELD_BG)
                    .min_size(Vec2::new(ui.available_width(), 34.0)),
                )
                .on_hover_text("Забыть прошлого победителя и прогнать все стратегии заново")
                .clicked()
            {
                self.reset_and_rescan(gf);
            }
        }
    }

    // ── Ряд тумблеров ────────────────────────────────────────────────────────
    fn toggles(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.horizontal(|ui| {
            changed |= ui.toggle_value(&mut self.game_filter, "Game Filter").changed();
            ui.add_space(6.0);
            changed |= ui.toggle_value(&mut self.ipset, "IPSet").changed();
        });
        if changed {
            self.save_config();
        }
    }

    // ── Лог-бокс ─────────────────────────────────────────────────────────────
    fn log_box(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Лог").color(MUTED).size(12.0));
        ui.add_space(4.0);
        // Лог заполняет всю оставшуюся высоту окна.
        let avail = (ui.available_height() - 16.0).max(120.0);
        egui::Frame::none()
            .fill(LOG_BG)
            .rounding(Rounding::same(6.0))
            .inner_margin(Margin::same(8.0))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(avail)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for line in &self.log_lines {
                            ui.label(
                                RichText::new(line)
                                    .monospace()
                                    .size(12.0)
                                    .color(Color32::from_rgb(168, 176, 184)),
                            );
                        }
                    });
            });
    }
}

// ── Хелперы рисования ────────────────────────────────────────────────────────

/// Статусная «пилюля» — закруглённый цветной бейдж.
fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::none()
        .fill(color.gamma_multiply(0.22))
        .rounding(Rounding::same(10.0))
        .inner_margin(Margin::symmetric(9.0, 3.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                dot(ui, color);
                ui.add_space(5.0);
                ui.label(RichText::new(text).size(12.0).color(color));
            });
        });
}

/// Маленькая цветная точка-индикатор.
fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

// ── Проверки состояния службы обхода ─────────────────────────────────────────

/// Фоновая проверка состояния службы обхода → канал.
/// Фоновая периодическая проверка состояния: состояние юнита + авторитетный
/// tri-check. `status_desync` ставится, если служба «active», а обход по факту не
/// поднят (живой рассинхрон: nfqws мёртв / правил нет) — чтобы UI показал «Ошибка»,
/// а не тихо врал «Работает». Сетевые/процессные вызовы — в отдельном потоке, UI не
/// блокируем.
fn check_service(tx: Sender<ServiceState>, status_desync: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let state = host().state();
        // Рассинхрон только когда служба считается запущенной: «active», но
        // authoritative_running() (служба И движок И правила) не подтверждает.
        let desync = state == ServiceState::Running && !host().authoritative_running();
        status_desync.store(desync, Ordering::SeqCst);
        let _ = tx.send(state);
    });
}

// ── Тест доступности доменов ─────────────────────────────────────────────────

/// Контрольная цель: HTTPS-домен (TLS с SNI — то, что режет DPI) или TCP-проверка IP.
#[derive(Clone)]
enum Probe {
    Https(String), // полный URL
    Tcp(String),   // "host:port"
}

impl Probe {
    /// Распарсить строку файла targets.txt вида `Name = "value"`.
    fn parse_line(line: &str) -> Option<Probe> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (_, rhs) = line.split_once('=')?;
        Probe::parse_value(rhs)
    }

    fn parse_value(value: &str) -> Option<Probe> {
        let v = value.trim().trim_matches('"').trim();
        if let Some(ip) = v.strip_prefix("PING:") {
            let ip = ip.trim();
            (!ip.is_empty()).then(|| Probe::Tcp(format!("{ip}:443")))
        } else if v.starts_with("http://") || v.starts_with("https://") {
            Some(Probe::Https(v.to_string()))
        } else if !v.is_empty() {
            Some(Probe::Https(format!("https://{v}")))
        } else {
            None
        }
    }

    fn label(&self) -> String {
        match self {
            Probe::Https(url) => url
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_end_matches('/')
                .to_string(),
            Probe::Tcp(addr) => addr.trim_end_matches(":443").to_string(),
        }
    }

    /// true = достижимо.
    fn check(&self, agent: &ureq::Agent) -> bool {
        self.check_timed(agent).0
    }

    /// Проверка с замером времени: (достижимо, миллисекунды).
    /// Для HTTPS это время TLS-хендшейка + TTFB (тело не качаем) — именно оно
    /// различает стратегии по обработке ClientHello. Для пинга используем его.
    fn check_timed(&self, agent: &ureq::Agent) -> (bool, u128) {
        let d = self.check_detail(agent);
        (d.0, d.1)
    }

    /// Как check_timed, но с причиной отказа (для диагностики автоподбора):
    /// (достижимо, мс, причина: "ok" / "dns: …" / "io: …" / "tcp: …").
    fn check_detail(&self, agent: &ureq::Agent) -> (bool, u128, String) {
        let start = Instant::now();
        let (ok, why) = match self {
            Probe::Https(url) => match agent.get(url).call() {
                Ok(_) => (true, "ok".to_owned()),
                Err(ureq::Error::Status(code, _)) => (true, format!("http {code}")),
                Err(ureq::Error::Transport(t)) => {
                    (false, format!("{:?}: {t}", t.kind()).replace('\n', " "))
                }
            },
            Probe::Tcp(addr) => match addr.to_socket_addrs() {
                Ok(mut addrs) => match addrs.next() {
                    Some(sa) => match TcpStream::connect_timeout(&sa, Duration::from_secs(3)) {
                        Ok(_) => (true, "ok".to_owned()),
                        Err(e) => (false, format!("tcp: {e}")),
                    },
                    None => (false, "нет адреса".to_owned()),
                },
                Err(e) => (false, format!("dns: {e}")),
            },
        };
        (ok, start.elapsed().as_millis(), why)
    }
}

// ── Элевированные операции со службой ────────────────────────────────────────

/// Путь к файлу-логу элевированных операций (для возврата сообщений в GUI).
fn svc_log_path() -> std::path::PathBuf {
    handshake_dir().join("zaprust_svc.log")
}

fn write_svc_log(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(svc_log_path())
    {
        let _ = writeln!(f, "{msg}");
    }
}

fn read_svc_log() -> Vec<String> {
    std::fs::read_to_string(svc_log_path())
        .map(|t| t.lines().map(|l| l.to_owned()).collect())
        .unwrap_or_default()
}

/// Выполнить операцию со службой (запускается уже под админом). Возвращает код выхода.
fn run_service_command(args: &[String]) -> i32 {
    let _ = std::fs::remove_file(svc_log_path()); // чистим прошлые сообщения
    let action = args.first().map(|s| s.as_str()).unwrap_or("");
    logging::info("svc", format!("операция службы: {action} (elevated={})", host().is_elevated()));

    let result: Result<(), String> = match action {
        "install" => {
            let name = args.get(1).map(|s| s.as_str()).unwrap_or("");
            let gf = args.get(2).map(|s| s == "1").unwrap_or(false);
            logging::info("svc", format!("install стратегии: {name}, game_filter={gf}"));
            host().install(name, gf)
        }
        "remove" => host().remove(),
        "start" => host().start(),
        "stop" => host().stop(),
        "uninstall" => host().uninstall(),
        other => Err(format!("неизвестная операция службы: {other}")),
    };

    match result {
        Ok(()) => {
            logging::info("svc", format!("{action}: ok"));
            write_op_result(action, true, None);
            0
        }
        Err(e) => {
            logging::error("svc", format!("{action}: {e}"));
            write_svc_log(&e);
            write_op_result(action, false, Some(e));
            1
        }
    }
}

/// GUI-сторона обновления (без прав): проверка → скачивание → замена. На Windows —
/// один источник Flowseal с элевированной заменой. На Linux — два источника
/// (Flowseal + nfqws), элевация только если служба установлена.
fn run_update(
    core_dir: Option<std::path::PathBuf>,
    strategy: Option<String>,
    game_filter: bool,
    log_tx: &Sender<String>,
) -> Result<String, String> {
    #[cfg(not(windows))]
    {
        run_update_linux(core_dir, strategy, game_filter, log_tx)
    }
    #[cfg(windows)]
    {
        run_update_windows(core_dir, strategy, game_filter, log_tx)
    }
}

/// Linux: проверить обе версии (Flowseal + nfqws), скачать изменившееся без прав,
/// затем применить. Если служба установлена — заменить в ОДНОМ элевированном
/// реинвоке (стоп демона → замена под root → chown обратно → перезапуск); иначе
/// записать прямо как пользователь, без диалога.
#[cfg(not(windows))]
fn run_update_linux(
    core_dir: Option<std::path::PathBuf>,
    strategy: Option<String>,
    game_filter: bool,
    log_tx: &Sender<String>,
) -> Result<String, String> {
    let core_dir = core_dir.ok_or_else(|| "ядро не найдено".to_owned())?;
    let agent = host().agent();

    let _ = log_tx.send("обновление: проверяю релизы Flowseal и bol-van…".to_owned());
    let fs = updater::check_latest(&agent)?;
    let bv = updater::check_latest_nfqws(&agent)?;
    let fs_local = updater::local_version(&core_dir);
    let bv_local = updater::local_nfqws_version(&core_dir);
    let fs_upd = fs_local.as_deref() != Some(fs.tag.as_str());
    let bv_upd = bv_local.as_deref() != Some(bv.tag.as_str());

    if !fs_upd && !bv_upd {
        return Ok(format!("уже актуально (Flowseal {}, nfqws {})", fs.tag, bv.tag));
    }
    if fs_upd {
        let _ = log_tx.send(format!(
            "обновление: Flowseal {} (текущая: {})",
            fs.tag,
            fs_local.as_deref().unwrap_or("неизвестна")
        ));
    }
    if bv_upd {
        let _ = log_tx.send(format!(
            "обновление: nfqws {} (текущая: {})",
            bv.tag,
            bv_local.as_deref().unwrap_or("неизвестна")
        ));
    }

    // Скачиваем нужное (запись в /tmp прав не требует).
    let dl = host().download_agent();
    let fs_zip = std::env::temp_dir().join("zaprust_flowseal_upd.zip");
    let bv_zip = std::env::temp_dir().join("zaprust_nfqws_upd.zip");
    if fs_upd {
        download_with_progress(&dl, &fs.zip_url, &fs_zip, log_tx, "обновление (Flowseal)")?;
    }
    if bv_upd {
        download_with_progress(&dl, &bv.zip_url, &bv_zip, log_tx, "обновление (nfqws)")?;
    }

    let done_msg = || {
        let mut parts = Vec::new();
        if fs_upd {
            parts.push(format!("Flowseal {}", fs.tag));
        }
        if bv_upd {
            parts.push(format!("nfqws {}", bv.tag));
        }
        format!("обновлено: {}", parts.join(" + "))
    };

    // Служба не установлена → применяем как пользователь, без polkit-диалога.
    if !host().state().installed() {
        if fs_upd {
            updater::apply(&core_dir, &fs_zip)?;
            updater::write_version(&core_dir, &fs.tag)?;
        }
        if bv_upd {
            updater::extract_nfqws(&bv_zip, &core_dir)?;
            updater::write_nfqws_version(&core_dir, &bv.tag)?;
        }
        let _ = std::fs::remove_file(&fs_zip);
        let _ = std::fs::remove_file(&bv_zip);
        return Ok(done_msg());
    }

    // Служба установлена → один элевированный реинвок: стоп демона/правил →
    // замена ядра под root → chown файлов обратно пользователю → перезапуск службы
    // на той же стратегии. «-» в позиции zip = этот источник не обновляем.
    let _ = log_tx.send("обновление: останавливаю службу и заменяю ядро (pkexec)…".to_owned());
    let fs_arg = if fs_upd { fs_zip.to_string_lossy().to_string() } else { "-".to_owned() };
    let bv_arg = if bv_upd { bv_zip.to_string_lossy().to_string() } else { "-".to_owned() };
    let gf = if game_filter { "1" } else { "0" };
    let mut argv: Vec<&str> = vec!["--apply-update", &fs_arg, &fs.tag, &bv_arg, &bv.tag];
    if let Some(name) = &strategy {
        argv.push(name);
        argv.push(gf);
    }

    let result = match host().run_elevated_self(&argv) {
        Ok(0) => Ok(done_msg()),
        Ok(code) => {
            for line in read_svc_log() {
                let _ = log_tx.send(format!("обновление: {line}"));
            }
            Err(format!("установка не удалась (код {code})"))
        }
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_file(&fs_zip);
    let _ = std::fs::remove_file(&bv_zip);
    result
}

/// Windows: проверка → скачивание → элевированная замена (один релиз Flowseal).
#[cfg(windows)]
fn run_update_windows(
    core_dir: Option<std::path::PathBuf>,
    strategy: Option<String>,
    game_filter: bool,
    log_tx: &Sender<String>,
) -> Result<String, String> {
    let core_dir = core_dir.ok_or_else(|| "ядро не найдено".to_owned())?;
    let agent = host().agent();

    let _ = log_tx.send("обновление: проверяю последний релиз Flowseal…".to_owned());
    let latest = updater::check_latest(&agent)?;
    let local = updater::local_version(&core_dir);

    if local.as_deref() == Some(latest.tag.as_str()) {
        return Ok(format!("уже актуально ({})", latest.tag));
    }
    let _ = log_tx.send(format!(
        "обновление: доступна {} (текущая: {})",
        latest.tag,
        local.as_deref().unwrap_or("неизвестна")
    ));

    let zip_path = std::env::temp_dir().join("zaprust_core_update.zip");
    let _ = log_tx.send("обновление: скачиваю ядро…".to_owned());
    let dl = host().download_agent();
    let mut last_pct: u64 = 0;
    updater::download(&dl, &latest.zip_url, &zip_path, |done, total| {
        if let Some(t) = total.filter(|t| *t > 0) {
            let pct = done * 100 / t;
            if pct >= last_pct + 20 {
                last_pct = pct;
                let _ = log_tx.send(format!("обновление: скачано {pct}%"));
            }
        }
    })?;

    let _ = log_tx.send("обновление: останавливаю обход и заменяю ядро (UAC)…".to_owned());
    let zip_str = zip_path.to_string_lossy().to_string();
    let gf = if game_filter { "1" } else { "0" };
    let mut argv: Vec<&str> = vec!["--apply-update", &zip_str, &latest.tag];
    if let Some(name) = &strategy {
        argv.push(name);
        argv.push(gf);
    }

    match host().run_elevated_self(&argv) {
        Ok(0) => Ok(format!("обновлено до {}", latest.tag)),
        Ok(code) => {
            for line in read_svc_log() {
                let _ = log_tx.send(format!("обновление: {line}"));
            }
            Err(format!("установка не удалась (код {code})"))
        }
        Err(e) => Err(e),
    }
}

/// Первая установка ядра. На Windows — один релиз Flowseal (винда-движок внутри).
/// На Linux — ДВА источника (ассеты Flowseal + движок nfqws bol-van), запись в
/// пользовательский XDG-каталог прав не требует (элевация нужна только службе).
fn download_core_impl(target: &std::path::Path, log_tx: &Sender<String>) -> Result<String, String> {
    #[cfg(not(windows))]
    {
        download_core_linux(target, log_tx)
    }
    #[cfg(windows)]
    {
        download_core_windows(target, log_tx)
    }
}

/// Linux: получить ядро из ДВУХ источников без элевации.
///   1) ассеты Flowseal (стратегии `.bat` + `bin/*.bin` + `lists/*.txt`), виндовые
///      файлы (winws.exe/WinDivert*/cygwin1.dll/*.exe) выкидываются при распаковке;
///   2) движок `nfqws` из релиза bol-van/zapret → `core/nfqws` (+ chmod 755).
///
/// Версии ведутся раздельно: `version.txt` (Flowseal) и `nfqws-version.txt` (bol-van).
#[cfg(not(windows))]
fn download_core_linux(
    target: &std::path::Path,
    log_tx: &Sender<String>,
) -> Result<String, String> {
    let agent = host().agent();
    let dl = host().download_agent();

    // 1) Ассеты Flowseal.
    let _ = log_tx.send("ядро: проверяю релиз Flowseal…".to_owned());
    let fs = updater::check_latest(&agent)?;
    let _ = log_tx.send(format!("ядро: качаю ассеты Flowseal {}…", fs.tag));
    let fs_zip = std::env::temp_dir().join("zaprust_flowseal.zip");
    download_with_progress(&dl, &fs.zip_url, &fs_zip, log_tx, "ядро (Flowseal)")?;
    updater::apply(target, &fs_zip)?;
    updater::write_version(target, &fs.tag)?;

    // 2) Движок nfqws (bol-van/zapret).
    let _ = log_tx.send("ядро: проверяю релиз bol-van/zapret (nfqws)…".to_owned());
    let bv = updater::check_latest_nfqws(&agent)?;
    let _ = log_tx.send(format!("ядро: качаю nfqws {}…", bv.tag));
    let bv_zip = std::env::temp_dir().join("zaprust_nfqws.zip");
    download_with_progress(&dl, &bv.zip_url, &bv_zip, log_tx, "ядро (nfqws)")?;
    updater::extract_nfqws(&bv_zip, target)?;
    updater::write_nfqws_version(target, &bv.tag)?;

    let _ = std::fs::remove_file(&fs_zip);
    let _ = std::fs::remove_file(&bv_zip);
    Ok(format!("установлено: Flowseal {} + nfqws {}", fs.tag, bv.tag))
}

/// Скачать файл с логом прогресса (каждые ~20%). Общий помощник для получения и
/// обновления ядра.
#[allow(dead_code)] // на Windows используется только частью путей
fn download_with_progress(
    agent: &ureq::Agent,
    url: &str,
    dest: &std::path::Path,
    log_tx: &Sender<String>,
    what: &str,
) -> Result<(), String> {
    let mut last_pct: u64 = 0;
    updater::download(agent, url, dest, |done, total| {
        if let Some(t) = total.filter(|t| *t > 0) {
            let pct = done * 100 / t;
            if pct >= last_pct + 20 {
                last_pct = pct;
                let _ = log_tx.send(format!("{what}: скачано {pct}%"));
            }
        }
    })
}

/// Windows: первая установка ядра Flowseal. Пытаемся без прав; если папка непишемая
/// (Program Files) — через элевацию (UAC).
#[cfg(windows)]
fn download_core_windows(
    target: &std::path::Path,
    log_tx: &Sender<String>,
) -> Result<String, String> {
    let agent = host().agent();
    let _ = log_tx.send("ядро: проверяю последний релиз Flowseal…".to_owned());
    let latest = updater::check_latest(&agent)?;
    let _ = log_tx.send(format!("ядро: скачиваю {}…", latest.tag));

    let zip_path = std::env::temp_dir().join("zaprust_core_download.zip");
    let dl = host().download_agent();
    let mut last_pct: u64 = 0;
    updater::download(&dl, &latest.zip_url, &zip_path, |done, total| {
        if let Some(t) = total.filter(|t| *t > 0) {
            let pct = done * 100 / t;
            if pct >= last_pct + 20 {
                last_pct = pct;
                let _ = log_tx.send(format!("ядро: скачано {pct}%"));
            }
        }
    })?;

    let _ = log_tx.send("ядро: распаковываю и устанавливаю…".to_owned());
    let direct = updater::apply(target, &zip_path)
        .and_then(|()| updater::write_version(target, &latest.tag));

    match direct {
        Ok(()) => Ok(format!("установлено {}", latest.tag)),
        Err(e) => {
            // Вероятно нет прав на запись — пробуем через элевацию (UAC).
            let _ = log_tx.send(format!("ядро: прямая установка не вышла ({e}), пробую с UAC…"));
            let zip_str = zip_path.to_string_lossy().to_string();
            match host().run_elevated_self(&["--apply-update", &zip_str, &latest.tag]) {
                Ok(0) => Ok(format!("установлено {}", latest.tag)),
                Ok(code) => {
                    for line in read_svc_log() {
                        let _ = log_tx.send(format!("ядро: {line}"));
                    }
                    Err(format!("установка не удалась (код {code})"))
                }
                Err(e) => Err(e),
            }
        }
    }
}

/// Элевированный воркер замены ядра.
/// Windows: args = [zip, tag, strategy?, gf?].
/// Linux:   args = [flowseal_zip|-, flowseal_tag, nfqws_zip|-, nfqws_tag, strategy?, gf?].
fn apply_update_command(args: &[String]) -> i32 {
    #[cfg(not(windows))]
    {
        apply_update_linux(args)
    }
    #[cfg(windows)]
    {
        apply_update_windows(args)
    }
}

/// Linux: под root заменить движок/ассеты из переданных zip'ов («-» = пропустить
/// источник), вернув файлы во владение исходному пользователю, и перезапустить
/// службу. Демон/правила гасятся ДО замены (запущенный nfqws заменять некорректно).
#[cfg(not(windows))]
fn apply_update_linux(args: &[String]) -> i32 {
    let _ = std::fs::remove_file(svc_log_path());

    let skip = |a: Option<&String>| -> Option<String> {
        a.filter(|s| s.as_str() != "-" && !s.is_empty()).cloned()
    };
    let fs_zip = skip(args.first());
    let fs_tag = args.get(1).cloned().unwrap_or_default();
    let bv_zip = skip(args.get(2));
    let bv_tag = args.get(3).cloned().unwrap_or_default();
    let strategy = args.get(4).cloned();
    let game_filter = args.get(5).map(|s| s == "1").unwrap_or(false);

    if fs_zip.is_none() && bv_zip.is_none() {
        write_svc_log("нечего заменять (оба источника пропущены)");
        return 1;
    }

    let core_dir = host().core_dir().unwrap_or_else(|| host().preferred_core_dir());

    // Останавливаем службу и гасим демон/правила ДО замены файлов.
    let was_running = host().state().installed();
    if was_running {
        let _ = host().remove();
    }
    host().reset_engine();
    std::thread::sleep(std::time::Duration::from_millis(800));

    if let Some(zip) = &fs_zip {
        if let Err(e) = updater::apply(&core_dir, std::path::Path::new(zip)) {
            write_svc_log(&format!("замена ассетов Flowseal: {e}"));
            return 1;
        }
        if let Err(e) = updater::write_version(&core_dir, &fs_tag) {
            write_svc_log(&format!("запись версии Flowseal: {e}"));
        }
    }
    if let Some(zip) = &bv_zip {
        if let Err(e) = updater::extract_nfqws(std::path::Path::new(zip), &core_dir) {
            write_svc_log(&format!("замена nfqws: {e}"));
            return 1;
        }
        if let Err(e) = updater::write_nfqws_version(&core_dir, &bv_tag) {
            write_svc_log(&format!("запись версии nfqws: {e}"));
        }
    }

    // Файлы создавались под root — вернуть владельца исходному пользователю, иначе
    // неэлевированный GUI/nfqws (--uid) не прочитает ядро в домашнем каталоге.
    host().fixup_owner(&core_dir);

    if was_running {
        if let Some(name) = strategy {
            if let Err(e) = host().install(&name, game_filter) {
                write_svc_log(&format!("перезапуск службы: {e}"));
                return 1;
            }
        }
    }
    0
}

/// Windows: элевированный воркер замены ядра. args: [zip, tag, strategy?, gf?].
#[cfg(windows)]
fn apply_update_windows(args: &[String]) -> i32 {
    let _ = std::fs::remove_file(svc_log_path());

    let Some(zip) = args.first() else {
        write_svc_log("нет пути к zip");
        return 1;
    };
    let tag = args.get(1).cloned().unwrap_or_default();
    let strategy = args.get(2).cloned();
    let game_filter = args.get(3).map(|s| s == "1").unwrap_or(false);

    // Для первой установки папки core может ещё не быть — берём предпочтительную.
    let core_dir = host().core_dir().unwrap_or_else(|| host().preferred_core_dir());

    // Снимаем блокировку файлов: если служба стоит — удаляем (движок отпустит файлы).
    let was_running = host().state().installed();
    if was_running {
        let _ = host().remove();
    }
    // Сбрасываем движок перехвата — иначе файлы ядра заняты и не заменятся
    // (Win: выгрузка драйвера WinDivert).
    host().reset_engine();
    std::thread::sleep(std::time::Duration::from_millis(800));

    // Заменяем ядро целиком (с сохранением *-user.txt).
    if let Err(e) = updater::apply(&core_dir, std::path::Path::new(zip)) {
        write_svc_log(&format!("замена ядра: {e}"));
        return 1;
    }
    if let Err(e) = updater::write_version(&core_dir, &tag) {
        write_svc_log(&format!("запись версии: {e}"));
    }

    // Поднимаем обход заново на той же стратегии (если был запущен).
    if was_running {
        if let Some(name) = strategy {
            match host().install(&name, game_filter) {
                Ok(()) => {}
                Err(e) => {
                    write_svc_log(&format!("перезапуск службы: {e}"));
                    return 1;
                }
            }
        }
    }
    0
}

// ── Автоподбор стратегии (killer-фича) ───────────────────────────────────────

/// Прогрев движка перед замером (на Windows за это время привязывается WinDivert).
const WARMUP_MS: u64 = 900;

/// Контрольные цели для отбора (минимум): YouTube + Discord по TLS.
fn autoselect_targets() -> Vec<Probe> {
    vec![
        Probe::Https("https://www.youtube.com".to_owned()),
        Probe::Https("https://discord.com".to_owned()),
    ]
}

fn autoselect_progress_path() -> std::path::PathBuf {
    handshake_dir().join("zaprust_autoselect.jsonl")
}
fn autoselect_cancel_path() -> std::path::PathBuf {
    handshake_dir().join("zaprust_autoselect.cancel")
}

/// Прогресс одной итерации подбора (JSONL: элевированный пишет, GUI читает).
#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
struct AutoProgress {
    idx: usize,
    total: usize,
    strategy: String,
    /// probe | lkg | verify | installed | none | canceled | error
    stage: String,
    /// "" | ok | fail
    result: String,
    ping: u32,
}

fn write_progress(p: &AutoProgress) {
    use std::io::Write;
    if let Ok(line) = serde_json::to_string(p) {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(autoselect_progress_path())
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn autoselect_canceled() -> bool {
    autoselect_cancel_path().exists()
}

fn median(v: &[u128]) -> u128 {
    if v.is_empty() {
        return 0;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    s[s.len() / 2]
}


/// Параллельно замерить цели: вернуть (достижимо, мс, причина) по каждой в исходном порядке.
fn measure_targets(targets: &[Probe], agent: &ureq::Agent) -> Vec<(bool, u128, String)> {
    let mut handles = Vec::with_capacity(targets.len());
    for t in targets {
        let a = agent.clone();
        let t = t.clone();
        handles.push(std::thread::spawn(move || t.check_detail(&a)));
    }
    handles
        .into_iter()
        .map(|h| h.join().unwrap_or((false, 0, "join error".to_owned())))
        .collect()
}

/// Проба одной стратегии: spawn → warmup → гейт по целям (fail-fast) →
/// при успехе ещё 2 замера на цель → медиана. Возвращает сумму медиан-пингов
/// (None — стратегия не пробила). Движок поднимается через BypassRuntime; его
/// сворачивание — в Drop хэндла (драйвер/правила между пробами не трогаем).
fn probe_strategy(
    s: &strategies::Strategy,
    game_filter: bool,
    targets: &[Probe],
    agent: &ureq::Agent,
) -> Option<u32> {
    let mut handle = match host().spawn_probe(s, game_filter) {
        Ok(h) => h,
        Err(e) => {
            logging::error("autoselect", format!("[{}] не запустился движок: {e}", s.name));
            return None;
        }
    };
    std::thread::sleep(Duration::from_millis(WARMUP_MS)); // прогрев движка перехвата

    // Если движок мгновенно умер — частая причина (AV/драйвер/аргументы).
    if let Some(code) = handle.try_exit() {
        let out = host().last_engine_output();
        logging::error(
            "autoselect",
            format!(
                "[{}] движок сразу вышел (код {code}){}",
                s.name,
                if out.is_empty() { String::new() } else { format!(" · {out}") }
            ),
        );
        std::thread::sleep(Duration::from_millis(250));
        return None;
    }

    // Гейт: все цели должны пройти. Логируем причину по каждой.
    let gate = measure_targets(targets, agent);
    for (t, (ok, ms, why)) in targets.iter().zip(gate.iter()) {
        logging::info(
            "autoselect",
            format!(
                "[{}] {} → {} {ms}ms{}",
                s.name,
                t.label(),
                if *ok { "OK" } else { "FAIL" },
                if *ok { String::new() } else { format!(" ({why})") }
            ),
        );
    }
    let passed = gate.iter().all(|(ok, _, _)| *ok);

    let ping = if passed {
        // Медиана из 3 замеров на цель (гейт + ещё 2) — только у кандидатов.
        let mut samples: Vec<Vec<u128>> = gate.iter().map(|(_, ms, _)| vec![*ms]).collect();
        for _ in 0..2 {
            let extra = measure_targets(targets, agent);
            for (i, (ok, ms, _)) in extra.iter().enumerate() {
                if *ok {
                    samples[i].push(*ms);
                }
            }
        }
        let sum: u128 = samples.iter().map(|v| median(v)).sum();
        Some(sum as u32)
    } else {
        None
    };

    drop(handle); // свернуть прогон движка (Drop глушит процесс)
    std::thread::sleep(Duration::from_millis(250)); // settle (драйвер не трогаем)
    ping
}

/// Финальная верификация победителя полным targets.txt: ≥70% целей доступны.
fn verify_winner(
    s: &strategies::Strategy,
    game_filter: bool,
    full: &[Probe],
    agent: &ureq::Agent,
) -> bool {
    let Ok(handle) = host().spawn_probe(s, game_filter) else {
        return false;
    };
    std::thread::sleep(Duration::from_millis(WARMUP_MS));
    let results = measure_targets(full, agent);
    drop(handle);
    std::thread::sleep(Duration::from_millis(250));

    let ok = results.iter().filter(|(o, _, _)| *o).count();
    logging::info(
        "autoselect",
        format!("верификация {}: {ok}/{} целей доступно", s.name, results.len()),
    );
    !results.is_empty() && ok * 100 >= results.len() * 70
}

/// Порядок проб: last-known-good → general → остальные (без дублей).
fn order_strategies(strats: &[strategies::Strategy], lkg: Option<&str>) -> Vec<strategies::Strategy> {
    let mut out: Vec<strategies::Strategy> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let push_named = |name: &str, out: &mut Vec<strategies::Strategy>, seen: &mut std::collections::HashSet<String>| {
        if let Some(s) = strats.iter().find(|s| s.name == name) {
            if seen.insert(s.name.clone()) {
                out.push(s.clone());
            }
        }
    };
    if let Some(l) = lkg {
        push_named(l, &mut out, &mut seen);
    }
    push_named("general", &mut out, &mut seen);
    for s in strats {
        if seen.insert(s.name.clone()) {
            out.push(s.clone());
        }
    }
    out
}

fn order_index(ordered: &[strategies::Strategy], name: &str) -> usize {
    ordered.iter().position(|s| s.name == name).unwrap_or(usize::MAX)
}

/// Полный список целей из core/utils/targets.txt (для верификации).
fn load_targets_file(core_dir: &std::path::Path) -> Vec<Probe> {
    let path = core_dir.join("utils").join("targets.txt");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let v: Vec<Probe> = text.lines().filter_map(Probe::parse_line).collect();
        if !v.is_empty() {
            return v;
        }
    }
    autoselect_targets()
}

/// Установить победителя службой (start=auto) + метка стратегии.
fn install_winner(name: &str, game_filter: bool) -> i32 {
    match host().install(name, game_filter) {
        Ok(()) => {
            logging::info("autoselect", format!("победитель установлен: {name}"));
            write_progress(&AutoProgress {
                strategy: name.to_owned(),
                stage: "installed".to_owned(),
                ..Default::default()
            });
            write_op_result("autoselect", true, None);
            0
        }
        Err(e) => {
            logging::error("autoselect", format!("установка победителя {name}: {e}"));
            write_svc_log(&format!("установка победителя: {e}"));
            write_op_result("autoselect", false, Some(e));
            1
        }
    }
}

// ── Редактор листов: запись файла (с фолбэком на элевацию) ───────────────────

/// Записать содержимое в файл списка. Пробуем напрямую; если папка непишема —
/// через элевацию (`--write-list dest tmp`).
fn write_list_with_fallback(dest: &std::path::Path, content: &[u8]) -> Result<(), String> {
    if std::fs::write(dest, content).is_ok() {
        return Ok(());
    }
    // Нет прав на запись — пишем во временный и копируем под админом.
    let tmp = std::env::temp_dir().join("zaprust_list_write.tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("временный файл: {e}"))?;
    let dest_s = dest.to_string_lossy().to_string();
    let tmp_s = tmp.to_string_lossy().to_string();
    match host().run_elevated_self(&["--write-list", &dest_s, &tmp_s]) {
        Ok(0) => Ok(()),
        Ok(code) => Err(format!("запись не удалась (код {code})")),
        Err(e) => Err(e),
    }
}

/// Элевированный воркер записи файла: копирует src → dest.
fn write_list_command(args: &[String]) -> i32 {
    let _ = std::fs::remove_file(svc_log_path());
    let (Some(dest), Some(src)) = (args.first(), args.get(1)) else {
        write_svc_log("--write-list: нужны dest и src");
        return 1;
    };
    match std::fs::copy(src, dest) {
        Ok(_) => 0,
        Err(e) => {
            write_svc_log(&format!("запись {dest}: {e}"));
            1
        }
    }
}

/// Элевированный воркер автоподбора. args: [game_filter("0"/"1"), last_known_good?].
fn autoselect_command(args: &[String]) -> i32 {
    let game_filter = args.first().map(|s| s == "1").unwrap_or(false);
    let lkg = args.get(1).cloned().filter(|s| !s.is_empty());

    let _ = std::fs::remove_file(autoselect_progress_path());
    let _ = std::fs::remove_file(autoselect_cancel_path());
    let _ = std::fs::remove_file(svc_log_path());

    let scan = host().scan();
    let Some(core_dir) = scan.core_dir.clone() else {
        write_svc_log("ядро не найдено");
        return 1;
    };
    if scan.strategies.is_empty() {
        write_svc_log("нет стратегий");
        return 1;
    }

    let ordered = order_strategies(&scan.strategies, lkg.as_deref());
    let total = ordered.len();
    let sel = autoselect_targets();
    let agent = host().probe_agent();
    logging::info(
        "autoselect",
        format!("старт: {total} стратегий, game_filter={game_filter}, lkg={lkg:?}"),
    );

    // Подготовка свипа платформой: создать недостающие вспомогательные списки,
    // снять активный обход, сбросить и прогреть движок перехвата. На Windows это
    // ровно прежняя последовательность (ensure_user_lists + reset WinDivert +
    // прайм); на Linux позже — поднятие правил nftables.
    host().prepare_sweep(&ordered, game_filter);

    // Быстрый путь: last-known-good. Прошёл гейт → ставим сразу.
    if let Some(name) = &lkg {
        if let Some(s) = ordered.iter().find(|s| &s.name == name) {
            write_progress(&AutoProgress {
                idx: 1,
                total,
                strategy: name.clone(),
                stage: "lkg".to_owned(),
                ..Default::default()
            });
            if probe_strategy(s, game_filter, &sel, &agent).is_some() {
                return install_winner(name, game_filter);
            }
        }
    }

    // Полный свип.
    let mut candidates: Vec<(String, u32)> = Vec::new();
    for (i, s) in ordered.iter().enumerate() {
        if autoselect_canceled() {
            // Активная уборка по отмене (а не только надежда на Drop прошлой пробы):
            // глушим любой движок перехвата и снимаем правила, службу НЕ ставим.
            // На Linux это критично — остаточные nft-правила исказили бы сеть.
            host().reset_engine();
            write_progress(&AutoProgress {
                stage: "canceled".to_owned(),
                ..Default::default()
            });
            logging::info("autoselect", "отменено пользователем — движок и правила сняты");
            return 2;
        }
        write_progress(&AutoProgress {
            idx: i + 1,
            total,
            strategy: s.name.clone(),
            stage: "probe".to_owned(),
            ..Default::default()
        });

        match probe_strategy(s, game_filter, &sel, &agent) {
            Some(ping) => {
                candidates.push((s.name.clone(), ping));
                write_progress(&AutoProgress {
                    idx: i + 1,
                    total,
                    strategy: s.name.clone(),
                    stage: "probe".to_owned(),
                    result: "ok".to_owned(),
                    ping,
                });
            }
            None => write_progress(&AutoProgress {
                idx: i + 1,
                total,
                strategy: s.name.clone(),
                stage: "probe".to_owned(),
                result: "fail".to_owned(),
                ..Default::default()
            }),
        }
    }

    // Выбор: мин. сумма пингов; тай-брейк (<15 мс) → раньше по порядку (general/lkg).
    if candidates.is_empty() {
        logging::warn(
            "autoselect",
            "ни одна стратегия не пробила YouTube+Discord. Если в логах по целям видно 'dns:' — у провайдера блокировка DNS: включите Secure DNS (DoH) в браузере/системе. winws не обходит DNS-блокировку.",
        );
        write_progress(&AutoProgress {
            stage: "none".to_owned(),
            ..Default::default()
        });
        return 3;
    }
    logging::info("autoselect", format!("кандидатов прошло: {}", candidates.len()));
    candidates.sort_by_key(|c| c.1);
    let best_ping = candidates[0].1;
    let winner = candidates
        .iter()
        .filter(|c| c.1 <= best_ping + 15)
        .min_by_key(|c| order_index(&ordered, &c.0))
        .map(|c| c.0.clone())
        .unwrap_or_else(|| candidates[0].0.clone());

    // Финальная верификация полным targets.txt.
    write_progress(&AutoProgress {
        strategy: winner.clone(),
        stage: "verify".to_owned(),
        ..Default::default()
    });
    let full = load_targets_file(&core_dir);
    if let Some(ws) = ordered.iter().find(|s| s.name == winner) {
        if !verify_winner(ws, game_filter, &full, &agent) {
            write_progress(&AutoProgress {
                stage: "none".to_owned(),
                ..Default::default()
            });
            return 3;
        }
    }

    install_winner(&winner, game_filter)
}

/// Шапка окружения одной пачкой при старте каждого процесса.
fn log_env_header() {
    let ver = env!("CARGO_PKG_VERSION");
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    logging::info("env", format!("Zaprust {ver} ({profile}), elevated={}", host().is_elevated()));
    logging::info("env", format!("ос: {}", host().os_version()));
    for line in host().diag_lines() {
        logging::info("env", line);
    }
    if let Ok(exe) = std::env::current_exe() {
        let p = exe.display().to_string();
        logging::info("env", format!("exe: {p}"));
        if p.contains(' ') || !p.is_ascii() {
            logging::warn("env", "путь к exe содержит пробел/не-ASCII — zapret и служба могут не работать");
        }
    }
    match host().core_dir() {
        Some(core) => {
            let cs = core.display().to_string();
            logging::info("env", format!("core: {cs}"));
            if cs.contains(' ') || !cs.is_ascii() {
                logging::warn("env", "путь к core содержит пробел/не-ASCII");
            }
            logging::info("env", format!("core: {}", host().engine_diag()));
        }
        None => logging::warn("env", "ядро не найдено (папка core/)"),
    }
}

// ── Хэндшейк результата между процессами (фикс рассинхрона статуса) ──────────

/// Каталог для межпроцессных файлов-хэндшейков между неэлевированным GUI и
/// элевированным реинвоком. На Linux это ВСЕГДА `/tmp`: pkexec вычищает
/// окружение, и `$TMPDIR` до реинвока не доезжает — он видит дефолтный /tmp.
/// Закрепляем /tmp и на стороне GUI, иначе при заданном `$TMPDIR` стороны
/// смотрели бы в разные каталоги. (Файлы, чей путь передаётся реинвоку
/// аргументом — zip обновления, --write-list — этой проблемы не имеют.)
/// На Windows процессы одного пользователя, обычный temp_dir подходит.
fn handshake_dir() -> std::path::PathBuf {
    #[cfg(not(windows))]
    {
        std::path::PathBuf::from("/tmp")
    }
    #[cfg(windows)]
    {
        std::env::temp_dir()
    }
}

fn op_result_path() -> std::path::PathBuf {
    handshake_dir().join("zaprust_result.json")
}

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct OpResult {
    op: String,
    ok: bool,
    service_state: String,
    winws_pid: Option<u32>,
    error: Option<String>,
}

/// Элевированный реинвок пишет структурированный итог операции в общий temp.
fn write_op_result(op: &str, ok: bool, error: Option<String>) {
    let state = host().state();
    let r = OpResult {
        op: op.to_owned(),
        ok,
        service_state: format!("{state:?}"),
        winws_pid: host().engine_alive(),
        error,
    };
    if let Ok(j) = serde_json::to_string(&r) {
        let _ = std::fs::write(op_result_path(), j);
    }
    logging::info(
        "result",
        format!(
            "записан итог: op={op} ok={ok} service={:?} winws_pid={:?}",
            state,
            r.winws_pid
        ),
    );
}

fn read_op_result() -> Option<OpResult> {
    std::fs::read_to_string(op_result_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

// ── Диагностика (D) ──────────────────────────────────────────────────────────

/// Текст диагностики: шапка окружения + последние проблемы из лога.
fn diagnostics_text() -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "Zaprust {} ({})\n",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) { "debug" } else { "release" }
    ));
    s.push_str(&format!("elevated: {}\n", host().is_elevated()));
    s.push_str(&format!("ос: {}\n", host().os_version()));
    for line in host().diag_lines() {
        s.push_str(&line);
        s.push('\n');
    }
    if let Ok(exe) = std::env::current_exe() {
        s.push_str(&format!("exe: {}\n", exe.display()));
    }
    if let Some(core) = host().core_dir() {
        s.push_str(&format!("core: {}\n", core.display()));
        s.push_str(&format!(
            "версия Flowseal: {}\n",
            updater::local_version(&core).as_deref().unwrap_or("нет")
        ));
        #[cfg(not(windows))]
        s.push_str(&format!(
            "версия nfqws: {}\n",
            updater::local_nfqws_version(&core).as_deref().unwrap_or("нет")
        ));
        s.push_str(&format!("core ready: {}\n", host().engine_diag()));
    } else {
        s.push_str("core: не найдено\n");
    }
    s.push_str(&format!(
        "service: {:?}, engine_alive: {:?}\n",
        host().state(),
        host().engine_alive()
    ));
    if let Some(p) = logging::log_path() {
        s.push_str(&format!("log: {}\n", p.display()));
    }
    s.push_str("\n— последние проблемы (ERROR/WARN) —\n");
    for line in logging::recent_problems(40) {
        s.push_str(&line);
        s.push('\n');
    }
    s
}

/// Тёмная плоская тема.
fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL_BG;
    visuals.window_fill = PANEL_BG;
    visuals.extreme_bg_color = LOG_BG;
    visuals.widgets.inactive.bg_fill = FIELD_BG;
    visuals.widgets.inactive.weak_bg_fill = FIELD_BG;
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(44, 48, 54);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(52, 56, 62);
    visuals.selection.bg_fill = ACCENT;
    visuals.widgets.noninteractive.rounding = Rounding::same(8.0);
    visuals.widgets.inactive.rounding = Rounding::same(8.0);
    visuals.widgets.hovered.rounding = Rounding::same(8.0);
    visuals.widgets.active.rounding = Rounding::same(8.0);

    let mut style = egui::Style::default();
    style.visuals = visuals;
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(10.0, 6.0);
    ctx.set_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_parses_targets_format() {
        match Probe::parse_line(r#"YouTubeWeb = "https://www.youtube.com""#) {
            Some(Probe::Https(u)) => assert_eq!(u, "https://www.youtube.com"),
            other => panic!("ожидали Https, got {:?}", other.map(|p| p.label())),
        }
        match Probe::parse_line(r#"CloudflareDNS1111 = "PING:1.1.1.1""#) {
            Some(Probe::Tcp(a)) => assert_eq!(a, "1.1.1.1:443"),
            other => panic!("ожидали Tcp, got {:?}", other.map(|p| p.label())),
        }
        assert!(Probe::parse_line("### Discord").is_none(), "комментарий пропускаем");
        assert!(Probe::parse_line("   ").is_none(), "пустую строку пропускаем");
        // bare-домен → https
        match Probe::parse_value("example.com") {
            Some(Probe::Https(u)) => assert_eq!(u, "https://example.com"),
            other => panic!("ожидали Https, got {:?}", other.map(|p| p.label())),
        }
    }
}
