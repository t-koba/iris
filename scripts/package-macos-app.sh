#!/bin/zsh
set -euo pipefail

script_dir=${0:A:h}
repo_root=${script_dir:h}
binary_name="Iris"
target_dir="$repo_root/target/package-macos"
app_root="$repo_root/dist/macos/Iris.app"
zip_path="$repo_root/dist/macos/Iris-macos.zip"
pkg_path="$repo_root/dist/macos/Iris-macos.pkg"
contents_dir="$app_root/Contents"
macos_dir="$contents_dir/MacOS"
resources_dir="$contents_dir/Resources"
frameworks_dir="$contents_dir/Frameworks"
build_variant="${IRIS_BUILD_VARIANT:-product}"
sign_identity="${IRIS_MACOS_SIGN_IDENTITY:--}"
notary_profile="${IRIS_MACOS_NOTARY_PROFILE:-}"
deployment_target="${IRIS_MACOS_DEPLOYMENT_TARGET:-14.0}"
export MACOSX_DEPLOYMENT_TARGET="$deployment_target"
version=$(sed -n '/^\[workspace.package\]$/,/^\[/{s/^version = "\([^" ]*\)".*/\1/p;}' "$repo_root/Cargo.toml" | head -n 1)

if [[ -z "$version" ]]; then
    echo "missing root Cargo workspace package version" >&2
    exit 1
fi

typeset -A bundled_dylibs

is_system_dependency() {
    local dep="$1"
    case "$dep" in
        /System/*|/usr/lib/*|@executable_path/*|@loader_path/*|@rpath/*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

non_system_dependencies() {
    local binary="$1"
    local dep
    while read -r dep _; do
        [[ -z "$dep" ]] && continue
        if ! is_system_dependency "$dep"; then
            print -r -- "$dep"
        fi
    done < <(otool -L "$binary" | tail -n +2 | awk '{print $1}')
}

bundle_dependency() {
    local src="$1"
    local name="${src:t}"
    local dest="$frameworks_dir/$name"
    local dep
    local deps

    if [[ -z "$src" || -n "${bundled_dylibs[$name]-}" ]]; then
        return
    fi

    cp -fL "$src" "$dest"
    chmod u+w "$dest"
    bundled_dylibs[$name]="$dest"

    install_name_tool -id "@loader_path/$name" "$dest"

    deps=("${(@f)$(non_system_dependencies "$dest")}")
    for dep in "${deps[@]}"; do
        [[ -z "$dep" ]] && continue
        bundle_dependency "$dep"
        install_name_tool -change "$dep" "@loader_path/${dep:t}" "$dest"
    done
}

rewrite_binary_dependencies() {
    local binary="$1"
    local dep
    local deps

    deps=("${(@f)$(non_system_dependencies "$binary")}")
    for dep in "${deps[@]}"; do
        [[ -z "$dep" ]] && continue
        bundle_dependency "$dep"
        install_name_tool -change "$dep" "@executable_path/../Frameworks/${dep:t}" "$binary"
    done
}

assert_no_unbundled_dependencies() {
    local binary="$1"
    local deps

    deps=("${(@f)$(non_system_dependencies "$binary")}")
    deps=("${(@)deps:#}")
    if (( ${#deps[@]} != 0 )); then
        echo "unbundled non-system dependencies remain in $binary:" >&2
        printf '  %s\n' "${deps[@]}" >&2
        exit 1
    fi
}

cd "$repo_root"
build_args=(cargo build --locked --release -p iris-ui-shell --bin "$binary_name")
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

rm -rf "$app_root"
mkdir -p "$macos_dir" "$resources_dir" "$frameworks_dir"

cp -f "$target_dir/release/$binary_name" "$macos_dir/$binary_name"
cp -f "$repo_root/assets/icons/iris-icon.icns" "$resources_dir/Iris.icns"

cat > "$contents_dir/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>Iris</string>
    <key>CFBundleExecutable</key>
    <string>Iris</string>
    <key>CFBundleIconFile</key>
    <string>Iris.icns</string>
    <key>CFBundleIdentifier</key>
    <string>dev.tk.iris</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>Iris</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.productivity</string>
    <key>LSMinimumSystemVersion</key>
    <string>$deployment_target</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>
EOF

chmod +x "$macos_dir/$binary_name"
chmod u+w "$macos_dir/$binary_name"

rewrite_binary_dependencies "$macos_dir/$binary_name"
assert_no_unbundled_dependencies "$macos_dir/$binary_name"
for dylib in "$frameworks_dir"/*.dylib(N); do
    assert_no_unbundled_dependencies "$dylib"
done

codesign_args=(--force --deep --sign "$sign_identity" --timestamp=none)
if [[ "$sign_identity" != "-" ]]; then
    codesign_args+=(--options runtime)
fi
codesign "${codesign_args[@]}" "$app_root"

rm -f "$zip_path"
ditto -c -k --sequesterRsrc --keepParent "$app_root" "$zip_path"

if [[ -n "$notary_profile" ]]; then
    if [[ "$sign_identity" == "-" ]]; then
        echo "IRIS_MACOS_NOTARY_PROFILE requires IRIS_MACOS_SIGN_IDENTITY to use a Developer ID certificate" >&2
        exit 1
    fi

    xcrun notarytool submit "$zip_path" --keychain-profile "$notary_profile" --wait
    xcrun stapler staple "$app_root"
    rm -f "$zip_path"
    ditto -c -k --sequesterRsrc --keepParent "$app_root" "$zip_path"
fi

# Package the final signed (and, when configured, stapled) app without changing it.
# This installer is unsigned: an app-signing identity is not an installer identity,
# and the ZIP notarization above does not notarize the installer.
rm -f "$pkg_path"
pkgbuild --component "$app_root" \
    --install-location /Applications \
    --identifier dev.tk.iris \
    --version "$version" \
    "$pkg_path"

echo "created $app_root"
echo "created $zip_path"
echo "created $pkg_path (unsigned installer; not notarized)"
