# MYCode design system

## Direction

MYCode uses layered paper and pixel geometry. The application, native icon, README banner and offline Wiki share one identity: warm paper, coral ink, a folded corner and a pixel M. The desktop is a working interface; pixel lettering belongs to the brand, while reading text and controls use native UI typography.

## Color

| Role | Light | Dark |
| --- | --- | --- |
| Canvas | #F8F3EA | #1B1B22 |
| Navigation | #EFE6D9 | #22222B |
| Sheet | #FFFBF4 | #2A2A34 |
| Primary text | #26232D | #F7F1E8 |
| Secondary text | #625B64 | #C2BBB2 |
| Fine edge | #DDD1C2 | #40404A |
| Accent | #C8373D | #F2786D |

Status colors remain semantic: green for completion, amber for waiting or warnings, red for failures. Workflow completion comes from the independent acceptance state; returned worktrees and integration handoffs are not successful completion.

## Material and geometry

Use a 4px spacing unit. Small details use 4px corners, controls 8px, and sheets 12px. Main surfaces carry a single fine edge. A short contact shadow and a diffuse outer shadow separate floating sheets. Avoid heavy dark fills on every button and repeated embossed bottom borders.

The native window requests background blur. Chrome uses translucent fills; the reading canvas has a subdued diagonal tint. Floating panels use 97–99% opacity so underlying text does not interfere with reading. The Wiki uses layered sheet edges and the same warm palette; its primary content remains readable without backdrop filters.

## Layout

- Conversation navigation stays on the left. The transcript, thinking traces, code and composer use the available central width. Window width does not silently change the user's selected font size.
- Welcome spans the available width and shows the shared mark, pixel wordmark and supporting copy directly on the page, without a banner background, border or shadow. On startup, one of six color gradients is selected for the wordmark; it stays stable while the application runs. A user can send a message in the default chat space before selecting a project.
- Details orders delegated work, file changes, then model and usage. Workflows are parent rows; their children are inspected through the node list. Individual tasks keep their stop and result actions.
- Agent details expand with the window, preserve composer clearance, and expose the workflow type, node state, dependencies, independent acceptance and previous results.
- Settings use a dedicated navigation column, section title and grouped control rows. Menus, pickers, confirmation prompts and settings sheets share the same material and corner scale.
- Wiki navigation, graph, node details and source viewer keep their deep links. Narrow screens use the list and a separate detail view. Documentation and source stay available offline.

## Typography and interaction

Use the OS UI font for prose, labels and CJK text. Use an installed monospace font for code, paths and measurements. The pixel wordmark is authored vector artwork and must not be approximated with a fallback font. Body text follows the existing S/M/L/XL interface scale.

Keep focus outlines, native caption behavior, keyboard shortcuts, reduced motion, plain-text copy and cancellation semantics. Selection must remain distinguishable from hover. Long titles truncate in lists and wrap in details. Fenced code uses the application's shared lexical highlighter in both conversation and subagent results.

## Brand sources

- `crates/mycode-desktop/assets/icon.svg` is the native mark.
- `crates/mycode-desktop/assets/wordmark.svg` is the pixel wordmark.
- `scripts/make_icon.py` generates PNG, ICO, ICNS and the size proof from the mark, plus six gradient wordmarks.
- `scripts/build_wiki.py` generates the offline Wiki, logo, wordmark and README banner.
- `scripts/wiki_shell.html` owns the Wiki's layout and behavior.

Generated assets must be regenerated from their source before publication. Do not maintain separate logo drawings for different surfaces.
