"""Package one native release build for download from GitHub Actions."""

import argparse
import json
import os
import plistlib
import shutil
import tarfile
import tempfile
import tomllib
import zipfile
from pathlib import Path

TARGETS = (
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
)


def package(root: Path, target: str) -> Path:
    """Archive a built executable and provenance; fail if required files are absent.

    macOS gets an unsigned app bundle. Tar archives preserve executable permissions
    on Unix; Windows gets a ZIP. Temporary staging is removed even after failures.
    """
    windows = target.endswith("windows-msvc")
    macos = target.endswith("apple-darwin")
    executable = "rowglide.exe" if windows else "rowglide"
    binary = root / "target" / target / "release" / executable
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    version = manifest["workspace"]["package"]["version"]
    name = f"rowglide-{target}"
    output = root / "target" / "ci-artifacts"
    output.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="rowglide-package-") as temporary:
        staging = Path(temporary) / name
        staging.mkdir()
        destination = staging / executable
        if macos:
            contents = staging / "Rowglide.app" / "Contents"
            (contents / "MacOS").mkdir(parents=True)
            destination = contents / "MacOS" / executable
            with (contents / "Info.plist").open("wb") as stream:
                plistlib.dump(
                    {
                        "CFBundleName": "Rowglide",
                        "CFBundleDisplayName": "Rowglide",
                        "CFBundleIdentifier": "dev.rowglide.app",
                        "CFBundleExecutable": "rowglide",
                        "CFBundlePackageType": "APPL",
                        "CFBundleVersion": version,
                        "CFBundleShortVersionString": version,
                        "NSHighResolutionCapable": True,
                    },
                    stream,
                )
        shutil.copy2(binary, destination)
        if not windows:
            destination.chmod(0o755)
        for document in ("LICENSE", "README.md"):
            shutil.copy2(root / document, staging / document)
        (staging / "build-info.json").write_text(
            json.dumps(
                {
                    "version": version,
                    "target": target,
                    "commit": os.environ.get("GITHUB_SHA"),
                    "ref": os.environ.get("GITHUB_REF"),
                    "run_id": os.environ.get("GITHUB_RUN_ID"),
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        archive = output / f"{name}.{'zip' if windows else 'tar.gz'}"
        if windows:
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as stream:
                for path in sorted(staging.rglob("*")):
                    if path.is_file():
                        stream.write(path, path.relative_to(staging.parent))
        else:
            with tarfile.open(archive, "w:gz") as stream:
                stream.add(staging, arcname=name)
    return archive


def main() -> None:
    """Parse the supported target and package its existing release build."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=TARGETS)
    args = parser.parse_args()
    print(package(Path(__file__).resolve().parent.parent, args.target))


if __name__ == "__main__":
    main()
