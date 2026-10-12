#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
bundle="$project_dir/target/ssmux-menubar.app"

cargo build --release --locked --manifest-path "$project_dir/Cargo.toml"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp "$project_dir/target/release/ssmux" "$bundle/Contents/MacOS/ssmux"
cp "$project_dir/assets/AppIcon.icns" "$bundle/Contents/Resources/AppIcon.icns"

cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>ssmux</string>
  <key>CFBundleIdentifier</key><string>com.ssmux.menubar</string>
  <key>CFBundleName</key><string>ssmux</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>LSUIElement</key><true/>
  <key>LSEnvironment</key>
  <dict><key>SSMUX_MENUBAR</key><string>1</string></dict>
</dict>
</plist>
PLIST

open "$bundle"
