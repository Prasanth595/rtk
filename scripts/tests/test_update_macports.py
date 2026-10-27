import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location(
    "update_macports", Path(__file__).parents[1] / "update-macports.py"
)
updater = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(updater)

PORTFILE = '''PortSystem 1.0
version             1.1.0
revision            3
maintainers         @example openmaintainer
license             {Apache-2 Restrictive}
if {${configure.build_arch} eq "arm64"} {
    distname        rtk-aarch64-apple-darwin
    checksums       rmd160  OLD_ARM_RMD \\
                    sha256  OLD_ARM_SHA \\
                    size    123
} else {
    distname        rtk-x86_64-apple-darwin
    checksums       rmd160  OLD_INTEL_RMD \\
                    sha256  OLD_INTEL_SHA \\
                    size    456
}
destroot {
    xinstall ${worksrcpath}/LICENSE-ENGINE ${destroot}${prefix}/share/doc/rtk
}
'''


class UpdateMacPortsTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name)
        self.portfile = self.directory / "Portfile"
        self.portfile.write_text(PORTFILE)
        self.manifest = []
        for target in updater.TARGETS:
            name = f"rtk-{target}.tar.gz"
            data = f"archive for {target}".encode()
            (self.directory / name).write_bytes(data)
            self.manifest.append(f"{hashlib.sha256(data).hexdigest()}  {name}\n")
        self.write_manifest()

    def write_manifest(self):
        (self.directory / "checksums.txt").write_text("".join(self.manifest))

    def update(self, tag="v1.2.0"):
        return updater.update_portfile(self.portfile, self.directory, tag)

    def test_updates_both_architectures_and_preserves_other_metadata(self):
        self.assertTrue(self.update())
        text = self.portfile.read_text()
        self.assertIn("version             1.2.0", text)
        self.assertIn("revision            0", text)
        self.assertIn("maintainers         @example openmaintainer", text)
        self.assertIn("license             {Apache-2 Restrictive}", text)
        self.assertIn("xinstall ${worksrcpath}/LICENSE-ENGINE", text)
        for target in updater.TARGETS:
            data = (self.directory / f"rtk-{target}.tar.gz").read_bytes()
            self.assertIn(hashlib.sha256(data).hexdigest(), text)
            self.assertIn(hashlib.new("ripemd160", data).hexdigest(), text)
            self.assertIn(f"size    {len(data)}", text)

    def test_rerun_is_a_noop(self):
        self.update()
        before = self.portfile.read_bytes()
        self.assertFalse(self.update())
        self.assertEqual(before, self.portfile.read_bytes())

    def test_same_release_preserves_macports_revision(self):
        self.update()
        self.portfile.write_text(self.portfile.read_text().replace("revision            0", "revision            2"))
        self.assertFalse(self.update())
        self.assertIn("revision            2", self.portfile.read_text())

    def test_same_release_with_changed_assets_is_rejected(self):
        self.update()
        digest = hashlib.sha256((self.directory / f"rtk-{updater.TARGETS[0]}.tar.gz").read_bytes()).hexdigest()
        self.portfile.write_text(self.portfile.read_text().replace(digest, "0" * 64))
        with self.assertRaises(ValueError):
            self.update()

    def test_rejects_downgrade(self):
        with self.assertRaises(ValueError):
            self.update("v1.0.9")
        self.assertEqual(PORTFILE, self.portfile.read_text())

    def test_rejects_prerelease_and_malformed_tags(self):
        for tag in ["v1.2.0-rc.1", "latest", "v01.2.0", "v1.2.0\n", "v1.2.0;echo bad"]:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                self.update(tag)

    def test_checksum_failure_leaves_portfile_untouched(self):
        (self.directory / f"rtk-{updater.TARGETS[1]}.tar.gz").write_bytes(b"tampered")
        with self.assertRaises(ValueError):
            self.update()
        self.assertEqual(PORTFILE, self.portfile.read_text())

    def test_missing_or_duplicate_manifest_entry_is_rejected(self):
        for manifest in [self.manifest[:1], self.manifest + self.manifest[:1]]:
            with self.subTest(manifest=manifest):
                (self.directory / "checksums.txt").write_text("".join(manifest))
                with self.assertRaises(ValueError):
                    self.update()
                self.assertEqual(PORTFILE, self.portfile.read_text())

    def test_changed_port_layout_fails_without_replacing_metadata(self):
        text = PORTFILE.replace("rtk-x86_64-apple-darwin", "unexpected-layout")
        self.portfile.write_text(text)
        with self.assertRaises(ValueError):
            self.update()
        self.assertEqual(text, self.portfile.read_text())


if __name__ == "__main__":
    unittest.main()
