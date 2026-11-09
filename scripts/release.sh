#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION=""
CREATE_TAG=0
PUSH_TAG=0
SIGN=0

usage() {
  cat <<'EOF'
Usage: scripts/release.sh <version> [--tag] [--push] [--sign]

Creates a local release build, stamps the app version, writes a checksum, and can
optionally create a Git tag and sign the archive.

Examples:
  ./scripts/release.sh 0.1.0
  ./scripts/release.sh v0.1.0 --tag --push
  ./scripts/release.sh 0.1.0 --sign
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --tag)
      CREATE_TAG=1
      ;;
    --push)
      PUSH_TAG=1
      ;;
    --sign)
      SIGN=1
      ;;
    -*)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
    *)
      if [[ -z "$VERSION" ]]; then
        VERSION="$1"
      else
        echo "Too many positional arguments" >&2
        usage >&2
        exit 1
      fi
      ;;
  esac
  shift
done

if [[ -z "$VERSION" ]]; then
  echo "A semantic version is required." >&2
  usage >&2
  exit 1
fi

VERSION="${VERSION#v}"
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "Version must be semver, for example 0.1.0 or 0.1.0-beta.1" >&2
  exit 1
fi

TAG="v${VERSION}"
TARGET_FILE="$ROOT_DIR/Sources/DtopApp/GeneratedVersion.swift"
DIST_DIR="$ROOT_DIR/dist"
mkdir -p "$DIST_DIR"

cat > "$TARGET_FILE" <<EOF
import Foundation

enum BuildVersion {
    static let current = "${VERSION}"
}
EOF

OS_NAME="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$OS_NAME" in
  linux)
    PLATFORM="linux-x86_64"
    ;;
  darwin)
    PLATFORM="macos-arm64"
    ;;
  mingw*|msys*|cygwin*)
    PLATFORM="windows-x86_64"
    ;;
  *)
    echo "Unsupported OS for local packaging: $OS_NAME" >&2
    exit 1
    ;;
 esac

cd "$ROOT_DIR"

echo "==> Building release binary"
if [[ "$PLATFORM" == linux-* ]]; then
  swift build -c release --static-swift-stdlib
else
  swift build -c release
fi

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/dtop-release.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

cp ".build/release/dtop" "$TMP_DIR/dtop"
ARTIFACT_NAME="dtop-${VERSION}-${PLATFORM}.tar.gz"
ARTIFACT_PATH="$DIST_DIR/$ARTIFACT_NAME"
tar -czf "$ARTIFACT_PATH" -C "$TMP_DIR" dtop
sha256sum "$ARTIFACT_PATH" | tee "$DIST_DIR/${ARTIFACT_NAME}.sha256"

if (( SIGN )); then
  echo "==> Signing archive"
  gpg --batch --yes --pinentry-mode loopback --armor --detach-sign "$ARTIFACT_PATH"
fi

if (( CREATE_TAG )); then
  echo "==> Creating tag ${TAG}"
  git tag -a "$TAG" -m "Release $TAG"
fi

if (( PUSH_TAG )); then
  echo "==> Pushing tag ${TAG}"
  git push origin "$TAG"
fi

echo
printf 'Release artifacts:\n  %s\n  %s\n' "$ARTIFACT_PATH" "$DIST_DIR/${ARTIFACT_NAME}.sha256"
if (( SIGN )); then
  printf '  %s.asc\n' "$ARTIFACT_PATH"
fi
printf '\nVersion: %s\nTag: %s\n' "$VERSION" "$TAG"
