#!/usr/bin/env bash
# Notarizes target/release/llama-cu.app, staples the ticket to it, and
# packages it as target/release/llama-cu-<version>-macos-<arch>.zip.
#
# Run `just app` first with a Developer ID Application identity. notarytool
# signs in with APPLE_ID, APPLE_TEAM_ID, and APPLE_APP_SPECIFIC_PASSWORD.

set -euo pipefail

readonly APP="target/release/llama-cu.app"
readonly SUBMISSION="target/release/llama-cu-notarization.zip"

# Fails unless every named environment variable is set.
require_env() {
  local name
  for name in "$@"; do
    if [[ -z "${!name:-}" ]]; then
      echo "error: ${name} is not set" >&2
      return 1
    fi
  done
}

# Fails unless the notarizing team signed the app. macOS keeps llama-cu's
# permissions across updates only while that signature stays the same.
require_team() {
  local details
  details="$(codesign --display --verbose=2 "${APP}" 2>&1)"
  if ! grep -qxF "TeamIdentifier=${APPLE_TEAM_ID}" <<<"${details}"; then
    echo "error: team ${APPLE_TEAM_ID} did not sign ${APP};" \
      "run just app with its Developer ID Application identity" >&2
    return 1
  fi
}

# Submits the app, waits for Apple, and fails with the notary log unless
# Apple accepts it.
notarize() {
  local credentials=(
    --apple-id "${APPLE_ID}"
    --team-id "${APPLE_TEAM_ID}"
    --password "${APPLE_APP_SPECIFIC_PASSWORD}"
  )
  rm -f "${SUBMISSION}"
  ditto -c -k --keepParent "${APP}" "${SUBMISSION}"
  local result
  result="$(xcrun notarytool submit "${SUBMISSION}" "${credentials[@]}" \
    --wait --output-format json)"
  rm "${SUBMISSION}"
  local status
  status="$(plutil -extract status raw -o - - <<<"${result}")"
  if [[ "${status}" == "Accepted" ]]; then
    return
  fi
  echo "error: notarization finished as ${status}" >&2
  local id
  id="$(plutil -extract id raw -o - - <<<"${result}")"
  xcrun notarytool log "${id}" "${credentials[@]}" >&2
  return 1
}

main() {
  cd "$(dirname "$0")/.."
  require_env APPLE_ID APPLE_TEAM_ID APPLE_APP_SPECIFIC_PASSWORD
  require_team
  notarize
  xcrun stapler staple "${APP}"
  xcrun stapler validate "${APP}"
  spctl --assess --type execute --verbose "${APP}"

  local version
  version="$(plutil -extract CFBundleShortVersionString raw -o - \
    "${APP}/Contents/Info.plist")"
  local arch
  arch="$(lipo -archs "${APP}/Contents/MacOS/llama-cu")"
  local archive="target/release/llama-cu-${version}-macos-${arch}.zip"
  rm -f "${archive}"
  ditto -c -k --keepParent "${APP}" "${archive}"
  echo "notarized ${APP} and packaged ${archive}"
}

main "$@"
