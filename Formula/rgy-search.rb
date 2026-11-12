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
# The 0.1.10.3 release carries both macOS archives, built and smoked on the
# CI runners. The URLs and the digests below name and pin those real files.
# scripts/publish-release.sh rewrites them from the combined SHA256SUMS in
# .dist/, so a formula that names one release and pins another's digest
# cannot be published. scripts/publish-homebrew.sh (.ps1 on Windows) copies
# this file to the danalec/homebrew-rgy tap on release day; the tap is a
# distribution point, not a second source of truth.
class RgySearch < Formula
  desc "Whole-filesystem name search and content search in one stack"
  homepage "https://github.com/danalec/rgy"
  license "Apache-2.0"
  version "0.1.10.3"

  on_arm do
    url "https://github.com/danalec/rgy/releases/download/rgy-v0.1.10.3/rgy-v0.1.10.3-aarch64-apple-darwin.tar.gz"
    sha256 "b8c9c6d2addf0ccacf81c7e3abd7713062cc712f314e53b35df884e0a4fa8dd4"
  end

  on_intel do
    url "https://github.com/danalec/rgy/releases/download/rgy-v0.1.10.3/rgy-v0.1.10.3-x86_64-apple-darwin.tar.gz"
    sha256 "a8c9eee49a25d5a09599a5e6892408350735762d3d90655c31b26765733f3858"
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
