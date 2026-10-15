class Ssmux < Formula
  desc "Menu bar manager and configuration tools for AWS SSM"
  homepage "https://github.com/ycchuang99/ssmux"
  version "__VERSION__"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/ycchuang99/ssmux/releases/download/v#{version}/ssmux-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "__MACOS_ARM64_SHA256__"
    else
      url "https://github.com/ycchuang99/ssmux/releases/download/v#{version}/ssmux-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "__MACOS_X86_64_SHA256__"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/ycchuang99/ssmux/releases/download/v#{version}/ssmux-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "__LINUX_X86_64_SHA256__"
    else
      odie "Linux ARM64 is not supported yet"
    end
  end

  depends_on "awscli"

  def install
    bin.install "ssmux"
  end

  def caveats
    <<~EOS
      ssmux also requires the AWS Session Manager Plugin.

      macOS:
        brew install --cask session-manager-plugin
    EOS
  end

  test do
    assert_match "ssmux", shell_output("#{bin}/ssmux --help")
  end
end
