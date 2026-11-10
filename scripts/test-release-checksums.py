"""发布清单的行为回归：保留混合 dist，但只发布当前版本的明确产物。"""

from pathlib import Path
import os
import re
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
VERSION = re.search(
    r'^version\s*=\s*"([^"]+)"',
    (ROOT / "Cargo.toml").read_text(encoding="utf-8"), re.M,
).group(1)
BASH = (
    str(Path(os.environ.get("PROGRAMFILES", "C:/Program Files")) / "Git/bin/bash.exe")
    if os.name == "nt" else shutil.which("bash")
)
ASSETS = {
    f"cortex-v{VERSION}-x86_64-pc-windows-msvc.zip",
    f"cortex-desktop-v{VERSION}-x86_64-pc-windows-msvc-setup.exe",
    f"cortex-desktop-v{VERSION}-x86_64-pc-windows-msvc-setup.exe.cdx.json",
}


class ReleaseManifestTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cortex-release-manifest-")
        self.addCleanup(self.temp.cleanup)
        self.dist = Path(self.temp.name)
        for name in ASSETS | {"old-setup.exe", "second-host-test.zip", "keep.sha256"}:
            (self.dist / name).write_text("synthetic fixture", encoding="utf-8")

    def run_manifest(self):
        return subprocess.run(
            [BASH, "scripts/release-checksums.sh", "--dist", str(self.dist), "--version", VERSION],
            cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace",
            timeout=30, check=False,
        )

    def test_only_current_assets_are_listed(self):
        result = self.run_manifest()
        self.assertEqual(result.returncode, 0, result.stderr)
        rows = (self.dist / "SHA256SUMS").read_text().splitlines()
        names = {row.split(maxsplit=1)[1].lstrip(" *") for row in rows}
        self.assertEqual(names, ASSETS, "旧版本和验收包不能进入发布清单")
        self.assertEqual(len(rows), 3, "每个明确产物必须且只能列出一次")

    def test_existing_individual_checksums_are_preserved(self):
        result = self.run_manifest()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.dist / "keep.sha256").read_text(), "synthetic fixture")

    def test_missing_sbom_fails_without_replacing_prior_manifest(self):
        sbom = next(name for name in ASSETS if name.endswith(".cdx.json"))
        (self.dist / sbom).unlink()
        (self.dist / "SHA256SUMS").write_text("previous manifest", encoding="utf-8")
        result = self.run_manifest()
        self.assertNotEqual(result.returncode, 0, "缺少 SBOM 必须阻止发布")
        self.assertEqual((self.dist / "SHA256SUMS").read_text(), "previous manifest")


if __name__ == "__main__":
    unittest.main()
