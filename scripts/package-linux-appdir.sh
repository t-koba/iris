#!/bin/zsh
set -euo pipefail

script_dir=${0:A:h}
repo_root=${script_dir:h}
binary_name="Iris"
desktop_id="iris"
target_dir="$repo_root/target/package-linux"
appdir="$repo_root/dist/linux/Iris.AppDir"
arch=$(uname -m)
build_variant="${IRIS_BUILD_VARIANT:-product}"

cd "$repo_root"
build_args=(cargo build --release -p iris-ui-shell --bin "$binary_name")
case "$build_variant" in
    product) ;;
    minimal)
        build_args+=(--no-default-features --features minimal)
        ;;
    *)
        echo "unknown IRIS_BUILD_VARIANT: $build_variant" >&2
        exit 1
        ;;
esac
CARGO_TARGET_DIR="$target_dir" "${build_args[@]}"

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

if command -v appimagetool >/dev/null 2>&1; then
    appimagetool "$appdir" "$repo_root/dist/linux/Iris-${arch}.AppImage"
    echo "created $repo_root/dist/linux/Iris-${arch}.AppImage"
else
    echo "created $appdir"
    echo "install appimagetool to wrap the AppDir into an AppImage"
fi
