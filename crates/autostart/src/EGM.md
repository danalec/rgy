# crates/autostart/src

## Overview
The seven library/binary sources of `rgy-autostart`: registering the rgy
search daemon to start at login, per platform. The crate root declares
`#![no_std]` with `extern crate alloc`; the decision (which platform, which
mechanism, which arguments) lives in `plan` and is portable, while the parts
that touch the machine (`render`, `linux`, `macos`, `windows`, `main`) are
behind the `std` feature. The `rgy-autostart` binary is declared in the
manifest with `required-features = ["std"]`.

## Interface
- `[lib.rs:no_std]` — crate root `#![no_std]` and `extern crate alloc`.
- `[lib.rs:pub mod plan]`, `[lib.rs:pub mod render]`, `[lib.rs:pub mod linux]`,
  `[lib.rs:pub mod macos]`, `[lib.rs:pub mod windows]`.
- `[lib.rs:pub use plan::{Mechanism, Platform, choose_mechanism, default_name, detect_platform, launcher_entry_supported}]`.
- `[lib.rs:DAEMON_NAME]`, `[lib.rs:LAUNCHER_NAME]`,
  `[lib.rs:LAUNCHER_HAS_NO_LOGIN_ENTRY]`, `[lib.rs:RUN_KEY]`.
- `[lib.rs:Launcher]`, `[lib.rs:Report]`, `[lib.rs:Removal]`.
- `[lib.rs:launcher_default_name]`, `[lib.rs:uninstall_targets]`,
  `[lib.rs:install_status]`, `[lib.rs:default_args]`, `[lib.rs:effective_args]`,
  `[lib.rs:command_words]`, `[lib.rs:root_of]`, `[lib.rs:listen_of]`,
  `[lib.rs:mechanism_label]`, `[lib.rs:split_args]` — pure decisions.
- `[lib.rs:cache_dir_from]`, `[lib.rs:cache_dir]`, `[lib.rs:socket_path_in]`,
  `[lib.rs:pipe_name_for]`, `[lib.rs:pipe_name]`, `[lib.rs:home_dir]`,
  `[lib.rs:display_path]`, `[lib.rs:resolve_binary]`, `[lib.rs:find_daemon]`,
  `[lib.rs:find_launcher]`, `[lib.rs:is_executable]` — std helpers.
- `[plan.rs:Platform]`, `[plan.rs:Mechanism]`, `[plan.rs:Transport]`,
  `[plan.rs:detect_platform]`, `[plan.rs:choose_mechanism]`,
  `[plan.rs:default_name]`, `[plan.rs:transport]`, `[plan.rs:listen_arg]`,
  `[plan.rs:launcher_entry_supported]`.
- `[render.rs:systemd_unit]`, `[render.rs:launchd_plist]`,
  `[render.rs:windows_task_command]`, `[render.rs:schtasks_command_line]`,
  `[render.rs:reg_add_command_line]`.
- `[linux.rs:config_home]`, `[linux.rs:write_unit]`, `[linux.rs:enable]`,
  `[linux.rs:status]`, `[linux.rs:systemctl]`; `[macos.rs:write_plist]`,
  `[macos.rs:load]`, `[macos.rs:launchctl]`; `[windows.rs:install_task]`,
  `[windows.rs:install_run_value]`, `[windows.rs:task_command]`.
- `[main.rs:main]`, `[main.rs:run]`, `[main.rs:install]`,
  `[main.rs:uninstall]`, `[main.rs:status]`, `[main.rs:doctor]` — the CLI.

## Invariants
- `[lib.rs:no_std]` — `no_std` + `alloc`; `extern crate std` only behind
  `#[cfg(feature = "std")]`.
- `[lib.rs:DAEMON_NAME]` — `searchd.exe` on Windows, `searchd` elsewhere;
  `[lib.rs:LAUNCHER_NAME]` — `rgxy.exe` / `rgxy`.
- `[lib.rs:default_args]` — the root is always explicit (`--root .` by
  default) and the transport is named, because a login entry starts in the
  platform's own directory and stdio ends.
- `[lib.rs:effective_args]` — a user-named `--root` or `--listen` wins over the
  tool's default.
- `[lib.rs:uninstall_targets]` — an uninstall removes the daemon and launcher
  entries together unless a name was given.
- `[lib.rs:install_status]` — the status keeps saying what happened to the
  daemon; a missing launcher does not undo a daemon that was armed.
- `[lib.rs:RUN_KEY]` — `HKCU\...\Run`, the user's hive, never the machine's;
  nothing is elevated and nothing prompts.

## Side Effects
- `[main.rs:install]`, `[main.rs:uninstall]` — write/remove systemd user units,
  launchd plists, Task Scheduler tasks and `HKCU\...\Run` values.
- `[windows.rs:install_task]`, `[windows.rs:install_run_value]` — run
  `schtasks` / `reg`; `[linux.rs:enable]` — run `systemctl`;
  `[macos.rs:load]` — run `launchctl`.
- `[lib.rs:home_dir]`, `[lib.rs:find_daemon]`, `[lib.rs:find_launcher]` — read
  `HOME`/`USERPROFILE`/`PATH` and stat binaries.
- `[lib.rs:resolve_binary]` — `std::fs::canonicalize`.
- `[lib.rs:cache_dir_from]` — delegates to `rgy_proto::endpoint::cache_dir_from`.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
