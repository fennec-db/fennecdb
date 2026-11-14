#!/usr/bin/env python3
"""deploy/drill/drill.py's judgments held to cases made by hand, each found
failing where it must -- the restore point's bound and how a writer's try
is judged -- with no Docker: `python3 deploy/drill/test_drill.py` (`make
drill` runs it first)."""

import http.server
import os
import socket
import sys
import threading
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import drill  # noqa: E402
from drill import judge_restore  # noqa: E402


def lost(s=-3.6, e=0.9):
    """A try sent before the moment that was never answered."""
    return [s, e, "unknown", "no answer: TimeoutError"]


class RestorePoint(unittest.TestCase):
    def test_exactly_the_rows_acknowledged_pass(self):
        self.assertIsNone(judge_restore("t", (396, 1, 396), 396, []))

    def test_the_row_after_them_passes_where_its_answer_never_came(self):
        # CI run 43627518610: row 397 landed before the moment and its
        # answer never reached the writer.
        self.assertIsNone(judge_restore("t", (397, 1, 397), 396, [lost()]))

    def test_the_row_after_them_passes_after_a_502(self):
        self.assertIsNone(judge_restore("t", (397, 1, 397), 396, [[-4.0, -0.1, "unknown", "502 ..."]]))

    def test_without_it_passes_too_where_it_may_not_have_run(self):
        self.assertIsNone(judge_restore("t", (396, 1, 396), 396, [lost()]))

    def test_the_row_after_them_fails_where_it_was_not_sent(self):
        why = judge_restore("t", (397, 1, 397), 396, [])
        self.assertIn("not 1..396;", why)
        self.assertIn("row 397 was not sent before the moment", why)

    def test_the_row_after_them_fails_where_every_try_was_refused(self):
        # A 503 says nothing landed, and a connection never made sent nothing.
        tries = [[-3.0, -2.9, "fail", "503 lease"], [-2.8, -2.8, "fail", "no connection: ConnectionRefusedError"]]
        why = judge_restore("t", (397, 1, 397), 396, tries)
        self.assertIn("not 1..396;", why)
        self.assertIn("sent -3.00 s, fail -2.90 s (503 lease)", why)

    def test_two_rows_past_fail_whatever_was_tried(self):
        why = judge_restore("t", (398, 1, 398), 396, [lost()])
        self.assertIn("not 1..396 or 1..397;", why)
        self.assertIn("unknown +0.90 s (no answer: TimeoutError)", why)

    def test_fewer_than_acknowledged_fail(self):
        self.assertIsNotNone(judge_restore("t", (395, 1, 395), 396, [lost()]))

    def test_a_hole_fails(self):
        self.assertIsNotNone(judge_restore("t", (396, 2, 397), 396, [lost()]))
        self.assertIsNotNone(judge_restore("t", (396, 1, 397), 396, [lost()]))

    def test_no_row_acknowledged_passes_one_row_where_a_try_may_have_run(self):
        self.assertIsNone(judge_restore("t", (1, 1, 1), 0, [lost()]))

    def test_no_row_acknowledged_fails_one_row_without_such_a_try(self):
        why = judge_restore("t", (1, 1, 1), 0, [])
        self.assertIn("not 1..0;", why)
        self.assertIn("row 1 was not sent before the moment", why)

    def test_several_tries_pass_the_row_after_them_where_one_may_have_run(self):
        tries = [[-3.0, -2.9, "fail", "503 lease"], lost(-2.5, 0.9), [-2.0, -1.9, "fail", "503 lease"]]
        self.assertIsNone(judge_restore("t", (397, 1, 397), 396, tries))

    def test_several_tries_fail_the_row_after_them_where_none_may_have_run(self):
        tries = [[-3.0, -2.9, "fail", "503 lease"], [-2.5, -2.4, "fail", "503 lease"],
                 [-2.0, -2.0, "fail", "no connection: ConnectionRefusedError"]]
        why = judge_restore("t", (397, 1, 397), 396, tries)
        self.assertIn("not 1..396;", why)

    def test_nothing_restored_passes_only_where_nothing_was_acknowledged(self):
        self.assertIsNone(judge_restore("t", (0, None, None), 0, []))
        self.assertIn("holds no row", judge_restore("t", (0, None, None), 3, []))


class Answering(http.server.BaseHTTPRequestHandler):
    """Answers each put with the status its tenant names (`/t/<status>/query`),
    or never, for `/t/silent/query`."""

    def do_POST(self):
        self.rfile.read(int(self.headers["Content-Length"]))
        what = self.path.split("/")[2]
        if what == "silent":
            self.server.release.wait(10)
            return
        self.send_response(int(what))
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *_):
        pass


class Tries(unittest.TestCase):
    """A writer's try judged as chaos.py's clients judge one."""

    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Answering)
        cls.server.release = threading.Event()
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.where = f"127.0.0.1:{cls.server.server_address[1]}"
        cls.addr = drill.addr
        drill.addr = lambda name, port: name

    @classmethod
    def tearDownClass(cls):
        drill.addr = cls.addr
        cls.server.release.set()
        cls.server.shutdown()
        cls.server.server_close()

    def put(self, tenant, where=None):
        w = drill.Writer(tenant, [where or self.where])
        w.TIMEOUT = 0.3
        try:
            return w.put(1)
        finally:
            w.drop()

    def test_a_2xx_is_ok(self):
        self.assertEqual(self.put("200"), ("ok", None))

    def test_a_503_and_a_4xx_did_not_run(self):
        self.assertEqual(self.put("503")[0], "fail")
        self.assertEqual(self.put("409")[0], "fail")

    def test_a_502_and_a_500_may_have_run(self):
        self.assertEqual(self.put("502")[0], "unknown")
        self.assertEqual(self.put("500")[0], "unknown")

    def test_no_answer_may_have_run(self):
        self.assertEqual(self.put("silent"), ("unknown", "no answer: TimeoutError"))

    def test_no_connection_did_not_run(self):
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        closed = f"127.0.0.1:{s.getsockname()[1]}"
        s.close()
        outcome, what = self.put("200", closed)
        self.assertEqual(outcome, "fail")
        self.assertIn("no connection", what)


if __name__ == "__main__":
    unittest.main(verbosity=2)
