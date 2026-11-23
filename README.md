# tui-core

A small terminal UI framework in Rust: a screen stack, vim-style navigation (`hjkl`, `/` to filter), an animated reticle around the selected row, and inline images drawn 1:1 with the terminal's cells.

It powers the `font`, `dots` and `github` TUIs in [roest1/dotfiles](https://github.com/roest1/dotfiles). Each of those is its own binary sharing this crate, not a mode of one program.

## Use it

```toml
[dependencies]
tui-core = { git = "https://github.com/roest1/tui-core", branch = "main" }
```

`Cargo.lock` pins the commit. `cargo update -p tui-core` moves it to `main`'s head. Use this same spec everywhere: two crates that pass `tui_core` types to each other must resolve to one copy, and a different `rev` or `tag` counts as a different crate.

A screen implements `tui_core::Screen` and runs with `tui_core::app::run(Box::new(screen))`. `Flow::Push` opens another screen on top, and `q` pops back.

| Module | What it does |
| --- | --- |
| `app` | the loop, the screen stack, the row list and the pane |
| `screen` | what a screen provides: rows, a pane, a layout, keys |
| `host` | a screen made of several screens, one per tab |
| `nav` | the keymap, the same in every screen |
| `filter` | what `/` narrows a list to |
| `reticle` | the animated brackets around the selection |
| `width` | how many cells a string occupies |
| `image` | inline images |
| `probe` | asks the terminal for its cell size and colors |
| `term` | raw mode, the alternate screen, restore on panic |

## Work on it next to a dependent

Clone this repo, then build the dependent against your checkout without editing its files:

```bash
cargo build --config "patch.'https://github.com/roest1/tui-core'.tui-core.path='$HOME/github/tui-core'"
```

That build rewrites the dependent's `Cargo.lock` to the unpinned path, so restore it (`git checkout Cargo.lock`) before committing. A `--locked` build fails on a lock left that way. Never commit a `[patch]` section or a path dependency.

## License

MIT. See [LICENSE](LICENSE).
