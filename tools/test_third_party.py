import unittest

from third_party import diagnostic_outcome


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


if __name__ == "__main__":
    unittest.main()
