#!/usr/bin/env python3
"""tools/instr's parts that run anywhere: a Mach-O's UUID rewritten in a
copy (unsigned: signing is macOS's), the refusals that keep a build's own
output untouched, the means of medians and two traces' deltas. The
measuring itself is macOS's and wired to `make instr-compare`, not CI."""

import importlib.util
import os
import struct
import sys
import tempfile
import unittest
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import compare  # noqa: E402
import setuuid  # noqa: E402

spec = importlib.util.spec_from_file_location("instr_trace", os.path.join(HERE, "trace.py"))
trace = importlib.util.module_from_spec(spec)
spec.loader.exec_module(trace)


def image(u, filler=b"code"):
    """A thin 64-bit Mach-O: a header, a load command of no interest, then
    `LC_UUID`."""
    other = struct.pack("<II", 0x2, 16) + b"\0" * 8  # LC_SYMTAB's place, cut short
    lc_uuid = struct.pack("<II", setuuid.LC_UUID, 24) + u.bytes
    cmds = other + lc_uuid
    header = struct.pack("<IiiIIIII", setuuid.MH_MAGIC_64, 0x0100000C, 0, 2, 2, len(cmds), 0, 0)
    return header + cmds + filler


def fat(*images):
    """A universal binary holding `images`, each at a 16-byte boundary."""
    head = struct.pack(">II", setuuid.FAT_MAGIC, len(images))
    off = 8 + 20 * len(images)
    entries, body = b"", b""
    for im in images:
        pad = (-(off + len(body))) % 16
        body += b"\0" * pad
        entries += struct.pack(">iiIII", 0x0100000C, 0, off + len(body), len(im), 4)
        body += im
    return head + entries + body


class SetUuid(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.d = self.dir.name

    def tearDown(self):
        self.dir.cleanup()

    def write(self, name, data):
        p = os.path.join(self.d, name)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "wb") as f:
            f.write(data)
        return p

    def test_a_copy_takes_the_uuid_and_the_source_keeps_its_own(self):
        before, want = uuid.uuid4(), uuid.uuid4()
        src = self.write("build/app", image(before))
        dst = os.path.join(self.d, "scratch", "app-1")
        os.makedirs(os.path.dirname(dst))
        self.assertEqual(setuuid.set_uuid(src, dst, want.hex, sign=False), str(want))
        self.assertEqual(setuuid.read_uuids(dst), [str(want)])
        self.assertEqual(setuuid.read_uuids(src), [str(before)])
        with open(src, "rb") as a, open(dst, "rb") as b:
            x, y = a.read(), b.read()
        self.assertEqual(len(x), len(y))
        self.assertEqual(sum(p != q for p, q in zip(x, y)), sum(p != q for p, q in zip(before.bytes, want.bytes)))

    def test_every_slice_of_a_universal_binary_is_set(self):
        src = self.write("build/app", fat(image(uuid.uuid4()), image(uuid.uuid4(), b"other")))
        dst = os.path.join(self.d, "app-1")
        u = setuuid.set_uuid(src, dst, sign=False)
        self.assertEqual(setuuid.read_uuids(dst), [u, u])

    def test_only_a_copy_outside_the_build_is_written(self):
        src = self.write("target/release/examples/app", image(uuid.uuid4()))
        os.makedirs(os.path.join(self.d, "target", "release", ".fingerprint"))
        with self.assertRaisesRegex(ValueError, "only a copy"):
            setuuid.set_uuid(src, src, sign=False)
        with self.assertRaisesRegex(ValueError, "build output"):
            setuuid.set_uuid(src, os.path.join(self.d, "target", "release", "examples", "app-1"), sign=False)
        # Beside the build directory, as target/instr is, is scratch.
        os.makedirs(os.path.join(self.d, "target", "instr"))
        setuuid.set_uuid(src, os.path.join(self.d, "target", "instr", "app-1"), sign=False)
        with self.assertRaisesRegex(ValueError, "exists"):
            setuuid.set_uuid(src, os.path.join(self.d, "target", "instr", "app-1"), sign=False)
        setuuid.set_uuid(src, os.path.join(self.d, "target", "instr", "app-1"), sign=False, force=True)

    def test_what_is_no_mach_o_image_is_refused(self):
        src = self.write("a.sh", b"#!/bin/sh\necho hello\n")
        with self.assertRaisesRegex(ValueError, "Mach-O"):
            setuuid.set_uuid(src, os.path.join(self.d, "b"), sign=False)
        no_uuid = struct.pack("<IiiIIIII", setuuid.MH_MAGIC_64, 0, 0, 2, 1, 16, 0, 0) + struct.pack("<II", 2, 16)
        src = self.write("c", no_uuid + b"\0" * 8)
        with self.assertRaisesRegex(ValueError, "LC_UUID"):
            setuuid.set_uuid(src, os.path.join(self.d, "d"), sign=False)


class Figures(unittest.TestCase):
    def test_a_build_is_the_mean_of_its_identities_medians(self):
        mean, se, lo, hi, meds = compare.mean_of_medians([[10, 11, 30], [20, 21, 22], [29, 30, 31]])
        self.assertEqual(meds, [11, 21, 30])
        self.assertAlmostEqual(mean, 62 / 3)
        self.assertEqual((lo, hi), (11, 30))
        self.assertAlmostEqual(se, 9.5043849 / 3 ** 0.5, places=5)

    def test_against_reads_one_identity_a_side_as_a_plain_pair_would(self):
        ref = compare.mean_of_medians([[100], [110]])
        other = compare.mean_of_medians([[90], [130]])
        d, se, lo, hi = compare.against(ref, other)
        self.assertEqual(d, 5)
        self.assertEqual((lo, hi), (-20, 30))
        self.assertAlmostEqual(se, (5 ** 2 + 20 ** 2) ** 0.5)

    def test_the_report_says_whether_a_difference_is_inside_two_errors(self):
        res = {("put", "base", 0): [100.0, 101.0], ("put", "base", 1): [110.0, 109.0],
               ("put", "head", 0): [104.0, 105.0], ("put", "head", 1): [107.0, 108.0]}
        text = "\n".join(compare.report([("base", None), ("head", None)], ["put"], res, 2, 2))
        self.assertIn("head against base: +1.0 (+0.95%), SE 4.7", text)
        self.assertIn("inside 2 SE", text)
        plain = {("put", "base", 0): [100.0, 102.0, 101.0], ("put", "head", 0): [150.0, 151.0, 99.0]}
        text = "\n".join(compare.report([("base", None), ("head", None)], ["put"], plain, 0, 3))
        self.assertIn("2 of 3 above", text)


class Traces(unittest.TestCase):
    def test_a_diff_keeps_the_programs_own_code_apart_from_the_allocator(self):
        with tempfile.TemporaryDirectory() as d:
            a, b = os.path.join(d, "a.trace"), os.path.join(d, "b.trace")
            with open(a, "w") as f:
                f.write("replays-base\tfennec_http::handle\n" * 10 + "libsystem_malloc.dylib\tmfm_alloc\n" * 7
                        + "libsystem_platform.dylib\t_platform_memmove\n" * 3)
            with open(b, "w") as f:
                f.write("replays-head\tfennec_http::handle\n" * 12 + "libsystem_malloc.dylib\t_xzm_free\n" * 4
                        + "libsystem_platform.dylib\t_platform_memmove\n" * 3)
            out = "\n".join(trace.diff(a, b))
        self.assertRegex(out, r"own\s+10\s+12\s+\+2")
        self.assertRegex(out, r"malloc\s+7\s+4\s+-3")
        self.assertRegex(out, r"system\s+3\s+3\s+\+0")
        # The two builds' modules are named apart, their functions one.
        self.assertIn("+2      10      12  fennec_http::handle", out)
        self.assertNotIn("mfm_alloc", out)

    def test_a_symbol_is_named_as_either_build_names_it(self):
        self.assertEqual(trace.clean("fennec_http::handle::h0123456789abcdef"), "fennec_http::handle")
        self.assertEqual(trace.clean("core::fmt::write (.llvm.123)"), "core::fmt::write")


if __name__ == "__main__":
    unittest.main()
