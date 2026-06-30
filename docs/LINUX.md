# Zaprust на Linux

Linux-порт **Zaprust** — того же GUI обхода DPI, что и под Windows, но на нативном
для Linux движке. Единая кодовая база: общими остаются интерфейс, выбор стратегии,
оркестрация, автоподбор, конфиг и логи; меняется только платформенный слой.

> Zaprust — **оболочка, не движок.** Трафик обрабатывает демон
> [`nfqws`](https://github.com/bol-van/zapret) (тот же zapret, поверх которого
> построен и виндовый Flowseal). Zaprust поднимает правила фаервола и управляет
> демоном через службу systemd.

## Что во что превращается (Windows → Linux)

| Подсистема | Windows | Linux |
|---|---|---|
| Движок | `winws.exe` | `nfqws` (демон) |
| Перехват пакетов | WinDivert (в аргументах winws) | **NFQUEUE + правила nftables/iptables** (отдельно от демона) |
| Элевация | UAC (`runas`) | **pkexec** (polkit-диалог) |
| Служба | `sc create … start=auto` | **systemd-юнит** + автозапуск |
| Тест доступности | native-tls (SChannel) | rustls |
| Поставка | портабл-zip | **AppImage** |

Ключевое отличие: на Linux движок **не самодостаточен**. `nfqws` сам ничего не
перехватывает — нужны отдельные правила nftables/iptables, заворачивающие исходящий
трафик (tcp 80,443; udp 443 + порты стратегии) в очередь NFQUEUE. Поэтому
**Старт = правила фаервола + запуск демона**, **Стоп = глушим демон + снимаем правила.**

---

## Запуск (AppImage)

```bash
chmod +x Zaprust-*-x86_64.AppImage
./Zaprust-*-x86_64.AppImage
```

1. Если ядра нет — нажмите **«Скачать ядро»**: подтянутся стратегии Flowseal и
   движок `nfqws` (в `~/.local/share/zaprust/core`, в сам AppImage ядро не
   вкладывается — образ read-only).
2. Нажмите **«Старт»**. Появится **диалог polkit** (запрос root) — он нужен, чтобы
   поднять правила фаервола и установить службу systemd. Это один диалог; дальше
   обход поднимается сам при загрузке системы.
3. **Простой режим** сам подберёт рабочую стратегию. В **расширенном режиме** можно
   выбрать стратегию вручную, править списки (⚙), тестировать домены и обновлять ядро.

### Зависимости системы

На любом современном десктопе они уже есть; ставить вручную обычно не нужно:

- **polkit** (`pkexec`) — диалог повышения прав;
- **systemd** — служба обхода и автозапуск;
- **nftables** (`nft`) или **iptables** — правила перехвата (Zaprust сам выбирает
  доступный бэкенд: `nft` → `iptables-nft` → `iptables-legacy` → `iptables`);
- **X11 или Wayland + OpenGL** — для самого GUI (egui грузит их через `dlopen`,
  поэтому в AppImage не вкладываются — берутся с хоста).

Если чего-то не хватает — в дистрибутиве это пакеты вида `polkit`, `systemd`,
`nftables` (или `iptables`).

### FUSE / libfuse2

Классический AppImage монтируется через FUSE. На части свежих дистрибутивов
**libfuse2** не стоит из коробки (там обычно fuse3), и запуск падает с ошибкой про
`libfuse.so.2` / `fusermount`. Два решения:

- поставить совместимость: Ubuntu/Debian — `sudo apt install libfuse2`
  (на 24.04 — `libfuse2t64`); Fedora — `sudo dnf install fuse-libs`; Arch — `fuse2`;
- **или запустить без FUSE** (образ сам распакуется во временную папку):

  ```bash
  ./Zaprust-*-x86_64.AppImage --appimage-extract-and-run
  ```

  Элевация (pkexec-реинвок) корректно работает и в этом режиме — путь к перезапуску
  берётся из `$APPIMAGE`, который рантайм выставляет в обоих случаях.

### Wayland и X11

GUI работает под обоими (egui/winit умеют). На Wayland иконка в доке привязывается
к `.desktop` по `app_id=zaprust` (он же `StartupWMClass`). Если в вашей среде
Wayland-бэкенд капризничает, можно принудительно выбрать X11:
`WINIT_UNIX_BACKEND=x11 ./Zaprust-*.AppImage`.

---

## Логи и диагностика

Все процессы (GUI и элевированный реинвок) пишут в один `zaprust.log` по XDG:

- **`~/.local/state/zaprust/logs/zaprust.log`** (или `$XDG_STATE_HOME/zaprust/logs`);
- если каталог непишем — фолбэк в `/tmp`.

Конфиг: `~/.config/zaprust/config.json` (или `$XDG_CONFIG_HOME/zaprust`).
Ядро: `~/.local/share/zaprust/core` (общий путь GUI и реинвока — кастомный
`$XDG_DATA_HOME` для него намеренно не используется, чтобы обе стороны видели одно).

**Если что-то не работает:** в расширенном режиме —
- **«Открыть папку логов»** → пришлите `zaprust.log`;
- **«Скопировать диагностику»** → краткая сводка (ядро, фаервол-бэкенд, euid,
  путь к nfqws, правила) сразу в буфер обмена.

Из терминала те же данные дают флаги:

```bash
./Zaprust-*.AppImage --diag        # сводка окружения (вкл. $APPIMAGE и цель реинвока)
./Zaprust-*.AppImage --test-net    # проверка доступности доменов (rustls)
./Zaprust-*.AppImage --fetch-core /tmp/core-test   # тест получения ядра, не трогая рабочее
```

### Аварийное восстановление сети

Если процесс с поднятыми правилами убили жёстко (SIGKILL) и трафик «завис»
(остался демон `nfqws` на очереди + правила), снять всё вручную:

```bash
sudo pkill -9 nfqws
sudo nft delete table inet zaprust      # для iptables: sudo iptables -t mangle -F ZAPRUST; sudo iptables -t mangle -X ZAPRUST
```

Правила не персистентны — перезагрузка тоже их снимает.

---

## Сборка из исходников

Нужны Rust (stable) и dev-пакеты системных либ для egui. На Debian/Ubuntu:

```bash
sudo apt install -y libxkbcommon-dev libwayland-dev libxcb1-dev \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libgl1-mesa-dev
```

```bash
cargo build --release      # релиз (opt-z + LTO + strip + panic=abort)
cargo run                  # запустить GUI
cargo test                 # тесты
```

> Кросс-проверка Windows-ветки (без mingw, не линкует):
> `cargo check --target x86_64-pc-windows-gnu`.

### Сборка AppImage

```bash
packaging/linux/build-appimage.sh         # соберёт release и образ в dist/
SKIP_BUILD=1 packaging/linux/build-appimage.sh   # из готового бинаря
```

Скрипт сам скачивает `linuxdeploy` + `appimagetool` (кеш в `~/.cache`), собирает
`AppDir` (бинарь + `.desktop` + иконка) и пакует в `dist/Zaprust-<ver>-x86_64.AppImage`.
Движок (`nfqws`) в образ **не** кладётся — тянется в XDG-data при первом запуске.

### CI

`.github/workflows/release.yml` по тегу `vX.Y.Z` собирает **Linux-AppImage** и
прикладывает его к GitHub-релизу этого репозитория. Сборка идёт на `ubuntu-22.04`
(старый glibc — ради совместимости с более новыми дистрибутивами). Windows
выпускается отдельно, в своём репозитории.

---

## Тестировалось

Критерий готовности порта: на **чистой** Ubuntu и Fedora — скачал `.AppImage` →
`chmod +x` → запустил от пользователя → простой режим Старт → один polkit-диалог →
автоподбор → обход работает → переживает перезагрузку.

> Эффективность обхода зависит от провайдера: на разных DPI пробивают разные
> стратегии (автоподбор перебирает их). Если ничего не подошло — пришлите
> `zaprust.log`.

## Возможные расширения (вне базовой поставки)

Базовая поставка — AppImage. Нативные пакеты (`.deb` / `.rpm` / **Flatpak**) можно
добавить отдельно: для системной установки в фиксированный путь пригодится
полноценный polkit-action (`packaging/linux/dev.zaprust.policy`, см.
[packaging/linux/README.md](../packaging/linux/README.md)) — у AppImage путь
динамический, поэтому там используется дефолтный `org.freedesktop.policykit.exec`.
