# SPDX-License-Identifier: Apache-2.0
#
# The Homebrew formula for rgy.
#
# The package is called rgy-search because `rgy` is taken on crates.io (a
# GameBoy emulator library); on homebrew the name is free, and one name
# across the registries is worth more than the shorter one. The binaries
# inside keep the names they have everywhere else (searchctl, searchd), the
# project itself is still rgy, and the repository is danalec/rgy.
#
# STAGED: this release does not carry the macOS archives, so the digests
# below are placeholders and the URLs name archives the release does not
# have. The CI builds them on a Mac; it publishes none of them.
# scripts/publish-release.sh rewrites the URLs, the digests and the version
# when it publishes the macOS archives.
class RgySearch < Formula
  desc "Whole-filesystem name search and content search in one stack"
  homepage "https://github.com/danalec/rgy"
  license "Apache-2.0"
  version "0.1.9.0"

  on_arm do
    url "https://github.com/danalec/rgy/releases/download/rgy-v0.1.9.0/rgy-v0.1.9.0-aarch64-apple-darwin.tar.gz"
    sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  end

  on_intel do
    url "https://github.com/danalec/rgy/releases/download/rgy-v0.1.9.0/rgy-v0.1.9.0-x86_64-apple-darwin.tar.gz"
    sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  end

  def install
    bin.install "searchctl", "searchd", "rgy-autostart", "rgxy"
    # The command the project is named after: another name for the one
    # searchctl, and a link rather than a copy.
    bin.install_symlink "rgy" => "searchctl"
  end

  test do
    (testpath / "invoice.txt").write("hello")
    out = shell_output("#{bin}/searchctl --root #{testpath} --index #{testpath}/rgy.idx --count invoice")
    assert_equal "1", out.strip
  end
end
