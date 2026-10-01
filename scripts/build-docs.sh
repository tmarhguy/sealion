#!/usr/bin/env bash
# Build the static technical manual with Asciidoctor.
# No dependency installation happens here; install Asciidoctor first:
#   macOS:  brew install asciidoctor
#   Linux:  sudo apt-get install -y asciidoctor   (or: gem install asciidoctor)
#   Other:  gem install asciidoctor
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$REPO_ROOT/docs/index.adoc"
OUT_DIR="$REPO_ROOT/build/docs"
OUT_FILE="$OUT_DIR/index.html"

if ! command -v asciidoctor >/dev/null 2>&1; then
  echo "error: asciidoctor not found on PATH." >&2
  echo "Install it first:" >&2
  echo "  macOS:  brew install asciidoctor" >&2
  echo "  Debian/Ubuntu: sudo apt-get install -y asciidoctor" >&2
  echo "  RubyGems (any OS with Ruby): gem install asciidoctor" >&2
  exit 1
fi

if [ ! -f "$SRC" ]; then
  echo "error: missing source $SRC" >&2
  exit 1
fi

mkdir -p "$OUT_DIR/theme" "$OUT_DIR/images"

# Version stamp, RISC-V-spec style ("Version <sha>, <date>" under the
# title). Derived from git at build time; falls back to the build date
# outside a git checkout. Pure Asciidoctor revision attributes.
REVDATE="$(git -C "$REPO_ROOT" log -1 --format=%cs 2>/dev/null || date +%F)"
REVNUM="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unversioned)"

# Single-page manual. docinfo=shared + docinfodir pulls docs/theme/docinfo.html
# (CSS/JS injection) without editing generated HTML afterwards.
asciidoctor \
  --backend html5 \
  --safe-mode safe \
  --attribute docinfo=shared \
  --attribute docinfodir="$REPO_ROOT/docs/theme" \
  --attribute revdate="$REVDATE" \
  --attribute revnumber="$REVNUM" \
  --out-file "$OUT_FILE" \
  "$SRC"

# Theme assets live next to the HTML so relative paths in docinfo.html resolve.
cp "$REPO_ROOT/docs/theme/docs.css" "$OUT_DIR/theme/docs.css"
cp "$REPO_ROOT/docs/theme/nav.js"   "$OUT_DIR/theme/nav.js"
cp "$REPO_ROOT/docs/theme/search.js" "$OUT_DIR/theme/search.js"

# Figures referenced via :imagesdir: images.
if [ -d "$REPO_ROOT/docs/images" ]; then
  cp -R "$REPO_ROOT/docs/images/." "$OUT_DIR/images/"
fi

echo "Built $OUT_FILE"
