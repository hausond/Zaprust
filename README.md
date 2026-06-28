# Zaprust

**Лёгкий нативный Windows-GUI для обхода DPI-блокировок** (Discord, YouTube, Telegram и др.) поверх движка [zapret](https://github.com/bol-van/zapret) и сборки стратегий [Flowseal/zapret-discord-youtube](https://github.com/Flowseal/zapret-discord-youtube). Написан на **Rust + egui**, один статический бинарь без webview и без сторонних рантаймов.

> Zaprust — **оболочка, не движок**. Трафик обрабатывают `winws.exe` + драйвер `WinDivert` из ядра zapret; Zaprust только управляет ими через службу Windows.

---

## Возможности

- **Простой режим (по умолчанию):** одна кнопка — нажал «Старт», приложение само **подбирает рабочую стратегию** (smart-автоподбор) и ставит её службой на постоянку.
- **Smart-автоподбор:** гоняет каждую стратегию реальным winws, отсеивает провалившие по TLS-хендшейку (fail-fast), среди прошедших выбирает с наименьшим пингом до YouTube и Discord. Один UAC на весь подбор. Запоминает победителя (last-known-good) для мгновенного повторного старта.
- **Расширенный режим:** ручной выбор стратегии (включая виртуальную `smart`), тумблеры Game Filter / IPSet, кнопка «Тест» (проверка доступности доменов), живой лог событий.
- **Служба + автозапуск:** Старт = `sc create … start=auto` + запуск, Стоп = остановка + удаление. После перезагрузки обход поднимается сам.
- **Элевация по требованию:** GUI работает без прав администратора; UAC запрашивается только на привилегированные операции (установка/удаление службы, замена ядра).
- **Апдейтер ядра:** подтягивает свежий релиз Flowseal целиком (bin + стратегии + списки), сохраняя пользовательские `*-user.txt`.
- **Поставка без ядра:** при первом запуске кнопка «Скачать ядро» сама тянет актуальную сборку Flowseal.
- **Редактор списков (⚙):** правка `core/lists/*.txt` прямо в окне, сохранение и откат к стандартному списку из релиза.
- **Логирование и диагностика:** все процессы пишут в один `zaprust.log`; panic-hook ловит крэши; кнопки «Открыть папку логов» и «Скопировать диагностику».

---

## Скриншоты

> _(добавьте сюда скриншоты простого и расширенного режима)_

---

## Архитектура (кратко)

- **service-only модель.** Прямого запуска winws как пользовательского режима нет: Старт всегда устанавливает службу `zapret` со `start=auto` (winws прописан в `binPath` напрямую, инлайн — не через `.bat`), Стоп — удаляет её.
- **Элевация = реинвок самого exe** через `ShellExecuteExW("runas")`. Скрытые служебные режимы (см. ниже) выполняют привилегированную работу в отдельном элевированном процессе и возвращают результат через temp-файл.
- **Главный поток рисует UI.** Сеть, пробы, апдейтер, элевированные операции — в фоновых потоках; обмен с UI только через каналы `mpsc` и atomics.
- **Модули:** `main.rs` (UI + оркестрация + реинвок-команды), `strategies.rs` (парсер `general*.bat`), `service.rs` (управление службой через `sc`), `updater.rs` (GitHub API + zip), `config.rs` (`%APPDATA%\Zaprust\config.json`), `logging.rs` (свой логгер).

Подробно о том, чем итоговая реализация отличается от изначального плана разработки — в [docs/deviations.md](docs/deviations.md).

### Внутренние CLI-режимы (реинвок/диагностика)

| Флаг | Назначение |
|------|------------|
| `--svc <install\|remove\|start\|stop\|uninstall> …` | элевированные операции со службой |
| `--autoselect <gf> [lkg]` | элевированный автоподбор стратегии |
| `--apply-update <zip> <tag> …` | элевированная замена ядра |
| `--write-list <dest> <src>` | элевированная запись файла списка |
| `--dump-args "<стратегия>"` | показать итоговый argv winws |
| `--test-run "<стратегия>"` | поднять winws на 4с и показать его вывод |
| `--test-net` | детект WinDivert + прогон теста доменов |
| `--update-dry` | безопасная проверка апдейтера (во временную папку) |

---

## Сборка из исходников

Проект собирается под **GNU-toolchain** (а не MSVC), чтобы не требовать Visual Studio Build Tools. Линковка статическая — у готового exe нет рантайм-зависимостей от mingw.

### 1. Rust (GNU)

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu
rustup default stable-x86_64-pc-windows-gnu
```

### 2. w64devkit (полный mingw-w64 для линковки)

rustup-овский self-contained mingw неполный (нет ассемблера для `dlltool` и `libgcc_eh.a`). Скачайте портативный **[w64devkit](https://github.com/skeeto/w64devkit/releases)** и распакуйте, например, в `C:\w64devkit`.

### 3. Заглушка `libgcc_eh.a` (важно!)

В GCC 16 (w64devkit) `libgcc_eh` слит в `libgcc.a`, но Rust всё ещё передаёт линкеру `-lgcc_eh`. Создайте **пустой** архив-заглушку (путь зависит от версии GCC):

```powershell
$env:Path = "C:\w64devkit\bin;$env:Path"
$gccver = (gcc -dumpversion)   # напр. 16.1.0
Push-Location "C:\w64devkit\lib\gcc\x86_64-w64-mingw32\$gccver"
ar crs libgcc_eh.a
Pop-Location
```

### 4. Сборка

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\w64devkit\bin;$env:Path"
cd zaprust

cargo build            # debug
cargo run              # запустить
cargo test             # тесты
cargo build --release  # релиз (LTO + opt-z, иконка/манифест вшиваются через build.rs)
```

> `w64devkit\bin` нужен **только на этапе сборки** (линкер + `windres` для ресурсов). Готовый exe запускается на чистой машине без него.

### 5. Упаковка (портабл-zip)

```powershell
$stage = "dist\stage\Zaprust"
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "zaprust\target\release\zaprust.exe" "$stage\"
Copy-Item "zaprust\PACKAGE_README.txt" "$stage\README.txt"
Compress-Archive "$stage" "dist\Zaprust-portable.zip" -Force
```

Ядро в zip можно не вкладывать — приложение скачает его кнопкой «Скачать ядро».

---

## Структура репозитория

```
.
├── README.md                 ← этот файл
├── .gitignore  .gitattributes
├── docs/
│   ├── deviations.md         ← отступления от изначального плана
│   └── icons-design-prompt.md← ТЗ на иконки для дизайн-агента
└── zaprust/                  ← Rust-крейт
    ├── Cargo.toml  Cargo.lock  build.rs
    ├── PACKAGE_README.txt     ← README, который кладётся в портабл-zip
    ├── assets/icons/          ← иконки интерфейса + логотип/ico
    └── src/
        ├── main.rs            ← UI, оркестрация, реинвок-команды
        ├── strategies.rs      ← поиск ядра + парсер стратегий
        ├── service.rs         ← служба Windows (sc/net/netsh)
        ├── updater.rs         ← апдейтер ядра (GitHub API + zip)
        ├── config.rs          ← персист настроек
        └── logging.rs         ← логгер + panic-hook
```

`zaprust/core/` (ядро zapret) и `zaprust/target/`, `dist/`, `logs/` — в репозиторий **не** попадают (см. `.gitignore`).

---

## Использование (для пользователя)

1. Распакуйте в путь **без пробелов, кириллицы и спецсимволов** (напр. `C:\Zaprust\`).
2. Запустите `zaprust.exe`. При отсутствии ядра — «Скачать ядро».
3. «Старт» → один UAC → обход поднимается и переживает перезагрузку.

Подробная пользовательская инструкция — в [zaprust/PACKAGE_README.txt](zaprust/PACKAGE_README.txt).

### Если что-то не работает

- Расширенный режим → **«Логи»** (откроется папка с `zaprust.log`) или **«Диагностика»** (сводка в буфер обмена).
- Логи лежат в `logs/` рядом с exe (или `%LOCALAPPDATA%\Zaprust\logs\`, если папка рядом непишема).

### Заметки

- **SmartScreen** на неподписанном exe → «Подробнее» → «Выполнить в любом случае».
- **Антивирус** может ругаться на WinDivert (PUA/RiskTool) — это ложное срабатывание, добавьте папку в исключения. Для раздачи собирайте **release** (debug ловится агрессивнее).
- Права администратора нужны для драйвера и службы — это инвариант Windows.

---

## Благодарности и дисклеймер

- [**bol-van/zapret**](https://github.com/bol-van/zapret) — движок обхода DPI.
- [**Flowseal/zapret-discord-youtube**](https://github.com/Flowseal/zapret-discord-youtube) — подобранные стратегии, списки и сборка под Windows.

Zaprust — независимый сторонний GUI и **не аффилирован** с bol-van или Flowseal.

## Лицензия

Лицензия не выбрана. Перед публикацией добавьте файл `LICENSE` (например, MIT) — учтите лицензии используемых зависимостей и ядра zapret.
