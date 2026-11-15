#!/usr/bin/env python3
"""tools/flutter_archive_check.py: the plugin as prepare.sh lays it out
passed, and each way 0.2.0's archive shipped a library again -- a link
packed as a copy, another platform's slices, an archive past its size --
refused, with a slice newer than its pod and a Mach-O outside ios/ and
macos/; a Java class file is no Mach-O.

    python3 tools/test_flutter_archive_check.py
"""

import importlib.util
import io
import os
import struct
import tarfile
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("flutter_archive_check", os.path.join(HERE, "flutter_archive_check.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

ARM64, X86_64 = 0x0100000C, 0x01000007
MACOS, IOS, SIM = 1, 2, 7
V11, V12, V15 = 11 << 16, 12 << 16, 15 << 16
PODS = {
    "ios/fennecdb_flutter.podspec": b"Pod::Spec.new do |s|\n  s.platform         = :ios, '15.0'\nend\n",
    "macos/fennecdb_flutter.podspec": b"Pod::Spec.new do |s|\n  s.platform         = :osx, '11.0'\nend\n",
}


def thin(cpu, platform, minos, fill):
    """A 64-bit Mach-O with one LC_BUILD_VERSION, padded past BIG with a
    byte of its own so that no two are alike."""
    cmd = struct.pack("<IIIIII", 0x32, 24, platform, minos, minos, 0)
    header = struct.pack("<IIIIIIII", 0xFEEDFACF, cpu, 0, 6, 1, len(cmd), 0, 0)
    return header + cmd + bytes([fill]) * check.BIG


def fat(*slices):
    """A universal file of the slices, each at a page of its own."""
    out = bytearray(struct.pack(">II", 0xCAFEBABE, len(slices)))
    at = 0x4000
    table, body = [], b""
    for s in slices:
        cpu = struct.unpack_from("<I", s, 4)[0]
        table.append(struct.pack(">IIIII", cpu, 0, at + len(body), len(s), 14))
        body += s + b"\0" * (-len(s) % 0x4000)
    out += b"".join(table)
    out += b"\0" * (at - len(out))
    return bytes(out) + body


DEVICE = thin(ARM64, IOS, V15, 1)
SIM_ARM, SIM_X86 = thin(ARM64, SIM, V15, 2), thin(X86_64, SIM, V15, 3)
MAC_ARM, MAC_X86 = thin(ARM64, MACOS, V11, 4), thin(X86_64, MACOS, V11, 5)
XC = "ios/Frameworks/FennecFFI.xcframework/"


def laid_out():
    """The plugin as prepare.sh lays it out: each platform's own, once."""
    return dict(PODS, **{
        "pubspec.yaml": b"name: fennecdb_flutter\n",
        XC + "Info.plist": b"<plist/>",
        XC + "ios-arm64/FennecFFI.framework/FennecFFI": DEVICE,
        XC + "ios-arm64_x86_64-simulator/FennecFFI.framework/FennecFFI": fat(SIM_X86, SIM_ARM),
        "macos/Libraries/libfennec_ffi.dylib": fat(MAC_X86, MAC_ARM),
        "android/src/main/jniLibs/arm64-v8a/libfennec_ffi.so": b"\x7fELF" + b"\6" * check.BIG,
    })


class Archive(unittest.TestCase):
    def run_check(self, files, links=()):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "fennecdb_flutter.tar.gz")
            with tarfile.open(path, "w:gz") as tar:
                for name, data in files.items():
                    info = tarfile.TarInfo(name)
                    info.size = len(data)
                    tar.addfile(info, io.BytesIO(data))
                for name, target in links:
                    info = tarfile.TarInfo(name)
                    info.type, info.linkname = tarfile.SYMTYPE, target
                    tar.addfile(info)
            return check.check(path)

    def test_the_layout_prepare_sh_makes_passes(self):
        lines, findings = self.run_check(laid_out())
        self.assertEqual(findings, [])
        table = "\n".join(lines)
        self.assertIn("| `macos/Libraries/libfennec_ffi.dylib` |", table)
        self.assertIn("x86_64 macOS 11.0; arm64 macOS 11.0", table)
        self.assertIn("arm64 iOS simulator 15.0", table)

    def test_a_framework_packed_through_its_links_is_refused(self):
        # 0.2.0's macOS framework: Versions/A's binary, and the copies pub
        # made of the two links that reached it.
        files = laid_out()
        fw = "macos/Frameworks/FennecFFI.xcframework/macos-arm64_x86_64/FennecFFI.framework/"
        for name in ("FennecFFI", "Versions/A/FennecFFI", "Versions/Current/FennecFFI"):
            files[fw + name] = fat(MAC_X86, MAC_ARM)
        _, findings = self.run_check(files)
        # Four files of the same bytes: three named copies of the first.
        same = [f for f in findings if "holds the same bytes as" in f]
        self.assertEqual(len(same), 3, findings)
        self.assertTrue(all(f.endswith(" as " + fw + "FennecFFI") for f in same), same)
        self.assertEqual(len(findings), 3, findings)

    def test_another_platforms_slices_are_refused(self):
        files = laid_out()
        files[XC + "macos-arm64/FennecFFI.framework/FennecFFI"] = thin(ARM64, MACOS, V11, 9)
        files["macos/Frameworks/ios-arm64/FennecFFI"] = thin(ARM64, IOS, V15, 10)
        _, findings = self.run_check(files)
        self.assertTrue(any("macos-arm64" in f and "built for macOS; ios/ holds" in f for f in findings), findings)
        self.assertTrue(any("built for iOS; macos/ holds macOS alone" in f for f in findings), findings)

    def test_a_slice_in_two_files_is_refused(self):
        # The device's slice again, inside a universal file of its own.
        files = laid_out()
        files[XC + "ios-arm64_arm64e/FennecFFI.framework/FennecFFI"] = fat(DEVICE, thin(ARM64, IOS, V15, 11))
        _, findings = self.run_check(files)
        self.assertTrue(any("its arm64 slice is in " + XC + "ios-arm64/" in f for f in findings), findings)

    def test_a_slice_newer_than_its_pod_is_refused(self):
        files = laid_out()
        files["macos/Libraries/libfennec_ffi.dylib"] = fat(thin(X86_64, MACOS, V12, 12), MAC_ARM)
        _, findings = self.run_check(files)
        self.assertEqual(len(findings), 1, findings)
        self.assertIn("asks for macOS 12.0, newer than the 11.0 macos/fennecdb_flutter.podspec declares", findings[0])

    def test_a_pod_with_no_platform_is_refused(self):
        files = laid_out()
        files["macos/fennecdb_flutter.podspec"] = b"Pod::Spec.new do |s|\nend\n"
        _, findings = self.run_check(files)
        self.assertTrue(any("macos/fennecdb_flutter.podspec: no single" in f for f in findings), findings)

    def test_a_mach_o_outside_ios_and_macos_is_refused(self):
        files = laid_out()
        files["android/src/main/jniLibs/libfennec_ffi.dylib"] = thin(ARM64, MACOS, V11, 13)
        _, findings = self.run_check(files)
        self.assertEqual(findings, ["android/src/main/jniLibs/libfennec_ffi.dylib: a Mach-O outside ios/ and macos/"])

    def test_a_link_is_refused(self):
        _, findings = self.run_check(laid_out(), links=[("macos/Libraries/Current", "libfennec_ffi.dylib")])
        self.assertTrue(any("a link, which pub never packs" in f for f in findings), findings)

    def test_an_archive_past_its_sizes_is_refused(self):
        packed, unpacked = check.LIMIT_PACKED, check.LIMIT_UNPACKED
        try:
            check.LIMIT_PACKED, check.LIMIT_UNPACKED = 1000, 100_000
            _, findings = self.run_check(laid_out())
        finally:
            check.LIMIT_PACKED, check.LIMIT_UNPACKED = packed, unpacked
        self.assertTrue(any(" packed, past the " in f for f in findings), findings)
        self.assertTrue(any(" unpacked, past the " in f for f in findings), findings)

    def test_a_java_class_file_is_no_mach_o(self):
        # 0xCAFEBABE, then Java 17's version where a fat header counts slices.
        self.assertIsNone(check.slices(struct.pack(">IHH", 0xCAFEBABE, 0, 61) + b"\0" * 64))
        self.assertEqual(len(check.slices(fat(MAC_X86, MAC_ARM))), 2)


class Versions(unittest.TestCase):
    def test_versions_read_as_written(self):
        self.assertEqual(check.version(check.encode("11.0")), "11.0")
        self.assertEqual(check.version(check.encode("10.12.2")), "10.12.2")
        self.assertLess(check.encode("11.0"), check.encode("12"))


if __name__ == "__main__":
    unittest.main()
