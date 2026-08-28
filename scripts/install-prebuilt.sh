#!/bin/sh

# Install the prebuilt mathpreview-cli matching this plugin checkout.
#
# Usage:
#   sh scripts/install-prebuilt.sh [version [install-prefix [release-target]]]
#
# With no arguments the version comes from Cargo.toml and the prefix is
# $XDG_DATA_HOME/${NVIM_APPNAME:-nvim}/mathpreview/<version>/<target>
# (falling back to ~/.local/share). The binary lands in
# <prefix>/bin/mathpreview-cli, the same path lua/mathpreview/init.lua resolves
# for install_method="github".

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)

if [ "$#" -gt 3 ]; then
  echo "usage: $0 [version [install-prefix [release-target]]]" >&2
  exit 2
fi

version=${1:-}
if [ -z "$version" ]; then
  version=$(awk -F '"' '/^version = "/ { print $2; exit }' "$repo_dir/Cargo.toml")
fi
version=${version#v}
if [ -z "$version" ]; then
  echo "mathpreview: could not determine the plugin version" >&2
  exit 1
fi
if ! printf '%s\n' "$version" | awk '
  /^[0-9]+\.[0-9]+\.[0-9]+$/ { valid = 1 }
  END { exit(valid ? 0 : 1) }
'; then
  echo "mathpreview: invalid release version '$version' (expected X.Y.Z)" >&2
  exit 1
fi

os=$(uname -s)
arch=$(uname -m)
target=${3:-}
if [ -n "$target" ]; then
  # Internal release-CI override: the macOS x86_64 artifact is built and
  # exercised under Rosetta on an arm64 runner. Normal users never need this.
  case "$target" in
    aarch64-apple-darwin|x86_64-apple-darwin|aarch64-unknown-linux-gnu|x86_64-unknown-linux-gnu) ;;
    *)
      echo "mathpreview: unsupported release target override '$target'" >&2
      exit 1
      ;;
  esac
else
  case "$os:$arch" in
    Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin ;;
    Darwin:x86_64|Darwin:amd64) target=x86_64-apple-darwin ;;
    Linux:arm64|Linux:aarch64) target=aarch64-unknown-linux-gnu ;;
    Linux:x86_64|Linux:amd64) target=x86_64-unknown-linux-gnu ;;
    *)
      echo "mathpreview: no prebuilt binary for $os/$arch; use install_method=\"cargo\"" >&2
      exit 1
      ;;
  esac
fi

# The current Linux release jobs build GNU binaries on Ubuntu 22.04. Reject
# known-incompatible systems before spending time and bandwidth on a download.
# A configured compatibility loader (for example nix-ld) satisfies this check.
if [ "$os" = Linux ]; then
  case "$target" in
    x86_64-unknown-linux-gnu) dynamic_loader=/lib64/ld-linux-x86-64.so.2 ;;
    aarch64-unknown-linux-gnu) dynamic_loader=/lib/ld-linux-aarch64.so.1 ;;
  esac
  if [ ! -e "$dynamic_loader" ]; then
    echo "mathpreview: the GitHub binary needs the standard glibc loader ($dynamic_loader); Alpine/musl and stock NixOS should use install_method=\"cargo\"" >&2
    exit 1
  fi
  if command -v getconf >/dev/null 2>&1; then
    glibc_version=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '$1 == "glibc" { print $2 }') || true
    if [ -n "$glibc_version" ] && ! awk -v version="$glibc_version" '
      BEGIN {
        split(version, part, ".")
        exit(part[1] > 2 || (part[1] == 2 && part[2] >= 34) ? 0 : 1)
      }
    '; then
      echo "mathpreview: the GitHub binary needs glibc 2.34 or newer (found $glibc_version); use install_method=\"cargo\"" >&2
      exit 1
    fi
  fi
fi

if [ "$#" -ge 2 ] && [ -n "$2" ]; then
  install_prefix=$2
else
  data_home=${XDG_DATA_HOME:-"${HOME:?HOME is not set}/.local/share"}
  nvim_appname=${NVIM_APPNAME:-nvim}
  install_prefix="$data_home/$nvim_appname/mathpreview/$version/$target"
fi
install_dir="$install_prefix/bin"

tag="v$version"
archive="mathpreview-cli-$tag-$target.tar.gz"
checksum="$archive.sha256"
release_base=${MATHPREVIEW_RELEASE_BASE_URL:-https://github.com/sonv/TexViewer/releases/download}
release_url="$release_base/$tag"

for tool in curl tar; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "mathpreview: $tool is required to install the GitHub binary" >&2
    exit 1
  fi
done
if command -v shasum >/dev/null 2>&1; then
  checksum_tool=shasum
elif command -v sha256sum >/dev/null 2>&1; then
  checksum_tool=sha256sum
else
  echo "mathpreview: shasum or sha256sum is required to verify the GitHub binary" >&2
  exit 1
fi

mkdir -p "$install_dir"
work_dir=$(mktemp -d "$install_dir/.install.XXXXXX")
cleanup() {
  rm -rf -- "$work_dir"
}
trap cleanup EXIT HUP INT TERM

echo "mathpreview: downloading $archive" >&2
if ! curl --fail --location --silent --show-error \
    --connect-timeout 15 --max-time 300 --retry 2 --retry-delay 1 --retry-max-time 300 \
    --output "$work_dir/$archive" "$release_url/$archive"; then
  echo "mathpreview: release $tag is unavailable for $target; use install_method=\"cargo\" for an unpublished/dev checkout" >&2
  exit 1
fi
if ! curl --fail --location --silent --show-error \
    --connect-timeout 15 --max-time 300 --retry 2 --retry-delay 1 --retry-max-time 300 \
    --output "$work_dir/$checksum" "$release_url/$checksum"; then
  echo "mathpreview: checksum for $archive is unavailable; refusing to install" >&2
  exit 1
fi

checksum_name=$(awk 'NR == 1 { name = $2 } END { if (NR != 1) exit 1; print name }' \
  "$work_dir/$checksum") || {
  echo "mathpreview: malformed checksum file for $archive" >&2
  exit 1
}
if [ "$checksum_name" != "$archive" ]; then
  echo "mathpreview: checksum names '$checksum_name', expected '$archive'" >&2
  exit 1
fi

if [ "$checksum_tool" = shasum ]; then
  (cd "$work_dir" && shasum -a 256 -c "$checksum")
else
  (cd "$work_dir" && sha256sum -c "$checksum")
fi

archive_entries=$(tar -tzf "$work_dir/$archive") || {
  echo "mathpreview: could not inspect $archive" >&2
  exit 1
}
if [ "$archive_entries" != "mathpreview-cli" ]; then
  echo "mathpreview: release archive must contain only mathpreview-cli" >&2
  exit 1
fi
tar -xzf "$work_dir/$archive" -C "$work_dir"
candidate="$work_dir/mathpreview-cli"
if [ ! -f "$candidate" ] || [ -L "$candidate" ]; then
  echo "mathpreview: release archive did not contain a regular mathpreview-cli" >&2
  exit 1
fi
chmod 755 "$candidate"
if [ "$(uname -s)" = Darwin ] && command -v xattr >/dev/null 2>&1; then
  xattr -d com.apple.quarantine "$candidate" >/dev/null 2>&1 || true
fi

version_output="$work_dir/version-output"
"$candidate" --version >"$version_output" 2>&1 &
version_pid=$!
version_wait=10
while kill -0 "$version_pid" 2>/dev/null; do
  if [ "$version_wait" -eq 0 ]; then
    kill -TERM "$version_pid" 2>/dev/null || true
    sleep 1
    kill -KILL "$version_pid" 2>/dev/null || true
    wait "$version_pid" 2>/dev/null || true
    echo "mathpreview: downloaded binary timed out during --version; refusing to install" >&2
    exit 1
  fi
  sleep 1
  version_wait=$((version_wait - 1))
done
if ! wait "$version_pid"; then
  reported_version=$(sed -n '1,8p' "$version_output")
  echo "mathpreview: downloaded binary cannot run on this system:" >&2
  echo "$reported_version" >&2
  echo "mathpreview: use install_method=\"cargo\" to compile locally" >&2
  exit 1
fi
reported_version=$(cat "$version_output")
if [ "$reported_version" != "mathpreview-cli $version" ]; then
  echo "mathpreview: downloaded binary reported '$reported_version', expected 'mathpreview-cli $version'" >&2
  exit 1
fi

# Both paths are inside install_dir, so this promotion is atomic. A failed
# download, verification, extraction, or version check leaves the previous
# executable untouched.
destination="$install_dir/mathpreview-cli"
if [ -L "$destination" ] || { [ -e "$destination" ] && [ ! -f "$destination" ]; }; then
  echo "mathpreview: refusing to replace non-regular destination $destination" >&2
  exit 1
fi
mv -f "$candidate" "$destination"

echo "mathpreview: installed mathpreview-cli $version to $destination" >&2
