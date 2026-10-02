---
authors: Claude
type: decisions
project: herdr-board
topics: [linear, tui-design, responsive]
---

# Responsive board — agreed rules (2026-10-02)

Agreed with Shawn; prototyped in the tuirealm toy (`~/projects/tuirealm-toy`). Build target: a new branch off `main` (the store migration merged as PR #7).

## Level, from where the board pane sits

| Board pane is… | Shows |
|---|---|
| The only pane in its tab | **Space level** — the whole board for the herdr space |
| One split among several | **Space → tab → active split** — what the focused split represents |

1. **Active split** = the last focused split *other than the board*, so clicking into the board to read does not change what it shows.
2. **What a split represents:** an agent pane bound to an issue → that issue's focused detail view; an unbound split → the tab's issue; no tab issue → the space board.
3. Replaces the `board tui --session` launch flag (PR #6): one TUI picks its level from its position.

herdr support (verified on 0.9.3 / protocol 22): `pane.layout` returns the tab's panes, rects, focused pane and zoom; `pane_focused` and `layout_updated` events report changes live. Caveat found while testing: the layout API's sizes (e.g. 120×40) did not match the pane's real terminal size, which follows the attached client — use the terminal's own size for width decisions and herdr's layout only for "how many panes, which is focused".

## Width

| Rule | Value |
|---|---|
| Minimum column (card + gaps) | 48 cells (44-cell card); was 53 |
| Columns shown | width ÷ 48, rounded down, capped at the workflow's column count; columns stretch to fill |
| Right-hand detail column | **Removed** (2026-10-02) — the pinned overlay replaces it, on demand |
| Card width | Capped at 64; a column whose inside is ≥ 82 cells shows cards two per row (min 40 each, 2-cell gap) — in practice single-column boards 86–95 wide. Lane bands count rows of cards so lanes stay aligned |
| Narrower than one column | List view (cards as rows, grouped by status, lanes inside), Return opens detail — not prototyped yet |
| Hidden columns | The outer left / right gaps become `←` / `→` buttons (with the hidden count) when columns are hidden on that side. Selected = one tone above the board (`panel_bg`), otherwise flush. Moving past the last visible column selects the button; a press toward its side pages one column without moving the selected issue; one press back steps onto the adjacent column (and reaching the end lands there too); Enter / Esc / ↑↓ return to the selected issue. Sideways scroll or Shift+wheel pages the same way |
| Resize | Re-fit immediately on terminal resize (herdr splits resize panes constantly) |

## Opening an issue

| Mode | How | Behaviour |
|---|---|---|
| Modal | Return, when the board is ≥ 136 cells wide | Centred window (≤ 128 wide) over the board; the board shows muted behind it |
| Expand | `󰁌 Expand` button / `e` | Fills the board; the button becomes `Window e` to shrink back |
| Overlay | Overlay button / `o` | Pins the issue as a floating panel over the right of the board (single-column layout). The board keeps the keys underneath; `o` from the board unpins |
| Narrow | Return, when the board is < 136 wide | Fills the board directly; no buttons |

Header buttons sit right-aligned on the issue's header row in wide views only. Esc closes the modal or the full view and returns to the card.
