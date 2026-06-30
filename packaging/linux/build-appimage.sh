#!/usr/bin/env bash
# Сборка Zaprust в AppImage (x86_64).
#
# Зачем именно так:
#   • Движок (nfqws) В ОБРАЗ НЕ кладём — AppImage read-only, а ядро тянется в
#     XDG-data при первом запуске (L8). Образ остаётся чистым.
#   • Рантайм-зависимости egui (X11/Wayland/GL/libxkbcommon) НЕ бандлим: они есть
#     на любом десктопе, а бандл GL/драйверов часто ломает рендер сильнее, чем
#     помогает. linuxdeploy утянет только то, что реально слинковано (NEEDED:
#     glibc-ядро), системные dlopen-зависимости берутся с хоста.
#   • Элевация из AppImage: pkexec-реинвок указывает на $APPIMAGE (см.
#     elevate.rs::self_exe_path), который рантайм AppImage выставляет сам —
#     поэтому путь к перезапуску корректен и внутри смонтированного образа.
#
# Использование:
#   packaging/linux/build-appimage.sh            # соберёт release и образ
#   SKIP_BUILD=1 packaging/linux/build-appimage.sh   # использовать готовый бинарь
#
# Переменные окружения:
#   LINUXDEPLOY   — путь к linuxdeploy(-x86_64.AppImage); по умолчанию ищется в
#                   PATH, рядом со скриптом и в кеше, иначе скачивается.
#   APPIMAGETOOL  — аналогично для appimagetool.
#   OUTDIR        — куда положить .AppImage (по умолчанию: корень проекта/dist).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_ROOT"

OUTDIR="${OUTDIR:-$PROJECT_ROOT/dist}"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/zaprust-appimage-tools"
APPDIR="$PROJECT_ROOT/target/appimage/AppDir"

# Версия из Cargo.toml — попадёт в имя файла Zaprust-<ver>-x86_64.AppImage.
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
ARCH="x86_64"
export ARCH

# AppImage-тулы сами — AppImage; на свежих дистрибутивах без libfuse2 их надо
# запускать в режиме extract-and-run, иначе они падают на mount.
export APPIMAGE_EXTRACT_AND_RUN=1

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31mОШИБКА:\033[0m %s\n' "$*" >&2; exit 1; }

# --- найти/скачать инструмент -------------------------------------------------
# Печатает путь к инструменту в stdout; все логи идут в stderr, чтобы не попасть
# в подставляемое значение.
fetch_tool() {
  local name="$1" url="$2" override="$3"
  if [ -n "$override" ] && [ -x "$override" ]; then echo "$override"; return; fi
  if command -v "$name" >/dev/null 2>&1; then command -v "$name"; return; fi
  local cached="$CACHE_DIR/$name.AppImage"
  if [ -x "$cached" ]; then echo "$cached"; return; fi
  mkdir -p "$CACHE_DIR"
  log "скачиваю $name…" >&2
  if command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$cached" >&2
  else
    curl -fsSL "$url" -o "$cached" >&2
  fi
  chmod +x "$cached"
  echo "$cached"
}

LINUXDEPLOY="$(fetch_tool linuxdeploy \
  'https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage' \
  "${LINUXDEPLOY:-}")"
APPIMAGETOOL="$(fetch_tool appimagetool \
  'https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage' \
  "${APPIMAGETOOL:-}")"

# linuxdeploy ищет appimagetool в PATH — кладём наш в начало пути.
export PATH="$(dirname "$APPIMAGETOOL"):$PATH"

# --- release-бинарь -----------------------------------------------------------
if [ "${SKIP_BUILD:-0}" != "1" ]; then
  log "cargo build --release"
  cargo build --release
fi
BIN="$PROJECT_ROOT/target/release/zaprust"
[ -x "$BIN" ] || die "не найден release-бинарь: $BIN (собери или SKIP_BUILD=0)"

# --- AppDir -------------------------------------------------------------------
log "готовлю AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" \
         "$APPDIR/usr/share/applications" \
         "$APPDIR/usr/share/icons/hicolor/256x256/apps"

cp "$BIN" "$APPDIR/usr/bin/zaprust"
cp "$SCRIPT_DIR/zaprust.desktop" "$APPDIR/usr/share/applications/zaprust.desktop"
cp "$PROJECT_ROOT/assets/icons/icon-app-256.png" \
   "$APPDIR/usr/share/icons/hicolor/256x256/apps/zaprust.png"

# --- linuxdeploy: дотащить NEEDED-библиотеки и собрать структуру --------------
log "linuxdeploy"
"$LINUXDEPLOY" \
  --appdir "$APPDIR" \
  --executable "$APPDIR/usr/bin/zaprust" \
  --desktop-file "$APPDIR/usr/share/applications/zaprust.desktop" \
  --icon-file "$APPDIR/usr/share/icons/hicolor/256x256/apps/zaprust.png"

# --- appimagetool: запаковать AppDir в .AppImage ------------------------------
mkdir -p "$OUTDIR"
OUTPUT="$OUTDIR/Zaprust-${VERSION}-${ARCH}.AppImage"
log "appimagetool → $OUTPUT"
VERSION="$VERSION" "$APPIMAGETOOL" "$APPDIR" "$OUTPUT"

chmod +x "$OUTPUT"
log "готово: $OUTPUT"
ls -lh "$OUTPUT"
