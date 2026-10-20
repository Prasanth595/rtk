# Kilo Code Plugin

> Part of [`hooks/`](../README.md)

## Specifics

- TypeScript plugin that intercepts `tool.execute.before` and mutates eligible Bash or shell commands in place.
- Delegates rewrite decisions to `rtk rewrite`; machine-readable output flags stay untouched.
- Locates RTK on Windows, Linux, and WSL, with `RTK_KILO_BIN` as an explicit override.
- Installed to `~/.config/kilo/plugin/rtk.ts` by `rtk init -g --agent kilocode`.
- Honors Kilo's `XDG_CONFIG_HOME` and `KILO_CONFIG_DIR` configuration-directory overrides.
- Set `RTK_KILO_BASH_PLUGIN=0` to disable rewrites for a session.

## Verification

From the repository root in Bash, with Rust 1.91 or newer and the `rustfmt`
and `clippy` components installed:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked
cargo test --test init_kilocode_test --locked
cargo test --all --locked
```

The regression tests run the command-line interface in temporary directories:
global installation writes the embedded plugin, repeated installation is a
no-op, dry-run writes nothing, uninstall preserves other plugins, and
project-scoped install and uninstall fail with the global command to use.
`KILO_CONFIG_DIR` takes precedence over `XDG_CONFIG_HOME`; empty overrides are
ignored. With neither override, the plugin goes under `~/.config/kilo`.

The command dispatch lives in `src/lib.rs`; `src/main.rs` only calls
`rtk::entry()`. Install and uninstall helpers live in
`src/hooks/init/kilocode.rs`, not in the instruction-only agents module.
