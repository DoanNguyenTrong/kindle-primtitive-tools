# PrimitiveOS shared tools

Shared Rust code for PrimitiveOS apps, including [Primitive Launcher](../doannt-kindle-launcher) and
[Primitive Browser](../doannt-kindle-browser). These projects include this repository as a git submodule under `tools/`.

| Crate | What |
|---|---|
| `crates/primitive-keyboard` | The on-screen keyboard: layers, rows, the text editing of a key press, and its look and geometry (`style`, `layout`). |
| `crates/primitive-quick` | Quick settings pieces: the CPU, memory and storage figures read from Linux, and the panel's icons. |
| `crates/primitive-app` | The native app kit (`primitive_app::run`): event loop, drawing, saving, USB drive mode, and the app's part of the system layer contract under Primitive Launcher (redraw on SIGUSR1 without a flash, the system theme, the top-edge swipe left to the launcher). See the module docs in `src/lib.rs`. |

Each app draws its own screens (the launcher with vello, the browser as HTML); both read the same definitions from
here, so a key or an icon looks and sits the same in both.

Licence: your choice of the GNU Affero General Public License v3.0 ([LICENSE-AGPL](LICENSE-AGPL)) or the PolyForm
Noncommercial License 1.0.0 ([LICENSE-POLYFORM](LICENSE-POLYFORM)), so each app that uses it can take it under its own
licence: Primitive Launcher and Primitive Browser under PolyForm Noncommercial, Primitive Terminal under AGPL-3.0.
Copyright (C) 2026 Doan Nguyen.

Test: `cargo test --workspace`.
