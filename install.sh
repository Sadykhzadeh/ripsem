#!/bin/sh
# Installs the latest ripsem release on macOS or Linux:
#
#   curl -fsSL https://raw.githubusercontent.com/Sadykhzadeh/ripsem/master/install.sh | sh
#
# The binary goes to ~/.local/bin, or to $RIPSEM_INSTALL_DIR when it is set.
set -eu

repo=Sadykhzadeh/ripsem
case "$(uname -s) $(uname -m)" in
  "Linux x86_64") target=x86_64-unknown-linux-gnu ;;
  "Linux aarch64" | "Linux arm64") target=aarch64-unknown-linux-gnu ;;
  "Darwin arm64") target=aarch64-apple-darwin ;;
  *)
    echo "ripsem has no prebuilt binary for $(uname -s) $(uname -m); see https://github.com/$repo#install" >&2
    exit 1
    ;;
esac
dir=${RIPSEM_INSTALL_DIR:-$HOME/.local/bin}

latest=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest")
tag=${latest##*/}
name=ripsem-$tag-$target
base=https://github.com/$repo/releases/download/$tag

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "downloading ripsem $tag for $target"
curl -fsSL -o "$tmp/$name.tar.gz" "$base/$name.tar.gz"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"

cd "$tmp"
grep " $name.tar.gz\$" SHA256SUMS > expected
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c expected >/dev/null
else
  shasum -a 256 -c expected >/dev/null
fi
tar -xzf "$name.tar.gz"

mkdir -p "$dir"
mv "$name/ripsem" "$dir/ripsem"
chmod 755 "$dir/ripsem"
echo "installed ripsem $tag to $dir/ripsem"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "add $dir to your PATH to run it as ripsem" ;;
esac
