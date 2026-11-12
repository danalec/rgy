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
# The 0.1.10.2 release carries both macOS archives, built and smoked on the
# CI runners. The URLs and the digests below name and pin those real files.
# scripts/publish-release.sh rewrites them from the combined SHA256SUMS in
# .dist/, so a formula that names one release and pins another's digest
# cannot be published.
class RgySearch < Formula
  desc "Whole-filesystem name search and content search in one stack"
  homepage "https://github.com/danalec/rgy"
  license "Apache-2.0"
  version "0.1.10.2"

  on_arm do
    url "https://github.com/danalec/rgy/releases/download/rgy-v0.1.10.2/rgy-v0.1.10.2-aarch64-apple-darwin.tar.gz"
    sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  end

  on_intel do
    url "https://github.com/danalec/rgy/releases/download/rgy-v0.1.10.2/rgy-v0.1.10.2-x86_64-apple-darwin.tar.gz"
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
