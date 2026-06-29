#!/usr/bin/env bash
# Strip AppImage-bundled shared libraries that also exist on the host system.
#
# Why this exists: linuxdeploy bundles WebKitGTK, GTK, wayland, glib, etc. into
# the AppImage for portability, but it does NOT bundle Mesa/EGL/GL. The bundled
# WebKitGTK was built against one Mesa ABI; at runtime it links against the
# host's Mesa, and the version skew blows up as `EGL_BAD_ALLOC` from WebKit's
# GPU process (white screen / SIGABRT on Wayland compositors like niri).
#
# Fix: prefer system libs everywhere they exist. We extract the AppImage, move
# every bundled lib that has a same-SONAME equivalent in the host's ldconfig
# cache out of the load path, then repack. The AppImage keeps its bundle as a
# fallback only for libs the host doesn't have.
#
# Usage:
#   scripts/fix-appimage.sh <path-to-appimage>
#
# Replaces the file in place. Requires: curl, file, awk, ldd, ldconfig, and
# network access on first run to fetch appimagetool.

set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <path-to-appimage>" >&2
    exit 64
fi

APPIMAGE="$(readlink -f "$1")"
if [ ! -f "$APPIMAGE" ]; then
    echo "AppImage not found: $1" >&2
    exit 66
fi
if ! file "$APPIMAGE" | grep -q "ELF"; then
    echo "Not an AppImage (ELF magic missing): $APPIMAGE" >&2
    exit 65
fi

WORK="$(mktemp -d)"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

EXTRACT_DIR="$WORK/squashfs-root"
echo "==> Extracting $(basename "$APPIMAGE")"
mkdir -p "$EXTRACT_DIR"
pushd "$WORK" >/dev/null
"$APPIMAGE" --appimage-extract >/dev/null 2>&1 || true
popd >/dev/null

if [ ! -d "$EXTRACT_DIR/usr/lib" ]; then
    echo "Extraction did not produce squashfs-root/usr/lib" >&2
    exit 70
fi

# Locate the main binary inside the bundle. Tauri names it after productName.
BINARY=""
for candidate in "$EXTRACT_DIR"/usr/bin/*; do
    if [ -x "$candidate" ] && file "$candidate" | grep -q "ELF"; then
        BINARY="$candidate"
        break
    fi
done
if [ -z "$BINARY" ]; then
    echo "Could not find main binary under squashfs-root/usr/bin" >&2
    exit 70
fi

echo "==> Stripping bundled libs that have system equivalents"
HIDDEN="$WORK/hidden"
mkdir -p "$HIDDEN"

# Cache ldconfig output once. Looping 200+ times would re-run ldconfig each
# iteration; worse, `ldconfig -p | awk '{exit}'` triggers SIGPIPE on ldconfig
# under `set -o pipefail`, which aborts the script. Read once into a variable.
LDCONFIG_CACHE="$(ldconfig -p)"

moved=0
for f in "$EXTRACT_DIR"/usr/lib/*.so*; do
    [ -e "$f" ] || continue
    # Skip symlinks; we'll reap dangling ones after moving the real files.
    [ -L "$f" ] && continue
    base="$(basename "$f")"
    # ldconfig -p format: "<soname> (libc6,x86-64) => /path"
    sys_path="$(printf '%s\n' "$LDCONFIG_CACHE" \
        | awk -v lib="$base" '$1 == lib { print $NF; exit }' || true)"
    if [ -n "$sys_path" ] && [ -e "$sys_path" ]; then
        mv "$f" "$HIDDEN/"
        moved=$((moved + 1))
    fi
done

# Remove symlinks now dangling (pointing at moved real files).
find "$EXTRACT_DIR/usr/lib" -xtype l -delete 2>/dev/null || true

echo "    moved $moved libs to fallback (host equivalents will be used)"

echo "==> Verifying dependency resolution"
unresolved="$(LDD_TIMEOUT=30 ldd "$BINARY" 2>&1 | grep "not found" || true)"
if [ -n "$unresolved" ]; then
    echo "Stripping left unresolved dependencies:" >&2
    echo "$unresolved" >&2
    echo "Refusing to repack a broken AppImage." >&2
    exit 69
fi

echo "==> Repacking AppImage"
APPIMAGETOOL="$WORK/appimagetool"
if ! command -v appimagetool >/dev/null 2>&1; then
    echo "    downloading appimagetool"
    # --http1.1 avoids sporadic TLS/HTTP2 failures behind some CI proxies.
    if ! curl --retry 3 --http1.1 -fsSL -o "$APPIMAGETOOL" \
            "https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-x86_64.AppImage"; then
        echo "Failed to download appimagetool" >&2
        exit 70
    fi
    chmod +x "$APPIMAGETOOL"
else
    APPIMAGETOOL="$(command -v appimagetool)"
fi

# appimagetool itself ships as an AppImage; on CI runners without FUSE mounted
# we force it to extract-and-run so it doesn't try to mount itself.
export APPIMAGE_EXTRACT_AND_RUN=1
export NO_STRIP=1

OUT_TMP="${APPIMAGE}.fixed"
# Run from WORK so appimagetool's extracted runtime doesn't pollute CWD.
pushd "$WORK" >/dev/null
"$APPIMAGETOOL" "$EXTRACT_DIR" "$OUT_TMP" >/dev/null 2>&1
popd >/dev/null

if [ ! -f "$OUT_TMP" ]; then
    echo "appimagetool did not produce output" >&2
    exit 70
fi

chmod +x "$OUT_TMP"
mv "$OUT_TMP" "$APPIMAGE"
echo "==> Done: $APPIMAGE"
