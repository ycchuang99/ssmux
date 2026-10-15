cask "ssmux" do
  version "__VERSION__"

  on_arm do
    url "https://github.com/ycchuang99/ssmux/releases/download/v#{version}/ssmux-menubar-v#{version}-aarch64-apple-darwin.zip"
    sha256 "__MENUBAR_ARM64_SHA256__"
  end

  on_intel do
    url "https://github.com/ycchuang99/ssmux/releases/download/v#{version}/ssmux-menubar-v#{version}-x86_64-apple-darwin.zip"
    sha256 "__MENUBAR_X86_64_SHA256__"
  end

  name "SSMux"
  desc "Menu bar controller for AWS Systems Manager sessions"
  homepage "https://github.com/ycchuang99/ssmux"

  depends_on cask: "session-manager-plugin"
  depends_on formula: "awscli"

  app "SSMux.app"
end
