import unittest

from benchlib.metrics import linear_slope, memory_summary


class MetricsTests(unittest.TestCase):
    def test_linear_slope(self) -> None:
        self.assertAlmostEqual(linear_slope([(0, 0), (1, 2), (2, 4)]), 2.0)

    def test_stable_tail(self) -> None:
        samples = [
            {"elapsed_seconds": float(index), "total_rss_bytes": 1000 + index}
            for index in range(20)
        ]
        summary = memory_summary(samples, 0.25, 0.05)
        self.assertTrue(summary["stable"])

    def test_growing_tail(self) -> None:
        samples = [
            {"elapsed_seconds": float(index), "total_rss_bytes": 1000 + index * 100}
            for index in range(20)
        ]
        summary = memory_summary(samples, 0.25, 0.05)
        self.assertFalse(summary["stable"])


if __name__ == "__main__":
    unittest.main()
