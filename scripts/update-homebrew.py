#!/usr/bin/env python3
import argparse
import hashlib
import re
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
TEMPLATES = ROOT / "packaging" / "homebrew"
ASSETS = {
    "__MACOS_ARM64_SHA256__": "ssmux-v{version}-aarch64-apple-darwin.tar.gz",
    "__MACOS_X86_64_SHA256__": "ssmux-v{version}-x86_64-apple-darwin.tar.gz",
    "__LINUX_X86_64_SHA256__": "ssmux-v{version}-x86_64-unknown-linux-gnu.tar.gz",
    "__MENUBAR_ARM64_SHA256__": "ssmux-menubar-v{version}-aarch64-apple-darwin.zip",
    "__MENUBAR_X86_64_SHA256__": "ssmux-menubar-v{version}-x86_64-apple-darwin.zip",
}


def checksum(path):
    digest = hashlib.sha256()
    with path.open("rb") as asset:
        for chunk in iter(lambda: asset.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def render(template, substitutions):
    for token, value in substitutions.items():
        count = template.count(token)
        if count:
            if count != 1:
                raise ValueError(f"expected one {token} placeholder in Homebrew template")
            template = template.replace(token, value)
    if re.search(r"__[A-Z0-9_]+__", template):
        raise ValueError("unresolved placeholder in Homebrew template")
    return template


def update(version, assets_dir, tap_dir):
    if not re.fullmatch(r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)", version):
        raise ValueError(f"invalid stable version: {version}")
    substitutions = {"__VERSION__": version}
    for placeholder, filename in ASSETS.items():
        path = assets_dir / filename.format(version=version)
        if not path.is_file():
            raise FileNotFoundError(f"missing release asset: {path}")
        substitutions[placeholder] = checksum(path)

    for template_path, relative_path in (
        (TEMPLATES / "Formula" / "ssmux.rb", Path("Formula") / "ssmux.rb"),
        (TEMPLATES / "Casks" / "ssmux.rb", Path("Casks") / "ssmux.rb"),
    ):
        destination = tap_dir / relative_path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(render(template_path.read_text(), substitutions))


def self_test():
    assert render("v=__VERSION__ sha=__SHA__", {"__VERSION__": "1.2.3", "__SHA__": "abc"}) == "v=1.2.3 sha=abc"
    assert render("v=__VERSION__", {"__VERSION__": "1.2.3", "__SHA__": "abc"}) == "v=1.2.3"
    with tempfile.TemporaryDirectory() as directory:
        asset = Path(directory) / "asset"
        asset.write_bytes(b"ssmux")
        assert checksum(asset) == hashlib.sha256(b"ssmux").hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--version")
    parser.add_argument("--assets-dir", type=Path)
    parser.add_argument("--tap-dir", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        self_test()
    elif args.version and args.assets_dir and args.tap_dir:
        update(args.version, args.assets_dir, args.tap_dir)
    else:
        parser.error("provide --version, --assets-dir and --tap-dir, or use --self-test")


if __name__ == "__main__":
    main()
