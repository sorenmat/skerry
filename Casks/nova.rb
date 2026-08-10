cask "nova" do
  arch arm: "arm64", intel: "x86_64"

  version :latest
  sha256 :no_check

  url "https://github.com/sorenmat/nova/releases/latest/download/Nova-macos-#{arch}.tar.gz"
  name "Nova"
  desc "Dual-frontend text editor for mixed-size workloads"
  homepage "https://github.com/sorenmat/nova"

  depends_on macos: :big_sur

  app "Nova.app"
  binary "#{appdir}/Nova.app/Contents/Resources/nova", target: "nv"

  uninstall quit: "com.smo.nova"
end
