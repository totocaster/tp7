#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 6 ]]; then
  echo "Usage: $0 <tap-dir> <version> <arm-url> <arm-sha> <intel-url> <intel-sha>" >&2
  exit 1
fi

TAP_DIR="$1"
VERSION="$2"
ARM_URL="$3"
ARM_SHA="$4"
INTEL_URL="$5"
INTEL_SHA="$6"

FORMULA_PATH="${TAP_DIR}/Formula/tp7.rb"
CASK_PATH="${TAP_DIR}/Casks/tp7.rb"
ARM_SUFFIX="aarch64-apple-darwin.tar.gz"
INTEL_SUFFIX="x86_64-apple-darwin.tar.gz"

if [[ "${ARM_URL}" != *"${ARM_SUFFIX}" ]]; then
  echo "Apple Silicon URL does not end with ${ARM_SUFFIX}: ${ARM_URL}" >&2
  exit 1
fi

if [[ "${INTEL_URL}" != *"${INTEL_SUFFIX}" ]]; then
  echo "Intel URL does not end with ${INTEL_SUFFIX}: ${INTEL_URL}" >&2
  exit 1
fi

URL_PREFIX="${ARM_URL%${ARM_SUFFIX}}"
EXPECTED_INTEL_URL="${URL_PREFIX}${INTEL_SUFFIX}"
if [[ "${INTEL_URL}" != "${EXPECTED_INTEL_URL}" ]]; then
  echo "Artifact URLs do not share an architecture-only suffix:" >&2
  echo "  arm:   ${ARM_URL}" >&2
  echo "  intel: ${INTEL_URL}" >&2
  exit 1
fi

rm -f "${FORMULA_PATH}"
mkdir -p "$(dirname "${CASK_PATH}")"

cat >"${CASK_PATH}" <<EOF
cask "tp7" do
  arch arm: "aarch64-apple-darwin", intel: "x86_64-apple-darwin"

  version "${VERSION}"
  sha256 arm:   "${ARM_SHA}",
         intel: "${INTEL_SHA}"

  url "${URL_PREFIX}#{arch}.tar.gz"
  name "TP-7 CLI"
  desc "Command-line file access for Teenage Engineering TP-7 field recorders"
  homepage "https://github.com/totocaster/tp7"

  depends_on cask: "macfuse"

  binary "tp7-#{version}-#{arch}/bin/tp7", target: "tp7"

  caveats <<~EOS
    Finder mounting uses macFUSE. If macOS blocks the system extension, allow
    macFUSE in System Settings -> Privacy & Security, then retry:

      tp7 doctor
      tp7 -a mount
  EOS
end
EOF
