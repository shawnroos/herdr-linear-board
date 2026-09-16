"""`scripts/open-board.sh`: the open-or-focus toggle's pane matcher.

The matcher is an allowlist whose failure is silent: a board pane whose title
it does not recognise makes every press open another overlay. These tests run
the real script against a stub `herdr` that serves a canned `pane list` and
logs every other call, so the decision is read from what the script executed.
"""
from __future__ import annotations

import json
import subprocess
import tempfile
import unittest
from pathlib import Path

from _support import PYTHON_SHEBANG, REPO_ROOT, clean_env, write_executable

SCRIPT = REPO_ROOT / "scripts" / "open-board.sh"

STUB = """
import json, os, sys
args = sys.argv[1:]
if args[:2] == ["pane", "list"]:
    sys.stdout.write(os.environ["STUB_PANES"])
    sys.exit(0)
with open(os.environ["STUB_LOG"], "a", encoding="utf-8") as log:
    log.write(json.dumps(args) + "\\n")
"""


def pane(pane_id: str, label: str, focused: bool = False) -> dict:
    return {"pane_id": pane_id, "label": label, "focused": focused}


class LauncherMatcherTests(unittest.TestCase):
    def decide(self, panes: list[dict]) -> list[str]:
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            herdr = write_executable(directory, "herdr", STUB, shebang=PYTHON_SHEBANG)
            log = directory / "calls.log"
            env = clean_env(
                HERDR_BIN_PATH=str(herdr),
                STUB_PANES=json.dumps({"result": {"panes": panes}}),
                STUB_LOG=str(log),
            )
            result = subprocess.run(
                ["bash", str(SCRIPT)],
                env=env,
                text=True,
                capture_output=True,
                check=False,
                timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual(len(calls), 1, calls)
        return calls[0]

    def test_an_unfocused_linear_title_is_focused(self) -> None:
        call = self.decide([pane("w1:p3", "Linear: Example Launch")])
        self.assertEqual(call, ["plugin", "pane", "focus", "w1:p3"])

    def test_a_focused_linear_title_is_closed(self) -> None:
        call = self.decide([pane("w1:p3", "Linear: Example Launch", focused=True)])
        self.assertEqual(call, ["pane", "close", "w1:p3"])

    def test_an_unrelated_title_opens_a_new_overlay(self) -> None:
        for label in ("zsh", "Linear", "Linearize: x", "My Linear: x", "linear: x"):
            with self.subTest(label=label):
                call = self.decide([pane("w1:p3", label)])
                self.assertEqual(call[:3], ["plugin", "pane", "open"])

    def test_the_kanban_titles_still_match(self) -> None:
        for label in ("Board", "Board [ALL]", "Board [herdr-board · ACTIVE]"):
            with self.subTest(label=label):
                call = self.decide([pane("w1:p3", label)])
                self.assertEqual(call, ["plugin", "pane", "focus", "w1:p3"])

    def test_with_a_kanban_and_a_linear_pane_each_is_matched_not_opened(self) -> None:
        kanban = pane("w1:p1", "Board [ALL]")
        linear = pane("w2:p1", "Linear: X")
        self.assertEqual(
            self.decide([pane("w9:p1", "zsh"), linear, kanban]),
            ["plugin", "pane", "focus", "w2:p1"],
        )
        self.assertEqual(
            self.decide([pane("w9:p1", "zsh"), kanban, linear]),
            ["plugin", "pane", "focus", "w1:p1"],
        )

    def test_the_toggle_focuses_then_closes_one_linear_overlay(self) -> None:
        # AE7: two presses against the same titled pane, and no second open.
        first = self.decide([pane("w1:p3", "Linear: Example Launch")])
        second = self.decide([pane("w1:p3", "Linear: Example Launch", focused=True)])
        self.assertEqual(first, ["plugin", "pane", "focus", "w1:p3"])
        self.assertEqual(second, ["pane", "close", "w1:p3"])


if __name__ == "__main__":
    unittest.main()
