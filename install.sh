#!/bin/sh
set -eu

repo="Bradley-Butcher/Changes"
install_dir="${INSTALL_DIR:-${HOME}/.local/bin}"

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) target="aarch64-apple-darwin" ;;
    Darwin-x86_64) target="x86_64-apple-darwin" ;;
    Linux-x86_64) target="x86_64-unknown-linux-gnu" ;;
    *)
        echo "Unsupported platform: $(uname -s) $(uname -m)" >&2
        exit 1
        ;;
esac

for command in curl tar install mktemp; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "Required command not found: $command" >&2
        exit 1
    }
done

archive="changes-${target}.tar.gz"
download_url="https://github.com/${repo}/releases/latest/download"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

cd "$tmp_dir"
curl -fsSL "${download_url}/${archive}" -o "$archive"
curl -fsSL "${download_url}/${archive}.sha256" -o "${archive}.sha256"

if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "${archive}.sha256"
elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "${archive}.sha256"
else
    echo "Required command not found: sha256sum or shasum" >&2
    exit 1
fi

tar -xzf "$archive"
mkdir -p "$install_dir"
install -m 0755 changes "${install_dir}/changes"

echo "Installed changes to ${install_dir}/changes"
case ":${PATH}:" in
    *":${install_dir}:"*) ;;
    *) echo "Add ${install_dir} to PATH to run changes." ;;
esac
