#!/usr/bin/env bash
# Points the llama-cu cask in rselbach/homebrew-tap at a released zip.
#
# Usage: scripts/update-homebrew-cask.sh VERSION ZIP
#
# GH_TOKEN must be allowed to push to rselbach/homebrew-tap. The release
# workflow runs this after it publishes the zip.

set -euo pipefail

readonly TAP="rselbach/homebrew-tap"
readonly CASK="Casks/llama-cu.rb"

checkout=""

# Removes the tap checkout.
cleanup() {
  if [[ -n "${checkout}" ]]; then
    rm -rf "${checkout}"
  fi
}

main() {
  if (( $# != 2 )); then
    echo "usage: $0 VERSION ZIP" >&2
    return 2
  fi
  local version="$1"
  local zip="$2"
  if [[ ! "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "error: invalid version ${version}" >&2
    return 1
  fi
  if [[ -z "${GH_TOKEN:-}" ]]; then
    echo "error: GH_TOKEN is not set" >&2
    return 1
  fi
  local sums
  sums="$(shasum -a 256 "${zip}")"
  local sha256="${sums%% *}"

  checkout="$(mktemp -d)"
  trap cleanup EXIT
  gh auth setup-git
  gh repo clone "${TAP}" "${checkout}" -- --depth=1

  local cask="${checkout}/${CASK}"
  sed -i.orig \
    -e "s/^  version \".*\"$/  version \"${version}\"/" \
    -e "s/^  sha256 \".*\"$/  sha256 \"${sha256}\"/" \
    "${cask}"
  rm "${cask}.orig"
  # The edit is silent when the cask stops matching these lines.
  if ! grep -qxF "  version \"${version}\"" "${cask}" \
    || ! grep -qxF "  sha256 \"${sha256}\"" "${cask}"; then
    echo "error: could not set version and sha256 in ${CASK}" >&2
    return 1
  fi
  if git -C "${checkout}" diff --quiet; then
    echo "${CASK} is already at ${version}"
    return
  fi

  git -C "${checkout}" config user.name "github-actions[bot]"
  git -C "${checkout}" config user.email \
    "41898282+github-actions[bot]@users.noreply.github.com"
  git -C "${checkout}" commit --quiet --all \
    --message "Update llama-cu to ${version}"
  git -C "${checkout}" push --quiet
  echo "updated ${CASK} to ${version}"
}

main "$@"
