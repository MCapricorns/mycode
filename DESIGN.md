# MYCode design system

## Direction

The MYCode desktop uses neutral slate surfaces, restrained blue accents and compact native typography. Fine dividers separate navigation, the conversation and Details. The native app icon, README banner and offline Wiki retain the folded paper and pixel M artwork.

## Color

| Role | Light | Dark |
| --- | --- | --- |
| Canvas | #F8F9FB | #171A1F |
| Navigation | #EEF1F5 | #1D2128 |
| Sheet | #FFFFFF | #252B34 |
| Primary text | #18212C | #E8EDF3 |
| Secondary text | #49586A | #AEB8C5 |
| Fine edge | #D1D9E4 | #363F4C |
| Accent | #345D8B | #8EAED7 |

Blue marks running tasks, amber marks waiting and cleanup, green marks a finished child, and red marks failures. Pair color with the state label.

## Material and geometry

Use a 4px spacing unit. Small details use 4px corners, controls 6px and sheets 8px. Desktop surfaces use opaque fills. Fine borders mark boundaries and a soft shadow separates floating panels. The Wiki uses layered sheet edges and a warm palette.

## Layout

- Conversation navigation stays on the left. The transcript, thinking traces, code and composer use the available central width. Window width does not silently change the user's selected font size.
- The start view shares the composer's 640px maximum width and aligns its heading, actions and history to the left. With a project selected, its name and path appear above the task entry and project switcher. Without a project, opening a folder and direct chat are both available. Recent projects and conversations sit side by side where space permits and stack in narrow panes. Conversation shortcuts follow the selected project when one is open. The start view scrolls independently of the transcript.
- The prompt field grows from two to six lines and then scrolls. The focused border highlights the editor. The bar below it groups the file picker, workspace, model, and thinking settings with the context meter and labeled send, steer, and stop controls. Controls wrap as the pane narrows.
- Details orders delegated work, file changes, then model and usage. Active runs appear above saved results. Each row shows the task, role, state, and elapsed time. The detail card opens above the inspector backdrop with the brief, activity, working path, and result.
- Settings use a dedicated navigation column, section title and grouped control rows. Menus, pickers, confirmation prompts and settings sheets share the same material and corner scale.
- Wiki navigation, graph, node details and source viewer keep their deep links. Narrow screens use the list and a separate detail view. Documentation and source stay available offline.

## Typography and interaction

Use the OS UI font for prose, labels and CJK text. Use an installed monospace font for code, paths and measurements. Body text follows the S/M/L/XL interface scale.

Keep focus outlines, native caption behavior, keyboard shortcuts, reduced motion, plain-text copy and cancellation semantics. Selection must remain distinguishable from hover. Long titles truncate in lists and wrap in details. Fenced code uses the application's shared lexical highlighter in both conversation and subagent results.

## Brand sources

- `crates/mycode-desktop/assets/icon.svg` is the native mark.
- `crates/mycode-desktop/assets/wordmark.svg` is the pixel wordmark.
- `scripts/make_icon.py` generates PNG, ICO, ICNS and the size proof from the mark, plus six gradient wordmarks.
- `scripts/build_wiki.py` generates the offline Wiki, logo, wordmark and README banner.
- `scripts/wiki_shell.html` owns the Wiki's layout and behavior.

Generated assets must be regenerated from their source before publication. Do not maintain separate logo drawings for different surfaces.
