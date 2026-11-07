#!/bin/zsh
set -euo pipefail

script_dir=${0:A:h}
repo_root=${script_dir:h}
binary_name="Iris"
desktop_id="iris"
target_dir="${CARGO_TARGET_DIR:-$repo_root/target/package-linux}"
appdir="$repo_root/dist/linux/Iris.AppDir"
arch=$(uname -m)
build_variant="${IRIS_BUILD_VARIANT:-product}"

cd "$repo_root"
build_args=(cargo build --locked --offline --release -p iris-ui-shell --bin "$binary_name")
case "$build_variant" in
    product)
        export IRIS_FFMPEG_PREFIX="${IRIS_FFMPEG_PREFIX:-$repo_root/target/linux-ffmpeg}"
        "$script_dir/build-linux-ffmpeg.sh"
        # Only the verified local FFmpeg pkg-config set may satisfy libav imports.
        export PKG_CONFIG_LIBDIR="$IRIS_FFMPEG_PREFIX/lib/pkgconfig"
        export PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
        export FFMPEG_DIR="$IRIS_FFMPEG_PREFIX"
        # Match the Linux CI profile used to satisfy the release binary size budget.
        export CARGO_PROFILE_RELEASE_OPT_LEVEL=s
        # Debian packaging requires dpkg-dev (dpkg-shlibdeps) and binutils.
        for tool in dpkg dpkg-deb dpkg-shlibdeps; do
            command -v "$tool" >/dev/null
        done
        version=$(sed -n '/^\[workspace.package\]$/,/^\[/{s/^version = "\([^" ]*\)".*/\1/p;}' Cargo.toml | head -n 1)
        dpkg --validate-version "$version"
        deb_arch=$(dpkg --print-architecture)
        ;;
    minimal)
        build_args+=(--no-default-features --features minimal)
        ;;
    *)
        echo "unknown IRIS_BUILD_VARIANT: $build_variant" >&2
        exit 1
        ;;
esac
CARGO_TARGET_DIR="$target_dir" "${build_args[@]}"

# This fixed output tree belongs to the packager; never retain stale runtimes.
rm -rf -- "$appdir"
mkdir -p \
    "$appdir/usr/bin" \
    "$appdir/usr/share/applications" \
    "$appdir/usr/share/icons/hicolor/256x256/apps"

cp -f "$target_dir/release/$binary_name" "$appdir/usr/bin/$binary_name"
cp -f "$repo_root/packaging/linux/iris.desktop" "$appdir/$desktop_id.desktop"
cp -f "$repo_root/packaging/linux/iris.desktop" "$appdir/usr/share/applications/$desktop_id.desktop"
cp -f "$repo_root/assets/icons/iris-icon-256.png" "$appdir/$desktop_id.png"
cp -f "$repo_root/assets/icons/iris-icon-256.png" "$appdir/usr/share/icons/hicolor/256x256/apps/$desktop_id.png"

cat > "$appdir/AppRun" <<'EOF'
#!/bin/sh
HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
exec "$HERE/usr/bin/Iris" "$@"
EOF

chmod +x "$appdir/AppRun" "$appdir/usr/bin/$binary_name"

if [[ "$build_variant" = product ]]; then
    "$repo_root/scripts/fetch-pdfium-runtime.sh" "$appdir"
    python3 "$script_dir/linux-ffmpeg.py" stage "$IRIS_FFMPEG_PREFIX" "$appdir"
    max_bytes=${IRIS_MAX_LINUX_RELEASE_BYTES:-35651584}
    actual=$(stat -c %s "$appdir/usr/bin/$binary_name")
    [[ "$actual" -le "$max_bytes" ]]
fi

# Minimal builds still produce an AppDir, but only the default-feature product
# is an installable Debian package. Reuse exactly the binary and desktop assets above.
if [[ "$build_variant" = product ]]; then
    deb_build_dir=$(mktemp -d)
    trap 'rm -rf -- "$deb_build_dir"' EXIT
    trap 'exit 1' HUP INT TERM
    deb_root="$deb_build_dir/iris"
    mkdir -p "$deb_root/DEBIAN" "$deb_build_dir/debian"
    cp -R "$appdir/usr" "$deb_root/usr"

    # dpkg-shlibdeps needs source control metadata, even with -O. Keep all of
    # its working files outside the repository and fail on unresolved libraries
    # or missing package dependency information (no --ignore-missing-info).
    cat > "$deb_build_dir/debian/control" <<EOF
Source: iris

Package: iris
Architecture: any
Description: Cross-platform file manager
EOF
    # The bundled PDFium is loaded at run time, so include its own shared-library
    # closure explicitly; dpkg-shlibdeps cannot discover dlopen() dependencies.
    python3 "$script_dir/linux-ffmpeg.py" shlibs "$deb_root/usr/lib/iris" "$deb_build_dir/debian/shlibs.local"
    # Private libav SONAMEs belong to iris, not Ubuntu's FFmpeg packages.
    # -xiris omits only that self-dependency; every external DSO still needs
    # a valid distro mapping. Explicit -e covers all bundled library closures.
    native_args=()
    for library in "$deb_root"/usr/lib/iris/*.so; do
        native_args+=(-e"$library")
    done
    shlibdeps=$(cd "$deb_build_dir" && dpkg-shlibdeps -O -xiris \
        -l"$deb_root/usr/lib/iris" \
        -e"$deb_root/usr/bin/$binary_name" "${native_args[@]}")
    depends=$(printf '%s\n' "$shlibdeps" | sed -n 's/^shlibs:Depends=//p')
    if [[ -z "$depends" ]]; then
        echo "dpkg-shlibdeps returned no runtime dependencies" >&2
        exit 1
    fi
    printf '%s\n' "$depends" > "$appdir/usr/share/doc/iris/system-dependencies.txt"
    cp "$appdir/usr/share/doc/iris/system-dependencies.txt" "$deb_root/usr/share/doc/iris/"
    cat > "$deb_root/DEBIAN/control" <<EOF
Package: iris
Version: $version
Section: utils
Priority: optional
Architecture: $deb_arch
Maintainer: Iris contributors
Depends: $depends
Description: Cross-platform file manager
 Browse, preview and manage files on local disks, network shares and cloud storage.
EOF
    chmod 0755 "$deb_root" "$deb_root/DEBIAN"
    chmod 0644 "$deb_root/DEBIAN/control"
    deb_name="iris_${version}_${deb_arch}.deb"
    dpkg-deb --build --root-owner-group "$deb_root" "$deb_build_dir/$deb_name"
    mv -f "$deb_build_dir/$deb_name" "$repo_root/dist/linux/$deb_name"
    rm -rf -- "$deb_build_dir"
    trap - EXIT HUP INT TERM
    python3 "$script_dir/linux-ffmpeg.py" check "$appdir" "$repo_root/dist/linux/ffmpeg-audit.json"
    echo "created $repo_root/dist/linux/$deb_name"
fi

if command -v appimagetool >/dev/null 2>&1; then
    appimagetool "$appdir" "$repo_root/dist/linux/Iris-${arch}.AppImage"
    echo "created $repo_root/dist/linux/Iris-${arch}.AppImage"
else
    echo "created $appdir"
    echo "install appimagetool to wrap the AppDir into an AppImage"
fi
