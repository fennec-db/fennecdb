# Written by tools/homebrew-formula.py in fennec-db/fennecdb from release v0.2.0's
# SHA256SUMS, not by hand: `--check` writes it again from the same release and
# compares, and `--verify` holds its sums to the release's files.
class Fennecdb < Formula
  desc "Embedded database with its own query language, FennecQL"
  homepage "https://fennecdb.com"
  license "Apache-2.0"

  livecheck do
    url :stable
    strategy :github_latest
  end

  on_macos do
    on_arm do
      url "https://github.com/fennec-db/fennecdb/releases/download/v0.2.0/fennec-v0.2.0-aarch64-apple-darwin.tar.gz"
      sha256 "02fd38c82bd8ac62421d32934ee0f1948efd1b4e92ddf1a05a039a2de45e597f"
    end
    on_intel do
      url "https://github.com/fennec-db/fennecdb/releases/download/v0.2.0/fennec-v0.2.0-x86_64-apple-darwin.tar.gz"
      sha256 "556121b63eb25fe1136fea3711005057386dc30250607b8340f79f340c43080a"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/fennec-db/fennecdb/releases/download/v0.2.0/fennec-v0.2.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "f3c9292518e2c7f1aa90964bb6c8a41d00a34d7bda3b005dcc582af98d425b17"
    end
    on_intel do
      url "https://github.com/fennec-db/fennecdb/releases/download/v0.2.0/fennec-v0.2.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "8c29fd6c24b0b106e7116cd8352f22a60267b6774ce03c94ee8dea4a14ad58e3"
    end
  end

  def install
    bin.install "fennec", "fennec-server"
  end

  test do
    # Each binary names its version on stdout, and the shell is run on a
    # script, in memory.
    assert_equal "fennec #{version}", shell_output("#{bin}/fennec --version").chomp
    assert_equal "fennec-server #{version}", shell_output("#{bin}/fennec-server --version").chomp
    script = "create collection t (n int)\nput t {id: 1, n: 7}\nget t\n"
    assert_match(/^1\s+7$/, pipe_output("#{bin}/fennec", script))
  end
end
