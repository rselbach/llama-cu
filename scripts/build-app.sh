#!/usr/bin/env bash
# Builds and signs target/release/llama-cu.app, the bundle that owns
# llama-cu's Accessibility and Screen Recording permissions.
#
# Signs with $LLAMA_CU_SIGN_IDENTITY, else the first Developer ID
# Application identity in the keychain, else ad hoc. macOS keeps permissions
# across rebuilds only for a stable identity, so ad hoc builds must be
# granted again after every rebuild.

set -euo pipefail

readonly BUNDLE_ID="com.rselbach.llama-cu"
readonly APP="target/release/llama-cu.app"
readonly ICON="assets/AppIcon.png"
readonly ICONSET="target/release/AppIcon.iconset"

# Prints the signing identity to use, or "-" for ad hoc.
signing_identity() {
  if [[ -n "${LLAMA_CU_SIGN_IDENTITY:-}" ]]; then
    echo "${LLAMA_CU_SIGN_IDENTITY}"
    return
  fi
  local identities
  identities="$(security find-identity -v -p codesigning)"
  local identity
  identity="$(sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' \
    <<<"${identities}" | head -n 1)"
  if [[ -z "${identity}" ]]; then
    echo "warning: no Developer ID identity; signing ad hoc" >&2
    echo "-"
    return
  fi
  echo "${identity}"
}

# Converts the 1024-pixel icon into the sizes macOS shows.
write_icon() {
  rm -rf "${ICONSET}"
  mkdir -p "${ICONSET}" "${APP}/Contents/Resources"
  local size
  for size in 16 32 128 256 512; do
    sips -z "${size}" "${size}" "${ICON}" \
      --out "${ICONSET}/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" "${ICON}" \
      --out "${ICONSET}/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil --convert icns --output "${APP}/Contents/Resources/AppIcon.icns" \
    "${ICONSET}"
}

write_info_plist() {
  local version="$1"
  cat >"${APP}/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleExecutable</key>
  <string>llama-cu</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>CFBundleIdentifier</key>
  <string>${BUNDLE_ID}</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>llama-cu</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${version}</string>
  <key>CFBundleVersion</key>
  <string>${version}</string>
  <key>LSMinimumSystemVersion</key>
  <string>14.0</string>
  <key>LSUIElement</key>
  <true/>
</dict>
</plist>
EOF
}

main() {
  cd "$(dirname "$0")/.."
  cargo build --release

  local version
  version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)"
  rm -rf "${APP}"
  mkdir -p "${APP}/Contents/MacOS"
  cp target/release/llama-cu "${APP}/Contents/MacOS/llama-cu"
  write_icon
  write_info_plist "${version}"

  local identity
  identity="$(signing_identity)"
  local flags=(--force --sign "${identity}")
  if [[ "${identity}" != "-" ]]; then
    # Hardened runtime and a secure timestamp are required for notarization.
    flags+=(--options runtime --timestamp)
  fi
  codesign "${flags[@]}" "${APP}"
  codesign --verify --strict "${APP}"
  echo "built ${APP} (${version}), signed by ${identity}"
}

main "$@"
