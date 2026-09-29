#!/usr/bin/env python3
"""Castle release targets; --self-test checks platform and packaging identities."""

import json
import sys

TARGETS = [
    {
        "os": "ubuntu-latest",
        "target": "x86_64-unknown-linux-gnu",
        "targets": "x86_64-unknown-linux-gnu",
        "velopack_runtime": "linux-x64",
        "update_target": "linux-x64",
        "pack_dir": "target/velopack-input",
        "main_exe": "castle-desktop",
        "primary_asset": "castle-desktop-linux-amd64.AppImage",
        "secondary_asset": "castle-desktop-linux-amd64.deb"
    },
    {
        "os": "ubuntu-24.04-arm",
        "target": "aarch64-unknown-linux-gnu",
        "targets": "aarch64-unknown-linux-gnu",
        "velopack_runtime": "linux-arm64",
        "update_target": "linux-arm64",
        "pack_dir": "target/velopack-input",
        "main_exe": "castle-desktop",
        "primary_asset": "castle-desktop-linux-arm64.AppImage",
        "secondary_asset": "castle-desktop-linux-arm64.deb"
    },
    {
        "os": "macos-14",
        "target": "aarch64-apple-darwin",
        "targets": "aarch64-apple-darwin",
        "update_target": "osx-arm64",
        "pack_dir": "target/Castle.app",
        "main_exe": "castle",
        "primary_asset": "castle-desktop-macos-arm64.dmg",
        "secondary_asset": "",
        "velopack_runtime": "osx-arm64",
        "macos_arches": "arm64"
    },
    {
        "os": "macos-14",
        "target": "x86_64-apple-darwin",
        "targets": "x86_64-apple-darwin",
        "update_target": "osx-x64",
        "pack_dir": "target/Castle.app",
        "main_exe": "castle",
        "primary_asset": "castle-desktop-macos-x64.dmg",
        "secondary_asset": "",
        "velopack_runtime": "osx-x64",
        "macos_arches": "x86_64"
    },
    {
        "os": "windows-latest",
        "target": "x86_64-pc-windows-msvc",
        "targets": "x86_64-pc-windows-msvc",
        "velopack_runtime": "win-x64",
        "update_target": "win-x64",
        "pack_dir": "target/velopack-input",
        "main_exe": "castle-desktop.exe",
        "primary_asset": "castle-desktop-windows-x86_64-setup.exe",
        "secondary_asset": ""
    }
]


def release_matrix():
    return {"include": TARGETS}


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        import plistlib
        import re
        from pathlib import Path

        repo = Path(__file__).resolve().parent.parent
        manifest = (repo / "crates/desktop/Cargo.toml").read_text()
        executable = re.search(r'\[\[bin\]\]\s+name = "([^"]+)"', manifest).group(1)
        with (repo / "crates/desktop/resources/Info.plist").open("rb") as source:
            plist = plistlib.load(source)
        assert plist["CFBundleIdentifier"] == "dev.castle.desktop"
        assert plist["CFBundleName"] == "Castle"
        feeds = {target["update_target"] for target in TARGETS}
        assert feeds == {"linux-x64", "linux-arm64", "win-x64", "osx-arm64", "osx-x64"}
        assert len(feeds) == len(TARGETS)
        assert len({target["primary_asset"] for target in TARGETS}) == len(TARGETS)
        for target in TARGETS:
            assert target["velopack_runtime"] == target["update_target"]
            assert target["primary_asset"].startswith("castle-desktop-")
            if target["update_target"].startswith("osx-"):
                assert target["pack_dir"] == "target/Castle.app"
                assert target["main_exe"] == plist["CFBundleExecutable"] == "castle"
            else:
                suffix = ".exe" if target["update_target"] == "win-x64" else ""
                assert target["main_exe"] == executable + suffix
        print("Release matrix checks passed.")
    else:
        print(json.dumps(release_matrix()))
