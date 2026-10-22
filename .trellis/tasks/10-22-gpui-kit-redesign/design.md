# GPUI Kit desktop workspace

## Scope and direction

Operate: extraction queue, archive browser, history/recovery, password library, settings and quick mode. Keep the engine, persisted data contracts, safe cancellation and explicit recovery unchanged. GPUI Kit 0.8 supplies controls, tabs, menus, resizable panes and focus semantics.

Inspected: `ui/mod.rs`, all page modules, configuration editor, preferences, desktop-beta workflow, GPUI Kit official getting-started documentation and local 0.8 component implementations. The old architecture already aliases gpui-kit as gpui, but concentrates workspace lifetime, navigation, configuration and rendering in one module. The old visual baseline is `.work/gui-proof/baseline/tasks.png`.

Alternatives considered:
- Dashboard with summary cards: rejected; counts are filters, not the primary object.
- Permanent three-column cockpit: rejected; an empty inspector and always-open defaults consume the queue.
- Task-first workspace with contextual inspector: selected. Files dominate; controls remain at the top, details/configuration appear only when useful.

## Visual Direction Contract

- Compact navigation: tasks, archives, history, passwords. Recovery belongs to history; appearance and integration belong to settings.
- Left alignment, one page heading, one primary action per context. No repeated titles, marketing/status filler or explanatory paragraphs for obvious controls.
- Main object: virtualized file queue / archive directory. Inspector is a counterweight, not a second empty workspace.
- Embedded static Noto Sans CJK SC Regular; 16 px body, 14 px metadata, 20 px title ceiling. Respect user font scaling.
- Spacing: 4/8/12/16/24 px. Controls 6 px corners, panes mostly flat. Group by spacing and tone; borders only for panes, tables and separators.
- Neutral light/dark surfaces with teal selection/action accent. Semantic success/warning/danger from the theme.
- Use toolkit Sidebar, TabBar, Checkbox, Input, Switch, menus, resizable panels and Empty. Do not recreate their interaction models in divs.
- No image assets needed. Bundle the official Noto CJK Sans 2.004 static Regular face and OFL for consistent Chinese/Latin weight; include its license in desktop packages and Nix installations. Existing Lucide archive/folder symbols communicate the actual objects.
- Motion is limited to component focus/selection/progress. Existing reduced-motion preference remains effective; no new animation dependency.
- Narrow windows: icon navigation; details become a stacked, scrollable region. Quick mode retains a single focused extraction flow. Desktop app; no invented mobile website.

## Architecture and verification

Workspace owns execution/data lifetime; View owns navigation, selection and input entities. Separate workspace, shell/navigation, quick configuration and task/page render modules. Preserve typed channels and off-UI-thread archive work.

Verify GUI crate compilation/tests, launch isolated configuration/database, inspect empty and populated queue, archive/directory/member preview, settings, password/history/recovery, light/dark and compact/narrow layouts. Record static, runtime and screenshot evidence separately. Native macOS testing remains platform-specific.

## Render-driven refinements

- User feedback: small/light text. Raised body/control typography, darkened muted foreground, and embedded a static regular CJK face instead of relying on variable platform faces. Existing explicit font preferences remain user-controlled.
- User feedback: collapsed icons misaligned. Removed nested footer padding, centered the header/toggle, and gave all navigation items the same axis.
- User feedback: text/button alignment and spacing. Native 40px controls, left-aligned file/history content, one 20px shell inset, consistent toolbar gaps and rows; width is adjusted directly by dragging the inspector.
- Real member preview exposed byte-zero archives being disabled and p7zip returning empty bytes for Chinese selectors under the C locale. Allow byte-zero previews and use C.UTF-8 for bounded member reads. The new regression fails against the old locale and passes with both installed 7z/7zz.
