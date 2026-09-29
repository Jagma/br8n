#!/bin/sh
set -eu

version="${1:?usage: scripts/homebrew-formula.sh <version>, once v<version> is released}"
release="https://github.com/Jagma/br8n/releases/download/v$version"

sha256_of() {
    sum="$(curl -fsSL "$release/br8n-$1.sha256")"
    sum="${sum%% *}"
    case "$sum" in
        *[!0-9a-f]* | "") echo "no checksum for br8n-$1 in v$version" >&2; exit 1 ;;
    esac
    if [ "${#sum}" -ne 64 ]; then
        echo "no checksum for br8n-$1 in v$version" >&2
        exit 1
    fi
    echo "$sum"
}
mac="$(sha256_of aarch64-apple-darwin)"
linux="$(sha256_of x86_64-unknown-linux-gnu)"

cat <<FORMULA
class Br8n < Formula
  desc "Local semantic search over your notes and past sessions for coding agents"
  homepage "https://github.com/Jagma/br8n"
  version "$version"
  license "AGPL-3.0-only"

  on_macos do
    depends_on arch: :arm64
    url "$release/br8n-aarch64-apple-darwin.tar.gz"
    sha256 "$mac"
  end

  on_linux do
    depends_on arch: :x86_64
    url "$release/br8n-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "$linux"
  end

  def install
    bin.install "br8n"
  end

  def caveats
    <<~EOS
      Finish setting br8n up. This registers its Claude Code plugin, offers to
      connect your other agents and downloads the embedding model:
        br8n install

      br8n needs Ollama running: https://ollama.com
      To update, run br8n update: it runs brew upgrade br8n, then br8n install.
    EOS
  end

  test do
    assert_equal "br8n #{version}", shell_output("#{bin}/br8n --version").strip
  end
end
FORMULA
