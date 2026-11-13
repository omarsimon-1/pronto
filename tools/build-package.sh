#!/usr/bin/env bash
# Build Pronto from committed HEAD for this host (x86_64 or aarch64).
# tools/package-source.py creates the source archive and packaging/PKGBUILD.in recipe.
# This script prints the install command; it never runs sudo or installs dependencies.
# Usage: tools/build-package.sh [extra makepkg options, e.g. --nodeps with rustup]
# Qt environment variables are cleared and the package must not link Qt.
set -euo pipefail

if [[ $EUID -eq 0 ]]; then
  echo "Run this as your own user, not root: it never needs sudo." >&2
  exit 1
fi
# makepkg's -s, -i and -r (alone or bundled, as -si) and their long forms call sudo.
for option in "$@"; do
  case $option in
    --syncdeps | --install | --rmdeps | -[!-]*[sir]* | -[sir]*)
      echo "Refusing makepkg option '$option': it would run sudo." >&2
      exit 1
      ;;
  esac
done

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

host_arch=$(uname -m)
case $host_arch in
  x86_64 | aarch64) ;;
  *) echo "Unsupported package architecture: $host_arch" >&2; exit 1 ;;
esac
if [[ -n $(git status --porcelain --untracked-files=no) ]]; then
  echo "warning: uncommitted changes aren't packaged; only HEAD ($(git rev-parse --short HEAD)) is" >&2
fi

# Keep Qt out of the build's environment.
unset QTDIR QT_SELECT QT_PLUGIN_PATH QT_QPA_PLATFORM QT_QPA_PLATFORMTHEME QML2_IMPORT_PATH \
  QML_IMPORT_PATH QMAKE QMAKESPEC CMAKE_PREFIX_PATH

python3 tools/package-source.py

package_dir=$root/.local/package
sed -i "s/^arch=.*/arch=('$host_arch')/" "$package_dir/PKGBUILD"
grep -q "^arch=('$host_arch')$" "$package_dir/PKGBUILD" || {
  echo "the PKGBUILD's arch couldn't be set to $host_arch" >&2
  exit 1
}

cd "$package_dir"
makepkg --cleanbuild --force --nocheck "$@"

package=$(makepkg --packagelist | grep -E "/pronto-[^/]*-${host_arch}\.pkg\.tar\.zst$" | head -n1)
if [[ -z $package || ! -f $package ]]; then
  echo "makepkg built no pronto $host_arch package" >&2
  exit 1
fi

# The package's binary must not need Qt.
check=$(mktemp -d)
trap 'rm -rf "$check"' EXIT
bsdtar -xf "$package" -C "$check" usr/bin/pronto
if readelf -d "$check/usr/bin/pronto" | grep -E 'NEEDED.*libQt'; then
  echo "the built binary needs Qt; not installing it" >&2
  exit 1
fi

echo
echo "Built $package"
echo "To install or update it, run:"
echo
echo "    sudo pacman -U $package"
