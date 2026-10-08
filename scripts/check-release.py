#!/usr/bin/env python3
"""Check tracked source and release payload for credentials/vendor binaries."""
import pathlib
import re
import subprocess

root = pathlib.Path(__file__).resolve().parent.parent
tracked = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
files = [root / name for name in tracked if name]
bundle = root / "dist" / "carlito"
if bundle.exists():
    files.extend(path for path in bundle.rglob("*") if path.is_file())
patterns = [rb"AIza[0-9A-Za-z_-]{35}", rb"gh[pousr]_[0-9A-Za-z]{20,}", rb"sk-[0-9A-Za-z]{20,}"]
errors = []
for path in files:
    if path.suffix == ".env" or path.name == "libqsgepaper.so":
        errors.append(f"Private file included: {path.relative_to(root)}")
    data = path.read_bytes()
    if any(re.search(pattern, data) for pattern in patterns):
        errors.append(f"Credential-shaped value in: {path.relative_to(root)}")
if errors:
    raise SystemExit("\n".join(errors))
print(f"Checked {len(files)} files: no credentials or vendor library included.")
