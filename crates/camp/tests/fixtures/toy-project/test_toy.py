"""Test suite for the toy CLI. Runs the program as a subprocess so it exercises
the real CLI surface. Extend this suite (TDD) when adding new behavior."""
import os
import subprocess
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
TOY = os.path.join(HERE, "toy")


def run(*args):
    return subprocess.run(
        [sys.executable, TOY, *args],
        capture_output=True,
        text=True,
    )


class TestLs(unittest.TestCase):
    def test_ls_lists_items_one_per_line(self):
        r = run("ls")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.splitlines(), ["alpha", "beta", "gamma"])


if __name__ == "__main__":
    unittest.main()
