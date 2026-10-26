import sys
import tempfile
import unittest
from pathlib import Path

from run import to_log
from third_party import diagnostic_outcome, log_text, metric_records


class DiagnosticOutcomeTests(unittest.TestCase):
    def test_non_macos_schedules_mutation_after_ordinary_failure(self):
        outcome = diagnostic_outcome("linux", 1, False, 10)
        self.assertTrue(outcome["mutation_scheduled"])
        self.assertFalse(outcome["ordinary_passed"])
        self.assertFalse(outcome["passed"])

    def test_either_suite_failure_fails_combined_outcome(self):
        self.assertFalse(diagnostic_outcome("linux", 1, False, 10, True)["passed"])
        self.assertFalse(diagnostic_outcome("linux", 0, False, 10, False)["passed"])
        self.assertTrue(diagnostic_outcome("linux", 0, False, 10, True)["passed"])

    def test_timeout_and_missing_metrics_fail_ordinary_suite(self):
        outcome = diagnostic_outcome("linux", 0, True, 9, True)
        self.assertFalse(outcome["ordinary_passed"])
        self.assertIn("semantic test timed out", outcome["ordinary_failures"])
        self.assertIn("semantic test produced 9 METRIC records; need 10",
                      outcome["ordinary_failures"])

    def test_macos_skips_mutation(self):
        outcome = diagnostic_outcome("darwin", 0, False, 10)
        self.assertFalse(outcome["mutation_scheduled"])
        self.assertIsNone(outcome["mutation_passed"])
        self.assertTrue(outcome["passed"])


class LogParsingTests(unittest.TestCase):
    def test_metrics_and_marker_survive_output_past_the_tail(self):
        # Ten cases report early; a later panic backtrace pushes them out of the tail.
        script = ("import json\n"
                  "for case in range(10): print('METRIC ' + json.dumps({'case': str(case)}))\n"
                  "print('editor-to-processor sound assertion failed')\n"
                  "print('x' * 200000)\n")
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "checks.log"
            result = to_log([sys.executable, "-c", script], log, cwd=directory,
                            timeout=60, tail_bytes=131072)
            self.assertNotIn("METRIC ", result.tail)
            text = log_text(log)
            self.assertEqual(len(metric_records(text)), 10)
            self.assertIn("editor-to-processor sound assertion failed", text)


if __name__ == "__main__":
    unittest.main()
