---
authors: Claude
type: design-spec
project: herdr-board
topics: [linear, tui-design, theme, responsive]
---

# Linear board design

The target look and behaviour for the Linear-mode board TUI, agreed with Shawn on 2026-10-01/02.
The working reference is the tui-realm prototype at `~/projects/tuirealm-toy` (commit 1c1b43d).
Read it for exact rendering. The prototype uses tui-realm and mock data; herdr-board keeps ratatui
and its own reducer. Port the design, not the framework.

Related: `responsive-board.md` (levels, widths, paging, issue open modes) and
`issue-detail-inventory.md` (what data exists, and the Linear + `gh` source split).

## 1. Theme

The board uses herdr's theme. herdr exposes no theme API to panes, so the board reads
`[theme] name` and `[theme.custom]` from `~/.config/herdr/config.toml` and maps them to herdr's own
token names. The current theme is `gruvbox` (herdr v0.9.3 `src/app/state.rs`).

| Role | Token | gruvbox value |
|---|---|---|
| App background | not a herdr token (Shawn's value) | `#1e1e1e` |
| Card / panel surface | `panel_bg` | `#282828` |
| Raised surface (selected card, chips, modal cards) | `surface0` | `#3c3836` |
| Rules, quiet lines | `surface1` | `#504945` |
| Dim text | `overlay0` / `overlay1` | `#928374` / `#a89984` |
| Text / secondary text | `text` / `subtext0` | `#ebdbb2` / `#d5c4a1` |
| Accent | `accent` | `#d79921` |
| Semantic | `red green yellow blue teal mauve peach` | herdr gruvbox values |
| Muted cards (unfocused columns) | not herdr tokens | text `#928374`, detail `#7c6f64`, surface `#262626` |

Rule: a surface is one tone above whatever it sits on. A card on the board is `panel_bg`. A card
inside a modal or a pinned panel (both `panel_bg`) is `surface0`.

Icons are Nerd Font glyphs (the terminal font is JetBrainsMono Nerd Font). The codepoints are in
the prototype's `icon` modules.

## 2. Board layout

| Element | Design |
|---|---|
| Columns | No borders. 4-cell gap between columns (2 cells of each column's slot). |
| Column titles | One powerline bar across the board. Focused column: herdr's active-tab style (`panel_bg` text on `accent`, bold), filling its slot including its half of each gap. Others: `overlay0` on `panel_bg`, running together with no divider. A full `/` cut (`U+E0BC`) where the colour changes, and at the bar's right end. No underline row. |
| Swimlanes | One line per lane across all columns: `─ ALPHA ─────── 5 󰈳 ─`. The count and filter icon are right-aligned (`md-filter_outline` expanded, `md-filter` folded). One blank row below (none in compact). Each lane band is as tall as its tallest column (the sum of its card heights and gaps), so lanes align. |
| Lane states | Normal: dim. Holds the selected card: label `accent`. Lane line selected: plus `surface0` background. Nothing selected: all lanes in the "holds the selected card" state. |
| Folding | Lane lines are selectable in ↑↓ order (lane, its cards, next lane). Return folds or unfolds the lane in every column. |
| Spacing | 1 blank row between cards, below lane lines (not in compact), and between a lane's last card and the next lane line. |

## 3. Cards

Cards are sized to their content (Shawn, 2026-10-06). Widths are unchanged in both densities. Selecting a card never moves text or changes its height.

**Default density** keeps the roomy spacing and drops only rows that would be empty:

| Row | Content | Rows |
|---|---|---|
| padding | blank (top) | 1 |
| header | `WEB-101 P0 ⧉2`, left-aligned. Marks and `▶` right-aligned | 1 |
| rule | blank; a solid rule when selected | 1 |
| title | bold, wrapped; `yellow` when selected | 1–2 |
| blank | only when a description follows | 0–1 |
| description | wrapped, ends in `…`; only when the issue has one | 0–3 |
| rule | blank; a dashed rule when selected; only when meta follows | 0–1 |
| meta | Nerd Font icon items, one row per group with data: assignee and milestone; PR, preview, diff, CI | 0–2 |
| padding | blank (bottom) | 1 |

A full card is 13 rows, as before. A card with a 2-row description and one meta row is 11; a title-only card is 5.

**Compact density** (`z` toggles it) drops the padding, rule and blank rows: header, title 1–2, description 0–3, meta 0–2. Selection shows only the surface and title colour. A full card is 8 rows, a title-only card 2.

In a 2-up row, the row is as tall as its taller card.

- **Inset:** 2 cells on each side, so text keeps its column whether or not the card is selected.
- **Selected card:** `surface0` background and a yellow title. No border. Rules only in default density.
- **Unfocused columns:** muted tones (section 1).
- **Marks keep their colour everywhere:** `!` red, `?` peach, `✓` green, `◇` mauve, `◉` blue.
- **Width:** cards cap at 64 cells. A column whose inside is ≥ 81 cells shows cards 2-up (minimum 40 each, 1-cell gap). See `responsive-board.md`.
- **Newest agent note:** when a card has one, it replaces the last description line as `› note`, or takes one description row when there is no description.

## 3b. Existing pieces, placed (Shawn, 2026-10-02)

| Today | In the redesign |
|---|---|
| Linear view tabs (`[Frontend]  Backend`, `[` `]`) | A second powerline row above the column titles, in the same style |
| Agent show-requests (pinned `◉` line, `a` accept / `x` dismiss) | A bottom-bar alert per request. Selecting it in the bar and pressing `a` / `x` acts on it |
| Spaces strip (`s`, unmapped tabs `t`) | Only on the "not bound" screen |
| Newest agent note (`› note` row) | Replaces the card's last description line when present |

## 4. Focus and keys

| Key | Action |
|---|---|
| ↑↓ / jk | Move through lanes and cards; ↑ from the top item reveals the filter |
| ←→ / hl / Tab | Change column. Past the last visible column, select the paging button |
| `/` | Filter (hidden until opened; stays visible while it holds text) |
| Return | Card: open the issue. Lane: fold or unfold |
| Esc | Clear the selection: nothing focused; all columns and cards in their default, unmuted state; all lanes active |
| `?` | Open the bottom bar |
| `z` | Toggle compact density |
| `p` | Focus the pinned issue panel (Esc or `p` returns) |
| ctrl+q | Quit (plain `q` does nothing) |

New keys win over today's bindings (Shawn, 2026-10-02): `?` opens the bar, and the old help sheet becomes a `keys` item in it. `o` means Overlay in the issue view, and focusing a pane moves to Return on its row. `q` no longer quits, and Esc deselects.

When nothing is selected, an arrow key, Tab or Return selects again in the last column you used.

## 5. Bottom bar

- **Zones:** global controls (left), context tip (centre, with `md-information_outline`), board-wide alerts (right).
- **Tip:** follows the selection. It shows the card's signals ("needs you and has a question · Enter opens it"), the lane fold action, the filter help, or "nothing selected".
- **Alerts:** cards with `!`, cards with `?`, failing CI. Each one jumps to its card and unfolds its lane.
- **`?` focuses the bar.** ←→ move between items, Return runs one, ↑ goes back, Esc clears the selection. It always opens on the first item.
- **Narrow screens:** when the three zones don't fit on one row, the bar stacks into three centred rows: tip, alerts, controls.

## 6. Issue view

Open modes are in `responsive-board.md` (modal, expand `e`, pinned overlay `o`, narrow).

- **Header and bottom bar are full width; the body has a 2-cell inset on each side.** This applies in every mode, including the narrow layout and the pinned panel.
- **Header:** powerline `ID │ status`, then the breadcrumb. In wide views, buttons are right-aligned (`󰁌 Expand e`, `Overlay o`; `Window e` when expanded; `Unpin o` when pinned).
- **Two columns:** a scrolling main column (100-cell reading cap) and a 40-cell sidebar docked to the right edge at full height. Below 100 cells, one column.
- **Main column:** title, description, ATTACHMENTS, PULL REQUESTS, ACTIVITY. Section lines use the lane line style.
- **Open PRs** are cards: state chip, branch → target, diff stats, checks summary, the specific merge blocker, reviews, preview URL. **Merged PRs** are one line each, with repo and diff aligned in columns.
- **Attachments other than PRs** are cards: kind and chip (commit, Slack thread, upload, link), title, detail (branch, channel, file name), link.
- **Modal:** no border (Shawn, 2026-10-06). Its `panel_bg` surface against the faded board marks its edge. Everything behind it is faded about 60% toward the background. The bottom bar is not faded.
- **Pinned panel:** takes one column's slot at the right. The board drops a column. No cards are hidden behind the panel.

## 7. Data sources for the issue view

- **Linear:** the issue, plus its attachments. Attachments say which PRs belong to the issue.
- **`gh`:** everything git-related for each PR: state, mergeability and the blocker, checks (deduplicated to the latest run per check), diff stats, reviews, and the preview URL from deployments.

See `issue-detail-inventory.md`.

## 8. Tab and split names (Shawn, 2026-10-02)

Every tab and split in the board's space always has a name, derived from its scope:

| Scope | Name |
|---|---|
| Tab bound to an issue | `WEB-101 · <title>` |
| Split bound to an issue | `WEB-101 · <agent>` |
| Anything else | `<repo or folder> · <agent>`, or the folder alone with no agent |

Names are capped at 40 cells. A name set by hand is never overwritten: the board replaces only names it generated, or blank ones.
