#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
bundle="$project_dir/target/ssmux-menubar.app"

cargo build --release --locked --manifest-path "$project_dir/Cargo.toml"
"$project_dir/scripts/package-macos-menubar.sh" "$project_dir/target/release/ssmux"

open "$bundle"
