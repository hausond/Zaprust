// Zaprust — лёгкий нативный GUI поверх сборки Flowseal/zapret-discord-youtube.
//
// Шаг 1: только визуальный каркас. Виджеты переключают локальное состояние,
// но никакой системной логики (процессы, файлы, сеть) здесь нет.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod logging;
mod service;
mod strategies;
mod updater;

use service::ServiceState;

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
        Some("--svc") | Some("--write-list") => "svc",
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
    // `zaprust --update-dry` — безопасная проверка апдейтера: скачать и применить
    // замену ядра во временную папку (рабочее ядро не трогаем).
    if cli.get(1).map(|s| s == "--update-dry").unwrap_or(false) {
        update_dry();
        return Ok(());
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

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([484.0, 644.0])
            .with_min_inner_size([484.0, 644.0])
            .with_max_inner_size([484.0, 644.0])
            .with_resizable(false)
            .with_icon(load_window_icon())
            .with_title("Zaprust"),
        ..Default::default()
    };

    eframe::run_native(
        "Zaprust",
        native_options,
        Box::new(|cc| Ok(Box::new(ZaprustApp::new(cc)))),
    )
}

/// Диагностика: вывести итоговый argv winws для стратегии.
fn dump_args(name: Option<&str>) {
    let scan = strategies::scan();
    for m in &scan.messages {
        eprintln!("# {m}");
    }
    let Some(core_dir) = scan.core_dir.clone() else {
        return;
    };
    let target = name.unwrap_or("general");
    match scan.strategies.iter().find(|s| s.name == target) {
        Some(strat) => {
            let args = strategies::resolve_game_filter(&strat.args, false);
            println!("exe:  {}", core_dir.join("bin").join("winws.exe").display());
            println!("cwd:  {}", core_dir.join("bin").display());
            println!("argc: {}", args.len());
            for (i, a) in args.iter().enumerate() {
                println!("[{i:02}] {a}");
            }
        }
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
    let agent = build_agent();
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
    if let Err(e) = updater::download(&agent, &latest.zip_url, &zip, |_, _| {}) {
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

/// Диагностика: детект WinDivert + прогон теста доменов в консоль.
fn test_net() {
    println!("WinDivert: {:?}", service::query("WinDivert"));
    println!("Служба zapret: {:?}", service::query(service::SERVICE_NAME));

    let scan = strategies::scan();
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
    let agent = build_agent();
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

/// Диагностика: спавн winws со стратегией, ожидание ~4с, печать его вывода.
fn test_run(name: Option<&str>) {
    let scan = strategies::scan();
    let Some(core_dir) = scan.core_dir.clone() else {
        eprintln!("ядро не найдено");
        return;
    };
    let target = name.unwrap_or("general");
    let Some(strat) = scan.strategies.iter().find(|s| s.name == target) else {
        eprintln!("стратегия не найдена: {target}");
        return;
    };

    let bin_dir = core_dir.join("bin");
    let exe = bin_dir.join("winws.exe");
    let args = strategies::resolve_game_filter(&strat.args, false);

    let log_path = std::env::temp_dir().join("zaprust_winws_test.log");
    let file = std::fs::File::create(&log_path).expect("create log");
    let err = file.try_clone().map(Stdio::from).unwrap_or_else(|_| Stdio::null());

    let mut cmd = Command::new(&exe);
    cmd.args(&args)
        .current_dir(&bin_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(err);

    println!("spawn: {} ({} args)", exe.display(), args.len());
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::sleep(Duration::from_secs(4));
            match child.try_wait() {
                Ok(Some(status)) => println!("winws завершился сам, код {:?}", status.code()),
                Ok(None) => {
                    println!("winws ещё жив через 4с — глушу");
                    let _ = child.kill();
                    let _ = child.wait();
                }
                Err(e) => println!("try_wait error: {e}"),
            }
        }
        Err(e) => println!("spawn error: {e}"),
    }

    println!("---- вывод winws ({}) ----", log_path.display());
    match std::fs::read(&log_path) {
        Ok(bytes) => println!("{}", String::from_utf8_lossy(&bytes)),
        Err(e) => println!("не прочитать лог: {e}"),
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

        // Сканируем ядро при старте (чисто файловая, синхронная операция).
        let core = strategies::scan();
        let mut log_lines: Vec<String> = core.messages.iter().cloned().collect();
        log_lines.push("zaprust: интерфейс запущен".to_owned());

        // Загружаем сохранённые настройки.
        let cfg = config::Config::load();

        // Синхронно узнаём состояние службы и её стратегию (быстро, один раз).
        let service0 = service::query(service::SERVICE_NAME);

        // Режим smart выбран, если в конфиге спец-значение.
        let smart_selected = cfg.strategy.as_deref() == Some(config::SMART);

        // Last-known-good победитель: из конфига, иначе — из метки активной службы.
        let auto_best = cfg.auto_best.clone().or_else(|| {
            if service0.installed() {
                service::installed_strategy()
            } else {
                None
            }
        });

        // Восстанавливаем выбранную РЕАЛЬНУЮ стратегию по имени (если не smart):
        // приоритет — реально запущенная служба, затем конфиг.
        let pick_name = if service0.installed() {
            service::installed_strategy()
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

        // Поддерживаем состояние службы свежим.
        check_service(service_tx.clone());

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


    /// Готово ли ядро к работе: версия + ключевые файлы + распарсенные стратегии.
    fn core_ready(&self) -> bool {
        let Some(dir) = &self.core.core_dir else {
            return false;
        };
        !self.core.strategies.is_empty()
            && updater::local_version(dir).is_some()
            && dir.join("bin").join("winws.exe").exists()
            && dir.join("bin").join("WinDivert.dll").exists()
            && dir.join("bin").join("WinDivert64.sys").exists()
    }

    /// Пересканировать ядро (после загрузки/обновления/подбора).
    fn reload_core(&mut self) {
        self.core = strategies::scan();
        if self.selected_strategy >= self.core.strategies.len() {
            self.selected_strategy = 0;
        }
        // Если служба установлена — подставить её стратегию (победителя подбора).
        if let Some(name) = service::installed_strategy() {
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
            .unwrap_or_else(strategies::preferred_core_dir);
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
            let _ = svc_tx.send(service::query(service::SERVICE_NAME));
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
            let agent = build_agent();
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

    /// Запущен ли обход (есть живой процесс winws).
    /// Обход активен = служба запущена.
    fn is_running(&self) -> bool {
        self.service == ServiceState::Running
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
            let res = run_elevated_self(&["--autoselect", gf, &lkg]);
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
            let _ = svc_tx.send(service::query(service::SERVICE_NAME));
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
            let agent = build_agent();
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
                open_in_explorer(&dir);
                self.log(format!("логи: {}", dir.display()));
            }
            None => self.log("папка логов не определена"),
        }
    }

    fn copy_diagnostics(&mut self) {
        if set_clipboard(&diagnostics_text()) {
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
            let code = run_elevated_self(&argv);
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

            // Авторитетный поллинг ~5с (учитываем START_PENDING и квирк winws-как-службы).
            let mut authoritative = false;
            for _ in 0..16 {
                let st = service::query(service::SERVICE_NAME);
                let _ = svc_tx.send(st);
                authoritative = authoritative_running();
                if expect_running && authoritative {
                    break;
                }
                if expect_removed
                    && st == ServiceState::NotInstalled
                    && service::winws_alive().is_none()
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
            }

            // Реинвок отчитался ok, но обход не поднялся → явная ошибка состояния.
            if matches!(code, Ok(0)) && expect_running && !authoritative {
                let st = service::query(service::SERVICE_NAME);
                let winws = service::winws_alive();
                let msg = format!(
                    "обход НЕ поднялся: служба={st:?}, winws_pid={winws:?}. Подробности в логах."
                );
                logging::error("gui", &msg);
                let _ = log_tx.send(format!("ОШИБКА: {msg}"));
                op_failed.store(true, Ordering::SeqCst);
            }

            let _ = svc_tx.send(service::query(service::SERVICE_NAME));
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
            check_service(self.service_tx.clone());
        }

        // Автоподбор успешно завершился — запомнить победителя в auto_best.
        if self.autoselect_applied.swap(false, Ordering::SeqCst) {
            if let Some(name) = service::installed_strategy() {
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
                if self.op_failed.load(Ordering::Relaxed) {
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

// ── Проверки состояния служб (WinDivert / zapret) ────────────────────────────

/// Фоновая проверка состояния службы/драйвера по имени → канал.
fn check_state(name: &'static str, tx: Sender<ServiceState>) {
    std::thread::spawn(move || {
        let _ = tx.send(service::query(name));
    });
}

/// Фоновая проверка состояния службы zapret → канал.
fn check_service(tx: Sender<ServiceState>) {
    check_state(service::SERVICE_NAME, tx);
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
        let start = Instant::now();
        let ok = match self {
            Probe::Https(url) => match agent.get(url).call() {
                Ok(_) => true,
                Err(ureq::Error::Status(_, _)) => true, // сервер ответил — домен доступен
                Err(_) => false,
            },
            Probe::Tcp(addr) => match addr.to_socket_addrs() {
                Ok(mut addrs) => addrs
                    .next()
                    .map(|sa| TcpStream::connect_timeout(&sa, Duration::from_secs(3)).is_ok())
                    .unwrap_or(false),
                Err(_) => false,
            },
        };
        (ok, start.elapsed().as_millis())
    }
}

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

/// Агент для кнопки «Тест» (умеренные таймауты, следует редиректам).
fn build_agent() -> ureq::Agent {
    build_agent_with(3000, 4000, 5)
}

/// Агент для автоподбора: тугие таймауты, без редиректов (чистый замер).
fn build_probe_agent() -> ureq::Agent {
    build_agent_with(1500, 1800, 0)
}

// ── Элевированные операции со службой ────────────────────────────────────────

/// Путь к файлу-логу элевированных операций (для возврата сообщений в GUI).
fn svc_log_path() -> std::path::PathBuf {
    std::env::temp_dir().join("zaprust_svc.log")
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
    logging::info("svc", format!("операция службы: {action} (elevated={})", is_elevated()));

    let result: Result<(), String> = match action {
        "install" => {
            let name = args.get(1).map(|s| s.as_str()).unwrap_or("");
            let gf = args.get(2).map(|s| s == "1").unwrap_or(false);
            logging::info("svc", format!("install стратегии: {name}, game_filter={gf}"));
            install_service_elevated(name, gf)
        }
        "remove" => service::remove(),
        "start" => service::start(),
        "stop" => service::stop(),
        "uninstall" => {
            let _ = service::remove(); // удалить службу (если есть)
            service::stop_driver(); // выгрузить WinDivert/WinDivert14
            Ok(())
        }
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

/// Сборка аргументов выбранной стратегии и установка службы (под админом).
fn install_service_elevated(strategy_name: &str, game_filter: bool) -> Result<(), String> {
    let scan = strategies::scan();
    let core_dir = scan.core_dir.ok_or_else(|| "ядро не найдено".to_owned())?;
    let strategy = scan
        .strategies
        .iter()
        .find(|s| s.name == strategy_name)
        .ok_or_else(|| format!("стратегия не найдена: {strategy_name}"))?;

    let exe = core_dir.join("bin").join("winws.exe");
    if !exe.exists() {
        return Err(format!("не найден {}", exe.display()));
    }
    let args = strategies::resolve_game_filter(&strategy.args, game_filter);
    service::install(&exe, &args, strategy_name)
}

/// GUI-сторона обновления (без прав): проверка → скачивание → элевированная замена.
/// Возвращает короткое сообщение об итоге.
fn run_update(
    core_dir: Option<std::path::PathBuf>,
    strategy: Option<String>,
    game_filter: bool,
    log_tx: &Sender<String>,
) -> Result<String, String> {
    let core_dir = core_dir.ok_or_else(|| "ядро не найдено".to_owned())?;
    let agent = build_agent();

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
    let mut last_pct: u64 = 0;
    updater::download(&agent, &latest.zip_url, &zip_path, |done, total| {
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

    match run_elevated_self(&argv) {
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

/// Первая установка ядра: скачать последний релиз и распаковать в target.
/// Пытаемся без прав; если папка непишемая (Program Files) — через элевацию.
fn download_core_impl(target: &std::path::Path, log_tx: &Sender<String>) -> Result<String, String> {
    let agent = build_agent();
    let _ = log_tx.send("ядро: проверяю последний релиз Flowseal…".to_owned());
    let latest = updater::check_latest(&agent)?;
    let _ = log_tx.send(format!("ядро: скачиваю {}…", latest.tag));

    let zip_path = std::env::temp_dir().join("zaprust_core_download.zip");
    let mut last_pct: u64 = 0;
    updater::download(&agent, &latest.zip_url, &zip_path, |done, total| {
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
            match run_elevated_self(&["--apply-update", &zip_str, &latest.tag]) {
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

/// Элевированный воркер замены ядра. args: [zip, tag, strategy?, gf?].
fn apply_update_command(args: &[String]) -> i32 {
    let _ = std::fs::remove_file(svc_log_path());

    let Some(zip) = args.first() else {
        write_svc_log("нет пути к zip");
        return 1;
    };
    let tag = args.get(1).cloned().unwrap_or_default();
    let strategy = args.get(2).cloned();
    let game_filter = args.get(3).map(|s| s == "1").unwrap_or(false);

    // Для первой установки папки core может ещё не быть — берём предпочтительную.
    let core_dir = strategies::find_core_dir().unwrap_or_else(strategies::preferred_core_dir);

    // Снимаем блокировку файлов: если служба стоит — удаляем (winws отпустит exe).
    let was_running = service::query(service::SERVICE_NAME).installed();
    if was_running {
        let _ = service::remove();
    }
    // Выгружаем драйвер WinDivert — иначе WinDivert64.sys/.dll заняты и не заменятся.
    service::stop_driver();
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
            match install_service_elevated(&name, game_filter) {
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

/// Контрольные цели для отбора (минимум): YouTube + Discord по TLS.
fn autoselect_targets() -> Vec<Probe> {
    vec![
        Probe::Https("https://www.youtube.com".to_owned()),
        Probe::Https("https://discord.com".to_owned()),
    ]
}

fn autoselect_progress_path() -> std::path::PathBuf {
    std::env::temp_dir().join("zaprust_autoselect.jsonl")
}
fn autoselect_cancel_path() -> std::path::PathBuf {
    std::env::temp_dir().join("zaprust_autoselect.cancel")
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

/// Спавн winws скрыто (CREATE_NO_WINDOW), без пайпов.
fn spawn_winws(core_dir: &std::path::Path, args: &[String]) -> std::io::Result<std::process::Child> {
    let bin_dir = core_dir.join("bin");
    let exe = bin_dir.join("winws.exe");
    let mut cmd = Command::new(&exe);
    cmd.args(args)
        .current_dir(&bin_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.spawn()
}

/// Параллельно замерить цели: вернуть (достижимо, мс) по каждой в исходном порядке.
fn measure_targets(targets: &[Probe], agent: &ureq::Agent) -> Vec<(bool, u128)> {
    let mut handles = Vec::with_capacity(targets.len());
    for t in targets {
        let a = agent.clone();
        let t = t.clone();
        handles.push(std::thread::spawn(move || t.check_timed(&a)));
    }
    handles
        .into_iter()
        .map(|h| h.join().unwrap_or((false, 0)))
        .collect()
}

/// Проба одной стратегии: spawn → warmup → гейт по целям (fail-fast) →
/// при успехе ещё 2 замера на цель → медиана. Возвращает сумму медиан-пингов
/// (None — стратегия не пробила). Драйвер WinDivert НЕ выгружается.
fn probe_strategy(
    core_dir: &std::path::Path,
    s: &strategies::Strategy,
    game_filter: bool,
    targets: &[Probe],
    agent: &ureq::Agent,
) -> Option<u32> {
    let args = strategies::resolve_game_filter(&s.args, game_filter);
    let mut child = spawn_winws(core_dir, &args).ok()?;
    std::thread::sleep(Duration::from_millis(400)); // warmup — WinDivert привязывается

    // Гейт: все цели должны пройти.
    let gate = measure_targets(targets, agent);
    let passed = gate.iter().all(|(ok, _)| *ok);

    let ping = if passed {
        // Медиана из 3 замеров на цель (гейт + ещё 2) — только у кандидатов.
        let mut samples: Vec<Vec<u128>> = gate.iter().map(|(_, ms)| vec![*ms]).collect();
        for _ in 0..2 {
            let extra = measure_targets(targets, agent);
            for (i, (ok, ms)) in extra.iter().enumerate() {
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

    let _ = child.kill();
    let _ = child.wait();
    std::thread::sleep(Duration::from_millis(250)); // settle (драйвер не трогаем)
    ping
}

/// Финальная верификация победителя полным targets.txt: ≥70% целей доступны.
fn verify_winner(
    core_dir: &std::path::Path,
    s: &strategies::Strategy,
    game_filter: bool,
    full: &[Probe],
    agent: &ureq::Agent,
) -> bool {
    let args = strategies::resolve_game_filter(&s.args, game_filter);
    let Ok(mut child) = spawn_winws(core_dir, &args) else {
        return false;
    };
    std::thread::sleep(Duration::from_millis(400));
    let results = measure_targets(full, agent);
    let _ = child.kill();
    let _ = child.wait();
    std::thread::sleep(Duration::from_millis(250));

    let ok = results.iter().filter(|(o, _)| *o).count();
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

/// Установить победителя службой (start=auto) + метка в реестр.
fn install_winner(name: &str, game_filter: bool) -> i32 {
    match install_service_elevated(name, game_filter) {
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
    match run_elevated_self(&["--write-list", &dest_s, &tmp_s]) {
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

    let scan = strategies::scan();
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
    let agent = build_probe_agent();

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
            if probe_strategy(&core_dir, s, game_filter, &sel, &agent).is_some() {
                return install_winner(name, game_filter);
            }
        }
    }

    // Полный свип.
    let mut candidates: Vec<(String, u32)> = Vec::new();
    for (i, s) in ordered.iter().enumerate() {
        if autoselect_canceled() {
            write_progress(&AutoProgress {
                stage: "canceled".to_owned(),
                ..Default::default()
            });
            return 2;
        }
        write_progress(&AutoProgress {
            idx: i + 1,
            total,
            strategy: s.name.clone(),
            stage: "probe".to_owned(),
            ..Default::default()
        });

        match probe_strategy(&core_dir, s, game_filter, &sel, &agent) {
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
        write_progress(&AutoProgress {
            stage: "none".to_owned(),
            ..Default::default()
        });
        return 3;
    }
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
        if !verify_winner(&core_dir, ws, game_filter, &full, &agent) {
            write_progress(&AutoProgress {
                stage: "none".to_owned(),
                ..Default::default()
            });
            return 3;
        }
    }

    install_winner(&winner, game_filter)
}

/// Запущены ли мы с правами администратора.
#[cfg(windows)]
fn is_elevated() -> bool {
    use std::ffi::c_void;
    #[repr(C)]
    struct TokenElevation {
        token_is_elevated: u32,
    }
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ELEVATION: i32 = 20;
    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: *mut c_void, desired: u32, token: *mut *mut c_void) -> i32;
        fn GetTokenInformation(
            token: *mut c_void,
            class: i32,
            info: *mut c_void,
            len: u32,
            ret_len: *mut u32,
        ) -> i32;
    }
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(h: *mut c_void) -> i32;
    }
    unsafe {
        let mut token: *mut c_void = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elev = TokenElevation {
            token_is_elevated: 0,
        };
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TOKEN_ELEVATION,
            &mut elev as *mut _ as *mut c_void,
            std::mem::size_of::<TokenElevation>() as u32,
            &mut ret,
        );
        CloseHandle(token);
        ok != 0 && elev.token_is_elevated != 0
    }
}
#[cfg(not(windows))]
fn is_elevated() -> bool {
    true
}

/// Версия Windows (через `cmd /c ver`).
fn windows_version() -> String {
    let mut cmd = Command::new("cmd");
    cmd.args(["/c", "ver"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::consts::OS.to_owned())
}

/// Шапка окружения одной пачкой при старте каждого процесса.
fn log_env_header() {
    let ver = env!("CARGO_PKG_VERSION");
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    logging::info("env", format!("Zaprust {ver} ({profile}), elevated={}", is_elevated()));
    logging::info("env", format!("windows: {}", windows_version()));
    if let Ok(exe) = std::env::current_exe() {
        let p = exe.display().to_string();
        logging::info("env", format!("exe: {p}"));
        if p.contains(' ') || !p.is_ascii() {
            logging::warn("env", "путь к exe содержит пробел/не-ASCII — zapret и служба могут не работать");
        }
    }
    match strategies::find_core_dir() {
        Some(core) => {
            let cs = core.display().to_string();
            logging::info("env", format!("core: {cs}"));
            if cs.contains(' ') || !cs.is_ascii() {
                logging::warn("env", "путь к core содержит пробел/не-ASCII");
            }
            let bin = core.join("bin");
            logging::info(
                "env",
                format!(
                    "core: winws.exe={} WinDivert.dll={} WinDivert64.sys={} version={}",
                    bin.join("winws.exe").exists(),
                    bin.join("WinDivert.dll").exists(),
                    bin.join("WinDivert64.sys").exists(),
                    updater::local_version(&core).unwrap_or_else(|| "нет".to_owned())
                ),
            );
        }
        None => logging::warn("env", "ядро не найдено (папка core/)"),
    }
}

// ── Хэндшейк результата между процессами (фикс рассинхрона статуса) ──────────

fn op_result_path() -> std::path::PathBuf {
    std::env::temp_dir().join("zaprust_result.json")
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
    let state = service::query(service::SERVICE_NAME);
    let r = OpResult {
        op: op.to_owned(),
        ok,
        service_state: format!("{state:?}"),
        winws_pid: service::winws_alive(),
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

/// Авторитетная истина «обход работает»: служба RUNNING И процесс winws жив.
fn authoritative_running() -> bool {
    let svc = service::query(service::SERVICE_NAME) == ServiceState::Running;
    let winws = service::winws_alive().is_some();
    if svc != winws {
        logging::warn(
            "state",
            format!("расхождение: служба RUNNING={svc}, winws жив={winws}"),
        );
    }
    svc && winws
}

/// Перезапустить наш exe с правами администратора и дождаться завершения.
/// Возвращает код выхода элевированного процесса.
#[cfg(windows)]
fn run_elevated_self(args: &[&str]) -> Result<i32, String> {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // Параметры: каждый аргумент с пробелом — в кавычках.
    let params: String = args
        .iter()
        .map(|a| {
            if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    let verb = wide(OsStr::new("runas"));
    let file = wide(exe.as_os_str());
    let params_w = wide(OsStr::new(&params));

    #[repr(C)]
    struct ShellExecuteInfoW {
        cb_size: u32,
        f_mask: u32,
        hwnd: *mut c_void,
        lp_verb: *const u16,
        lp_file: *const u16,
        lp_parameters: *const u16,
        lp_directory: *const u16,
        n_show: i32,
        h_inst_app: *mut c_void,
        lp_id_list: *mut c_void,
        lp_class: *const u16,
        hkey_class: *mut c_void,
        dw_hot_key: u32,
        h_icon: *mut c_void,
        h_process: *mut c_void,
    }
    const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
    const SW_HIDE: i32 = 0;
    const INFINITE: u32 = 0xFFFF_FFFF;

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
    }
    extern "system" {
        fn WaitForSingleObject(h: *mut c_void, ms: u32) -> u32;
        fn GetExitCodeProcess(h: *mut c_void, code: *mut u32) -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }

    logging::info("elevate", format!("реинвок (UAC): {}", args.join(" ")));
    unsafe {
        let mut info: ShellExecuteInfoW = std::mem::zeroed();
        info.cb_size = std::mem::size_of::<ShellExecuteInfoW>() as u32;
        info.f_mask = SEE_MASK_NOCLOSEPROCESS;
        info.lp_verb = verb.as_ptr();
        info.lp_file = file.as_ptr();
        info.lp_parameters = params_w.as_ptr();
        info.n_show = SW_HIDE;

        if ShellExecuteExW(&mut info) == 0 {
            let err = GetLastError();
            // ERROR_CANCELLED 1223 = пользователь нажал «Нет» в UAC.
            let msg = if err == 1223 {
                "элевация отклонена пользователем (UAC: Нет)".to_owned()
            } else {
                format!("элевация не удалась (GetLastError={err})")
            };
            logging::error("elevate", &msg);
            return Err(msg);
        }
        if info.h_process.is_null() {
            logging::warn("elevate", "нет hProcess для ожидания результата");
            return Ok(0);
        }
        WaitForSingleObject(info.h_process, INFINITE);
        let mut code: u32 = 0;
        GetExitCodeProcess(info.h_process, &mut code);
        CloseHandle(info.h_process);
        logging::info("elevate", format!("реинвок завершён, код={code}"));
        Ok(code as i32)
    }
}

#[cfg(not(windows))]
fn run_elevated_self(_args: &[&str]) -> Result<i32, String> {
    Err("элевация поддерживается только на Windows".to_owned())
}

// ── Диагностика (D) ──────────────────────────────────────────────────────────

/// Открыть путь в проводнике.
fn open_in_explorer(path: &std::path::Path) {
    let _ = Command::new("explorer").arg(path).spawn();
}

/// Текст диагностики: шапка окружения + последние проблемы из лога.
fn diagnostics_text() -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "Zaprust {} ({})\n",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) { "debug" } else { "release" }
    ));
    s.push_str(&format!("elevated: {}\n", is_elevated()));
    s.push_str(&format!("windows: {}\n", windows_version()));
    if let Ok(exe) = std::env::current_exe() {
        s.push_str(&format!("exe: {}\n", exe.display()));
    }
    if let Some(core) = strategies::find_core_dir() {
        let bin = core.join("bin");
        s.push_str(&format!("core: {}\n", core.display()));
        s.push_str(&format!(
            "core ready: winws={} dll={} sys={} version={}\n",
            bin.join("winws.exe").exists(),
            bin.join("WinDivert.dll").exists(),
            bin.join("WinDivert64.sys").exists(),
            updater::local_version(&core).unwrap_or_else(|| "нет".to_owned())
        ));
    } else {
        s.push_str("core: не найдено\n");
    }
    s.push_str(&format!(
        "service: {:?}, winws_alive: {:?}\n",
        service::query(service::SERVICE_NAME),
        service::winws_alive()
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

/// Положить текст в буфер обмена (CF_UNICODETEXT).
#[cfg(windows)]
fn set_clipboard(text: &str) -> bool {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;
    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;
    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(h: *mut c_void) -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(fmt: u32, mem: *mut c_void) -> *mut c_void;
        fn CloseClipboard() -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalLock(h: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(h: *mut c_void) -> i32;
    }
    let wide: Vec<u16> = OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
        if h.is_null() {
            CloseClipboard();
            return false;
        }
        let p = GlobalLock(h) as *mut u16;
        if !p.is_null() {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            GlobalUnlock(h);
        }
        SetClipboardData(CF_UNICODETEXT, h);
        CloseClipboard();
        true
    }
}

#[cfg(not(windows))]
fn set_clipboard(_text: &str) -> bool {
    false
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
