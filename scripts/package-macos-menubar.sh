#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${1:?usage: package-macos-menubar.sh PATH_TO_SSMUX_BINARY}
bundle="$project_dir/target/ssmux-menubar.app"
version=$("$binary" --version | awk 'NR == 1 { print $2 }')

[ -n "$version" ] || { echo "could not read ssmux version from $binary" >&2; exit 1; }

mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp "$binary" "$bundle/Contents/MacOS/ssmux"
cp "$project_dir/assets/AppIcon.icns" "$bundle/Contents/Resources/AppIcon.icns"

cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>ssmux</string>
  <key>CFBundleIdentifier</key><string>com.ssmux.menubar</string>
  <key>CFBundleName</key><string>ssmux</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>LSUIElement</key><true/>
  <key>LSEnvironment</key>
  <dict><key>SSMUX_MENUBAR</key><string>1</string></dict>
</dict>
</plist>
PLIST

plutil -lint "$bundle/Contents/Info.plist"
