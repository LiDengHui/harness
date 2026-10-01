//! A wasmtime plugin sandbox: capabilities are granted by the host, checked by
//! the host, and impossible for a plugin to talk its way out of.
//!
//! # Why the checks are here and not there
//!
//! A plugin is untrusted input — a `.wat` or `.wasm` file someone handed the
//! harness. Anything the plugin could enforce on its own behalf is therefore not
//! enforcement at all, so:
//!
//! * the manifest is supplied by the *caller*, never read from inside the
//!   module, and the host holds it;
//! * every host function re-checks that manifest on each call, and a refused
//!   call **fails the plugin call** rather than returning empty data a plugin
//!   could mistake for a result;
//! * every guest-supplied path goes through
//!   [`harness_core::path::resolve_within`], so `file_read` on `../../etc/passwd`
//!   is refused even though `file_read` was granted. Capabilities pick the kind
//!   of access; the path guard picks the place;
//! * modules may be signed with Ed25519 ([`PluginHost::load_signed`]) so a
//!   distributable plugin can be pinned to its author.
//!
//! # The plugin ABI
//!
//! Plugins are plain WebAssembly modules with no WASI and no ambient authority:
//! the only imports available are the four functions below. Because no Rust
//! target for `wasm32-wasip1` is required to write one, the examples in
//! `plugins/` are hand-written `.wat`.
//!
//! A module must export:
//!
//! | export | type | meaning |
//! |---|---|---|
//! | `memory` | `(memory …)` | the plugin's linear memory |
//! | `alloc` | `(func (param i32) (result i32))` | bump-allocate `len` bytes, return a pointer |
//! | *(manifest `entry`, e.g. `run`)* | `(func (param i32 i32 i32 i32) (result i32))` | the entry point |
//!
//! and one call is:
//!
//! 1. the host calls `alloc(input_len)` and writes the request there;
//! 2. the host calls `alloc(out_cap)` and passes both regions to the entry:
//!    `entry(in_ptr, in_len, out_ptr, out_cap)`;
//! 3. the entry returns the number of bytes it wrote to `out_ptr`, which the
//!    host reads back as UTF-8. A negative return means the plugin declined to
//!    answer.
//!
//! The pointer dance exists because the host needs somewhere in *guest* memory
//! to leave data, and a fixed offset would collide with whatever the plugin
//! keeps in its own data segment. Asking a plugin to expose an entry point and
//! an allocator is the smallest stable contract that avoids the host having to
//! know a plugin's layout.
//!
//! # Host imports
//!
//! | import | signature | capability |
//! |---|---|---|
//! | `harness.log` | `(ptr, len)` | none — always allowed |
//! | `harness.read_file` | `(path_ptr, path_len, dest_ptr, dest_cap) -> i64` | [`Capability::FileRead`] |
//! | `harness.write_file` | `(path_ptr, path_len, data_ptr, data_len) -> i64` | [`Capability::FileWrite`] |
//! | `harness.http_get` | `(url_ptr, url_len, dest_ptr, dest_cap) -> i64` | [`Capability::NetworkAccess`] |
//!
//! The data-transferring imports take a **destination pointer and capacity**
//! supplied by the plugin, for the same reason `entry` does: the host must not
//! guess at guest addresses. They return the number of bytes transferred, or
//! `-1` for an ordinary failure (no such file, connection refused) that the
//! plugin is free to handle. Two things are never "ordinary" and always fail the
//! whole call instead:
//!
//! * a missing capability, e.g.
//!   ``plugin `x` is not granted `file_read`, so `harness.read_file` is refused``;
//! * a path that escapes the workspace root, or a URL whose scheme is neither
//!   `http` nor `https`.
//!
//! # Limits
//!
//! Two ceilings, enforced by two different mechanisms because they answer
//! different questions:
//!
//! * [`SandboxLimits::timeout`] — **epoch interruption**. A background thread
//!   bumps the engine epoch every 10 ms and each call's store gets a deadline
//!   counted in those ticks. Wall-clock is what the field promises; the
//!   alternative, a fuel budget, counts instructions and so means different
//!   elapsed times on different machines.
//! * [`SandboxLimits::max_memory_bytes`] — a [`wasmtime::StoreLimits`] on the
//!   store, so `memory.grow` past the ceiling fails and a runaway plugin is
//!   stopped at the allocator instead of at the OOM killer.
//!
//! # Example
//!
//! ```no_run
//! # use harness_sandbox::{PluginHost, PluginManifest, SandboxLimits};
//! # use std::path::Path;
//! # async fn run() -> harness_core::Result<()> {
//! let mut host = PluginHost::new(".", SandboxLimits::default())?;
//! let manifest = PluginManifest::from_toml(
//!     "name = \"echo\"\nversion = \"0.1.0\"\nentry = \"run\"\ncapabilities = []\n",
//! )?;
//! host.load(manifest, Path::new("plugins/echo.wat"))?;
//!
//! let reply = host.call("echo", "hello").await?;
//! assert_eq!(reply, "hello");
//! # Ok(())
//! # }
//! ```

mod host;
mod limits;
mod manifest;
mod sign;

pub use host::{LoadedPlugin, PluginHost, ALLOC_EXPORT};
pub use limits::SandboxLimits;
pub use manifest::{Capability, PluginManifest};
pub use sign::{public_key_of, sign_module, verify_module};
