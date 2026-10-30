import unittest
from pathlib import Path

from benchlib.config import (
    ConfigError,
    MODULES,
    execution_mode,
    load_scenario,
    module_cases,
    module_requires_export,
    selected_modules,
)


class ConfigTests(unittest.TestCase):
    def test_default_scenario_contains_all_modules(self) -> None:
        scenario = load_scenario(Path(__file__).parents[1] / "scenarios" / "deepseek-harness.toml")
        self.assertEqual(selected_modules("all"), list(MODULES))
        self.assertEqual(len(module_cases(scenario, "e2e")), 3)
        self.assertEqual(module_cases(scenario, "collector")[0]["case_id"], "collector-01")

    def test_module_selection(self) -> None:
        self.assertEqual(selected_modules("collector,memory"), ["collector", "memory"])

    def test_backend_modules_use_native_runner(self) -> None:
        for module in MODULES:
            if module != "e2e":
                self.assertEqual(execution_mode("auto", module), "native")
        self.assertEqual(execution_mode("auto", "e2e"), "auto")
        with self.assertRaises(ConfigError):
            execution_mode("native", "e2e")

    def test_long_running_memory_module_skips_full_export(self) -> None:
        self.assertFalse(module_requires_export("preflight"))
        self.assertFalse(module_requires_export("memory"))
        self.assertTrue(module_requires_export("export"))


if __name__ == "__main__":
    unittest.main()
