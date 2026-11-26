# MYCode design system

## Direction

MYCode uses layered paper and pixel geometry. The application, native icon, README banner and offline Wiki share one identity: warm paper, coral ink, a folded corner and a pixel M. The desktop is a working interface; pixel lettering belongs to the brand, while reading text and controls use native UI typography.

## Color

| Role | Light | Dark |
| --- | --- | --- |
| Canvas | #F6F1E8 | #191C1F |
| Navigation | #ECE3D7 | #22262A |
| Sheet | #FFFCF7 | #2B3034 |
| Primary text | #26232D | #F8F2E9 |
| Secondary text | #625B64 | #C6BFB5 |
| Fine edge | #DACDBD | #434B51 |
| Accent | #AD3B30 | #FF927D |

Blue marks running tasks, amber marks waiting and cleanup, green marks a finished child, and red marks failures. Pair color with the state label.

## Material and geometry

Use a 4px spacing unit. Small details use 4px corners, controls 8px, and sheets 12px. Main surfaces carry a single fine edge. A short contact shadow and a diffuse outer shadow separate floating sheets. Avoid heavy dark fills on every button and repeated embossed bottom borders.

The native window requests background blur. Chrome uses translucent fills; the reading canvas has a subdued diagonal tint. Floating panels use 97–99% opacity so underlying text does not interfere with reading. The Wiki uses layered sheet edges and the same warm palette; its primary content remains readable without backdrop filters.

## Layout

- Conversation navigation stays on the left. The transcript, thinking traces, code and composer use the available central width. Window width does not silently change the user's selected font size.
- Welcome centers the MYCode pixel wordmark above a row of equal 192 x 40px icon-and-text actions. There is no greeting, description, or repeated project path. Actions reuse the shared Paper button treatment: a coral primary action, a quiet secondary fill, fine borders, and matching semibold labels. The controls wrap without changing their size in narrow panes. Opening a project is primary before a folder is selected; describing a task is primary afterward. Recent projects form a compact list below the actions. Welcome content and composer share a 640px maximum width. Welcome scrolls from the top independently of transcript tail-follow. The conversation header appears only after work starts. The wordmark palette stays stable for the application lifetime, and direct chat remains available without selecting a project.
- The prompt field grows from two to six lines and then scrolls. The focused border highlights the editor. The bar below it groups the file picker, workspace, model, and thinking settings with the context meter and labeled send, steer, and stop controls. Controls wrap as the pane narrows.
- Details orders delegated work, file changes, then model and usage. Active runs appear above saved results. Each row shows the task, role, state, and elapsed time. The detail card opens above the inspector backdrop with the brief, activity, working path, and result.
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
