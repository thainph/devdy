# Devdy design system

Single reference for how UI is built in `src/` (app + `controller/`). Before
adding UI, reuse what is listed here; if something is missing, extend the shared
primitive (or add a token) instead of hand-writing classes in a screen.

Primitives live in `src/components/ui/` and are imported from `@/components/ui`.

## 1. Tokens

### Colors (`src/assets/base.css`)

Use the semantic utilities — they follow light/dark mode and the color themes.

| Purpose | Utility | Never |
|---|---|---|
| Page / surface | `bg-background`, `bg-card`, `bg-sidebar`, `bg-muted` | `bg-white`, `bg-slate-*` |
| Text | `text-foreground`, `text-muted-foreground` | `text-gray-*` |
| Brand / selected | `primary` (`bg-primary`, `text-primary`, `bg-primary/15` for a selected row) | `indigo-*` |
| Hover | `bg-accent` (`hover:bg-accent/50` for rows) | |
| Error / delete | `destructive` | `red-*` |
| Success / done / connected | `success` | `emerald-*`, `green-*` |
| Warning / pending / overdue-soon | `warning` | `amber-*`, `yellow-*` |
| Info / neutral highlight | `info` | `sky-*`, `blue-*` |

Tinted surfaces use opacity on the same token: `bg-warning/10 border-warning/20 text-warning`.
Solid fills pair with their foreground: `bg-success text-success-foreground`,
`bg-warning text-warning-foreground`, `bg-info text-info-foreground`,
`bg-destructive text-destructive-foreground` (never `text-white` / `text-black`).
Question / plan prompts and the "ok" segment of usage meters use `info` (fixed hue — `primary` changes per color theme and can collide with warning/destructive).

Exceptions (raw palette is OK): `Badge` solid tones, charts/Gantt series colors,
calendar event colors, mascot/decorations — data-driven palettes, not UI state.

### Radius

`rounded-md` controls and buttons · `rounded` inner chips / segments · `rounded-lg`
cards & floating toolbars · `rounded-full` dots, count pills, avatars.

### Typography scale

| Size | Use |
|---|---|
| `text-[10px]` | count pills, Button `xs` text, micro labels, kbd hints |
| `text-[11px]` | meta lines, hints under fields, list secondary text, segmented controls, banners |
| `text-xs` (12px) | default UI text: form fields, Button `sm`, list content, menu items |
| `text-[13px]` | list row titles that need slightly more weight than body |
| `text-sm` (14px) | page titles (`PageHeader`), list row primary titles, body copy |
| `text-[15px]` | detail-pane title (`DetailHeader`) |
| `text-lg`+ | empty states / About only |

`text-[9px]` is reserved for `Badge size="xs"`. Don't use `text-[12px]` / `text-[14px]`
(use `text-xs` / `text-sm`).

### Icons (lucide)

| Context | Size | Stroke |
|---|---|---|
| inside Button `xs`, inline meta, inline retry | `h-3 w-3` | 2 (1.75 for outlined glyphs) |
| inside Button `sm` / `icon-sm` / `icon`, menu items, list rows | `h-3.5 w-3.5` | 1.75 |
| inside Button `md` / `icon-lg`, card actions | `h-4 w-4` | 1.75–2 |
| empty states | `h-6 w-6` in a `h-12 w-12 rounded-xl bg-muted` tile | 1.5 |

## 2. Sizing scale — one height per size, shared by every control

| Size | Height | Button | Input / AppSelect / DatePicker | Icon button |
|---|---|---|---|---|
| — | 24px `h-6` | — | — | `size="icon-xs"` (dense spots only) |
| `xs` | 28px `h-7` | `size="xs"` (10px text) | `size="xs"` | `size="icon-sm"` |
| `sm` | 32px `h-8` | `size="sm"` (default) | `size="sm"` (default) | `size="icon"` |
| `md` | 36px `h-9` | `size="md"` | `size="md"` | `size="icon-lg"` |

Heights come from `controlHeight` in `controlStyles.ts`. Controls placed in the
same row must use the same size, so their edges line up. Never override height
with `class="h-…"` on a shared control.

## 3. Buttons — which size/variant where

Always `<Button>`; a raw `<button>` is only for non-button-looking hit areas
(a list row, a tab, a checkbox square, an inline text link, a chip's ×).

| Place | Size | Variant |
|---|---|---|
| `PageHeader` actions | `sm` / `icon` — a labelled one as `PageHeaderButton` (§4 Narrow panes) | `outline` (primary action `primary`) |
| List sidebar toolbar (next to search) | `icon` | `outline` (`primary` when toggled on, e.g. pin) |
| List sidebar "+ New" | `sm` | `primary` |
| Detail-pane header (`DetailHeader`, Note/Todo/Secret/Slack headers): Save, Cancel, More ⋯, Edit, Done, plus the project select / due date / Sessions in that row | `sm` / `icon` (32px — same as the list sidebar toolbar beside it) | Save `primary`, others `outline` |
| `DetailSection` actions (Add, Test…) | `xs` | `outline` |
| Inline row actions (remove a row, clear a field) | `icon-sm` | `destructive-ghost` / `ghost` |
| Dense spots (banner retry, chip ×, folder-header actions, h-8 sub-toolbars) | `icon-xs` | `ghost` / `destructive-ghost` |
| Card hover toolbars | `icon-sm` | `ghost` / `destructive-ghost` |
| Modal / Drawer footer | `sm` | Cancel `ghost`, confirm `primary`, delete `destructive` |
| Empty state CTA | `sm` | `primary` (+ secondary `outline`) |

Toggled icon buttons: `:variant="on ? 'primary' : 'outline'"` + `:aria-pressed`.
Every icon-only button needs `:title` and `:aria-label`.

## 4. Layout patterns

- **Full-page view**: `PageHeader` (h-13, px-6 → px-3 when narrow) → optional
  toolbar strip (`border-b border-border/60 px-6 py-2`) → scrolling content.
- **List-detail screen** (Skills, Rules, MCP, Servers, Notes/Todos, Slack, Secret
  notes): list sidebar (`listDetail/ListDetailSidebar`, `items/ItemSidebar`,
  `slack/SlackSidebar`) + right pane with `listDetail/DetailHeader`, body of
  `listDetail/DetailSection`s, and `listDetail/DetailEmpty` when nothing is
  selected. The sidebar is 40% of the screen, 220–360px (`widthClass` of
  `useListPaneSidebar`); never give it a fixed width.
  A sidebar has one header row (`ListSidebarHeader`: title + count, optional
  quick-filter badge, "+ New", ⋯ with Reload / secondary actions / Hide the
  list), one `ListSearchBox` whose sliders menu holds every filter, sort and
  display option plus "Reset to defaults" (`ListMenuLabel` / `ListMenuCheck`),
  `ListFilterSummary` while filtering, then `ListGroupHeader` folders. No other
  control rows (selects, segmented controls, chip or button rows).

### Narrow panes (split view)

Any screen can be shown in half of a split, so every screen must work from a
~360px pane with no horizontal page scroll. Size against the screen's own
width with container queries, never the window (`sm:` / `md:` / `lg:` read
the window and are wrong in a split):

- Put `@container/screen` on the screen root and use `@[40rem]/screen:…`
  (narrow below 640px): padding `p-3 @[40rem]/screen:p-6`, grids that gain
  columns, a section nav (Settings, Project settings) that goes icon-only
  (`w-12`, labels `hidden @[40rem]/screen:block`, `title` + `aria-label` kept).
- `PageHeader` is its own `@container/page-header`. The title always truncates.
  Below 40rem its actions go compact:
  - `PageHeaderButton` (`label` + icon in the slot): icon-only when narrow, the
    label stays as `title` / `aria-label`.
  - `PageHeaderSearch` (`v-model`, `placeholder`): a w-56 field when wide, an
    icon button that opens the field over the header row when narrow.
  - Custom buttons (popover triggers…): `pageHeaderCompactButton` on the
    Button and `pageHeaderLabel` on the label (`pageHeaderWideLabel` for a
    secondary button in a crowded header, labelled from 56rem).
  - Header `#meta` content that doesn't fit (Stats filters) moves to a wrapping
    toolbar strip below the header: render it in `#meta` behind
    `hidden @[64rem]/page-header:flex` and in the strip behind `@[64rem]/screen:hidden`.
- List-detail screens: `provideListPane(key, { autoHide: true })` takes the list
  out of the layout below 640px of screen width (`LIST_PANE_NARROW_BELOW`); the
  detail header's ☰ (`ListPaneButtons mode="show"`) floats it as a peek, ⌘B
  peeks too. This never changes the user's remembered hide / show choice.
- Detail headers wrap: the actions drop under the title, right-aligned
  (`flex-wrap` + `ml-auto justify-end`), instead of overflowing.
- A wide data table (Processes) may scroll inside its own card
  (`overflow-x-auto` + `min-w-[…]`); the page itself never scrolls sideways.
- **Switchers** (filters, view modes, auth method): `SegmentedControl`.
- **Dropdowns**: `AppSelect` for values, `DropdownMenu` + `DropdownItem` for actions.
- **Dialogs**: `Modal`, `ConfirmModal`, `PromptModal`, `Drawer` — never a custom overlay.
- **On/off**: `ToggleSwitch` — `size="sm"` in list rows/cards, `size="md"` in
  settings rows; `:checked="'mixed'"` for partially-enabled groups. A switch applies
  at once; for picking items in a form (applied by a later button) use `Checkbox`.
- **Checkboxes**: `Checkbox` (`v-model`, label in the default slot — the whole row is
  the hit area; `'mixed'` for a group header; `tone="destructive"` for a dangerous
  option). Never a raw `<input type="checkbox">`.
- **Status pill**: `Badge` / `StatusBadge`; count pill: the one in `PageHeader`
  (`#leading` slot for a back button or page icon).
- **Inline messages**: `Banner tone="destructive|warning|success|info"` (+ `#actions`
  slot for retry/dismiss) — never a hand-copied tinted box.

## 5. Remote controller (`src/controller/`, phones)

Same tokens (§1), touch-first layout — don't use `PageHeader` / list-detail or
desktop `xs`/`icon-sm` sizes here.

| Piece | Size | Use |
|---|---|---|
| `ControllerHeader` | h-12, px-3, title `text-base font-semibold` | header of every tab/editor |
| `HeaderButton` | 32px, icon `h-4 w-4` | back / refresh / disconnect, plain icon buttons in the composer |
| `FabButton` | 56px round, bottom-right | "+ new" on list tabs |
| `BottomTabBar` | icon `h-5`, label `text-[10px]` | tab navigation |
| `SearchField` | full width | top of list tabs |
| Sheets (`ThemeSheet`, `PushSheet`) | scrim `bg-black/50`, `rounded-t-xl` panel | secondary settings |
| List rows | `px-3 py-2.5`, whole row tappable, `border-b border-border/40` | inbox / runs / notes |

Touch targets: ≥ 32px in dense toolbars, ≥ 36px (`Button size="md"`) for decision
buttons, ≥ 44px for a standalone primary action.

## 6. Themes and skins

Palettes live one per file in `src/assets/themes/<id>.css` (imported at the top of `base.css`) and are registered in `src/lib/colorThemes.ts`. Every file defines a `[data-theme='<id>']:not(.dark)` and a `[data-theme='<id>'].dark` block with the full token set plus `--aurora-1/2`; `src/lib/__tests__/themeContrast.test.mjs` enforces WCAG AA on every pair.

A **skin** (group `skin`) also changes shape, material and motion, not just color. Skin rules go in the same file, **outside `@layer`** (unlayered rules beat Tailwind utilities), always scoped under `[data-theme='<id>']`. Hooks a skin may restyle:

| Hook | Element |
|---|---|
| `--radius` | every `rounded-sm/md/lg` (and primitives) |
| `--font-ui` | body font stack (`font-mono` follows Tailwind's `--font-mono`) |
| `.ui-card` `.ui-modal` `.ui-drawer` `.ui-popover` `.ui-toast` | surfaces (Card, Modal, Drawer, Dropdown/ContextMenu, toasts) |
| `.ui-btn` + `.ui-btn-<variant>` | Button (`primary`, `outline`, `ghost`, `destructive`, `destructive-ghost`, `subtle`) |
| `.ui-field` | Input / Textarea / AppSelect trigger |
| `.ui-menu-item` | DropdownMenu row (DropdownItem) |
| `.ui-switch` · `.ui-switch-knob` | ToggleSwitch track (`aria-checked`) · knob |
| `.ui-checkbox` | Checkbox box |
| `.ui-segmented` + `.is-active` | SegmentedControl group · selected segment |
| `.ui-badge` + `.ui-badge-<tone>` | Badge (`neutral`, `running`, `success`, `error`, `warning`, `info`, `primary`) |
| `.ui-backdrop` | Modal / Drawer scrim |
| `.app-shell` · `.app-titlebar` · `.app-sidebar` | window root (opaque `bg-background` — it hides `body::before` unless a skin makes it translucent) · title bar · navigation sidebar |
| `.app-sidebar .is-active` · `.app-nav-indicator` | active nav link · its 2px primary bar |
| `.rt-card` · `.rt-tray` · `.rt-row` · `.rt-empty` · `.rt-kbd` | Dashboard bento tile (also `.ui-card`) · Running / History tray · session row on a tray (tints from `--card`, set it there for translucent rows) · tray empty state · shortcut keycap |
| `body::before` / `body::after` | full-window background layers (aurora by default) |
| `html[data-agent-state]` | `idle` · `running` · `waiting` · `error` · `done` — the app-wide agent state (see `src/assets/agent-ambient.css`) |

Skin rules: keep text readable (the contrast test still runs on the tokens), put any animation behind `@media (prefers-reduced-motion: no-preference)` and under `html:not([data-animated-bg='false'])`, and never animate layout properties — the stream log must stay smooth.

## 7. Checklist for a new screen

1. Layout from §4, primitives from `@/components/ui`; works in a 360px pane (§4 Narrow panes).
2. Buttons and controls follow §2–§3; same-row controls share one size.
3. Colors only from §1 tokens; icons per the icon table.
4. i18n keys in both `en` and `vi`; titles/aria-labels on icon buttons.
