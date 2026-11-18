# doannt-kindle-tools

Code shared by the Kindle projects of the same author: [Primitive Launcher](../doannt-kindle-launcher) and
[Primitive Browser](../doannt-kindle-browser). Each project includes this repository as a git submodule under `tools/`.

| Crate | What |
|---|---|
| `crates/primitive-keyboard` | The on-screen keyboard: layers, rows, the text editing of a key press, and its look and geometry (`style`, `layout`). |
| `crates/primitive-quick` | Quick settings pieces: the CPU, memory and storage figures read from Linux, and the panel's icons. |

Each app draws its own screens (the launcher with vello, the browser as HTML); both read the same definitions from
here, so a key or an icon looks and sits the same in both.

Licence: AGPL-3.0, as the projects that use it.

Test: `cargo test --workspace`.
