//! The host side of the plugin boundary: loading, capability gating, limiting,
//! and calling.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use wasmtime::{
    Caller, Config, Engine, EngineWeak, Error, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder, Trap,
};

use harness_core::path::resolve_within;
use harness_core::{HarnessError, Result};

use crate::limits::SandboxLimits;
use crate::manifest::{Capability, PluginManifest};
use crate::sign::verify_module;

/// Export a plugin must provide so the host has somewhere to put bytes.
///
/// The host never guesses at a plugin's layout: it asks `alloc` for a region and
/// writes only there, which keeps the host out of the business of knowing what a
/// plugin keeps in its data segment.
pub const ALLOC_EXPORT: &str = "alloc";

const MEMORY_EXPORT: &str = "memory";

/// Wall-clock resolution of the epoch interrupt. A call's timeout is rounded up
/// to a whole number of ticks, so this is also the worst-case overshoot.
const EPOCH_TICK: Duration = Duration::from_millis(10);

/// Longest path or URL the host will accept from a plugin. Longer inputs are
/// refused rather than truncated: a silently shortened path is a different path.
const MAX_STRING_BYTES: usize = 8 * 1024;

/// Entries kept in the host's log buffer, so a chatty plugin cannot grow it
/// without bound.
const MAX_LOG_ENTRIES: usize = 1024;

/// Everything a plugin's calls see of the host.
struct HostState {
    /// The plugin's name, so a refusal can say whose it was.
    plugin: String,
    root: PathBuf,
    capabilities: Vec<Capability>,
    max_read_bytes: usize,
    logs: Arc<Mutex<Vec<String>>>,
    http: reqwest::Client,
    limits: StoreLimits,
}

/// A plugin that has been compiled and accepted by the host.
#[derive(Debug)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    module: Module,
}

/// Loads, verifies and calls WebAssembly plugins.
///
/// One host owns one engine and one epoch ticker; each call gets a fresh store,
/// so nothing a call does — memory it grew, logs it wrote, deadlines it reached
/// — can be observed by the next call.
pub struct PluginHost {
    engine: Engine,
    linker: Linker<HostState>,
    /// Canonicalised once so every containment check compares like with like.
    root: PathBuf,
    limits: SandboxLimits,
    http: reqwest::Client,
    logs: Arc<Mutex<Vec<String>>>,
    plugins: Vec<LoadedPlugin>,
    /// Held only for its `Drop`: the ticker has to stop when the host goes.
    _ticker: EpochTicker,
}

impl PluginHost {
    pub fn new(workspace_root: impl Into<PathBuf>, limits: SandboxLimits) -> Result<Self> {
        let mut config = Config::new();
        // Epoch interruption is what makes `timeout` enforceable; see
        // `EpochTicker` for why a ticker thread rather than a fuel budget.
        config.epoch_interruption(true);

        let engine = Engine::new(&config).map_err(|err| {
            HarnessError::Sandbox(format!("failed to create the wasm engine: {err}"))
        })?;

        let http = reqwest::Client::builder()
            // An HTTP request runs on the host side while the plugin is
            // suspended, so nothing else bounds it: without this the plugin's
            // wall-clock limit could be outlived by a hung request.
            .timeout(limits.timeout)
            .build()
            .map_err(|err| {
                HarnessError::Sandbox(format!("failed to build the http client: {err}"))
            })?;

        let root = harness_core::path::canonicalize(&workspace_root.into());
        let linker = build_linker(&engine)?;
        let ticker = EpochTicker::spawn(&engine)?;

        Ok(Self {
            engine,
            linker,
            root,
            limits,
            http,
            logs: Arc::new(Mutex::new(Vec::new())),
            plugins: Vec::new(),
            _ticker: ticker,
        })
    }

    /// The workspace root every guest-supplied path is resolved against.
    pub fn workspace_root(&self) -> &Path {
        &self.root
    }

    /// The limits this host applied to every call.
    pub fn limits(&self) -> &SandboxLimits {
        &self.limits
    }

    /// Loads a `.wasm` or `.wat` module and its manifest.
    ///
    /// `Module::new` sniffs the bytes, so the text format and the binary format
    /// go through the same path; no target for `wasm32-wasip1` is needed to
    /// write a plugin.
    pub fn load(&mut self, manifest: PluginManifest, wasm_path: &Path) -> Result<LoadedPlugin> {
        let bytes = std::fs::read(wasm_path).map_err(|err| {
            HarnessError::Sandbox(format!(
                "could not read plugin module `{}`: {err}",
                wasm_path.display()
            ))
        })?;
        self.accept(manifest, &bytes)
    }

    /// Loads a module only after its Ed25519 signature over the module bytes
    /// verifies against `public_key`. Unsigned plugins are refused here.
    pub fn load_signed(
        &mut self,
        manifest: PluginManifest,
        wasm_path: &Path,
        signature: &[u8],
        public_key: &[u8],
    ) -> Result<LoadedPlugin> {
        let bytes = std::fs::read(wasm_path).map_err(|err| {
            HarnessError::Sandbox(format!(
                "could not read plugin module `{}`: {err}",
                wasm_path.display()
            ))
        })?;
        verify_module(&bytes, signature, public_key).map_err(|err| {
            HarnessError::Sandbox(format!("refusing plugin `{}`: {err}", manifest.name))
        })?;
        self.accept(manifest, &bytes)
    }

    pub fn plugins(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    /// Every line plugins have logged so far, oldest first.
    pub fn logs(&self) -> Vec<String> {
        match self.logs.lock() {
            Ok(logs) => logs.clone(),
            // A poisoned lock means a plugin panicked while the host held it;
            // the logging channel is diagnostics, so the snapshot is not worth
            // failing an otherwise healthy host over.
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn clear_logs(&self) {
        match self.logs.lock() {
            Ok(mut logs) => logs.clear(),
            Err(poisoned) => poisoned.into_inner().clear(),
        }
    }

    /// Calls the plugin's entry point with a string and returns its string.
    ///
    /// The string is an opaque payload: the host does not parse it. A plugin
    /// that wants JSON can be handed JSON.
    pub async fn call(&self, plugin: &str, input: &str) -> Result<String> {
        let loaded = self
            .plugins
            .iter()
            .find(|loaded| loaded.manifest.name == plugin)
            .ok_or_else(|| {
                HarnessError::Sandbox(format!("no plugin named `{plugin}` is loaded"))
            })?;

        let cap = self.limits.max_read_bytes.min(i32::MAX as usize);
        if input.len() > cap {
            return Err(HarnessError::Sandbox(format!(
                "input of {} bytes exceeds the {cap} byte per-call cap",
                input.len()
            )));
        }

        let limits = StoreLimitsBuilder::new()
            .memory_size(self.limits.max_memory_bytes)
            .build();
        let mut store = Store::new(
            &self.engine,
            HostState {
                plugin: loaded.manifest.name.clone(),
                root: self.root.clone(),
                capabilities: loaded.manifest.capabilities.clone(),
                max_read_bytes: self.limits.max_read_bytes,
                logs: Arc::clone(&self.logs),
                http: self.http.clone(),
                limits,
            },
        );
        store.limiter(|state| &mut state.limits);
        // Relative to the current epoch, so concurrent calls on other tasks
        // cannot shorten each other's deadline.
        store.set_epoch_deadline(epoch_ticks(self.limits.timeout));

        let instance = self
            .linker
            .instantiate_async(&mut store, &loaded.module)
            .await
            .map_err(|err| call_error(plugin, err))?;

        let memory = instance
            .get_memory(&mut store, MEMORY_EXPORT)
            .ok_or_else(|| {
                HarnessError::Sandbox(format!(
                    "plugin `{plugin}` does not export `{MEMORY_EXPORT}`"
                ))
            })?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, ALLOC_EXPORT)
            .map_err(|err| {
                HarnessError::Sandbox(format!(
                    "plugin `{plugin}` does not export `{ALLOC_EXPORT}(len: i32) -> i32`: {err}"
                ))
            })?;
        let entry = instance
            .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, &loaded.manifest.entry)
            .map_err(|err| {
                HarnessError::Sandbox(format!(
                    "plugin `{plugin}` does not export `{}(in_ptr, in_len, out_ptr, out_cap) -> i32`: {err}",
                    loaded.manifest.entry
                ))
            })?;

        let input_len = input.len() as i32;
        let in_ptr = alloc
            .call_async(&mut store, input_len)
            .await
            .map_err(|err| call_error(plugin, err))?;
        memory
            .write(&mut store, in_ptr as usize, input.as_bytes())
            .map_err(|err| {
                HarnessError::Sandbox(format!(
                    "plugin `{plugin}` returned a bad input buffer: {err}"
                ))
            })?;

        let out_ptr = alloc
            .call_async(&mut store, cap as i32)
            .await
            .map_err(|err| call_error(plugin, err))?;
        let written = entry
            .call_async(&mut store, (in_ptr, input_len, out_ptr, cap as i32))
            .await
            .map_err(|err| call_error(plugin, err))?;

        let written = usize::try_from(written).map_err(|_| {
            HarnessError::Sandbox(format!(
                "plugin `{plugin}` reported failure for a {input_len} byte request"
            ))
        })?;
        if written > cap {
            return Err(HarnessError::Sandbox(format!(
                "plugin `{plugin}` claimed to write {written} bytes into a {cap} byte buffer"
            )));
        }

        let mut reply = vec![0u8; written];
        memory
            .read(&store, out_ptr as usize, &mut reply)
            .map_err(|err| {
                HarnessError::Sandbox(format!(
                    "plugin `{plugin}` returned an unreadable buffer: {err}"
                ))
            })?;
        String::from_utf8(reply).map_err(|err| {
            HarnessError::Sandbox(format!("plugin `{plugin}` returned invalid UTF-8: {err}"))
        })
    }

    fn accept(&mut self, manifest: PluginManifest, bytes: &[u8]) -> Result<LoadedPlugin> {
        manifest.validate()?;
        if self
            .plugins
            .iter()
            .any(|loaded| loaded.manifest.name == manifest.name)
        {
            return Err(HarnessError::Sandbox(format!(
                "a plugin named `{}` is already loaded",
                manifest.name
            )));
        }
        let module = Module::new(&self.engine, bytes).map_err(|err| {
            HarnessError::Sandbox(format!(
                "plugin `{}` does not compile: {err}",
                manifest.name
            ))
        })?;
        self.plugins.push(LoadedPlugin {
            manifest: manifest.clone(),
            module: module.clone(),
        });
        Ok(LoadedPlugin { manifest, module })
    }
}

/// Number of epoch ticks that add up to `timeout`, plus one so the deadline is
/// never already expired when the call starts.
fn epoch_ticks(timeout: Duration) -> u64 {
    let tick = EPOCH_TICK.as_millis().max(1) as u64;
    (timeout.as_millis() as u64 / tick).max(1) + 1
}

/// Turns a wasm trap into an error that says what actually went wrong.
///
/// The distinct case is the interrupt: telling a caller that a plugin "was
/// interrupted" is very different from "the plugin trapped".
fn call_error(plugin: &str, err: Error) -> HarnessError {
    if let Some(Trap::Interrupt) = err.downcast_ref::<Trap>() {
        return HarnessError::Sandbox(format!(
            "plugin `{plugin}` was interrupted: it exceeded its wall-clock limit"
        ));
    }
    HarnessError::Sandbox(format!("plugin `{plugin}` failed: {err:#}"))
}

/// Advances the engine's epoch on a timer. Epoch interruption is the wall-clock
/// mechanism here rather than a fuel budget because `SandboxLimits::timeout`
/// promises a duration: fuel counts instructions, so the same budget would be
/// seconds or milliseconds depending on the machine.
struct EpochTicker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl EpochTicker {
    fn spawn(engine: &Engine) -> Result<Self> {
        // A weak handle on purpose: the ticker must not be the reason an engine
        // stays alive after the host that owns it is gone.
        let monitor: EngineWeak = engine.weak();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("harness-sandbox-epoch".to_string())
            .spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    match monitor.upgrade() {
                        Some(engine) => engine.increment_epoch(),
                        None => break,
                    }
                    std::thread::sleep(EPOCH_TICK);
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // At most one tick of latency, so joining here is bounded.
            let _ = thread.join();
        }
    }
}

/// Wires up the `harness` module.
///
/// Every import is defined here and checks the manifest itself. That is the
/// whole point of the design: a capability checked by the plugin would not be a
/// capability, so each host function refuses on its own authority and fails the
/// call rather than returning something a plugin might mistake for data.
fn build_linker(engine: &Engine) -> Result<Linker<HostState>> {
    let mut linker: Linker<HostState> = Linker::new(engine);

    linker
        .func_wrap_async(
            "harness",
            "log",
            |mut caller: Caller<'_, HostState>, (ptr, len): (i32, i32)| {
                Box::new(async move {
                    // Unconditional: a plugin has to be able to say why it is
                    // unhappy even with no grants at all.
                    let line = read_string(&mut caller, ptr, len, MAX_STRING_BYTES)?;
                    if let Ok(mut logs) = caller.data().logs.lock() {
                        if logs.len() < MAX_LOG_ENTRIES {
                            logs.push(line);
                        }
                    }
                    Ok::<(), Error>(())
                })
            },
        )
        .map_err(link_error("log"))?;

    linker
        .func_wrap_async(
            "harness",
            "read_file",
            |mut caller: Caller<'_, HostState>,
             (path_ptr, path_len, dest_ptr, dest_cap): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let path = read_string(&mut caller, path_ptr, path_len, MAX_STRING_BYTES)?;
                    let (root, cap) = {
                        let state = caller.data();
                        require(state, Capability::FileRead)?;
                        (
                            state.root.clone(),
                            state.max_read_bytes.min(dest_cap.max(0) as usize),
                        )
                    };
                    let resolved = resolve_guest_path(&root, &path)?;

                    // A missing or unreadable file is an ordinary outcome the
                    // plugin is allowed to handle, unlike a refusal.
                    let contents = match std::fs::read(&resolved) {
                        Ok(contents) => contents,
                        Err(_) => return Ok(-1),
                    };
                    let copied = contents.len().min(cap);
                    write_guest(&mut caller, dest_ptr, &contents[..copied])?;
                    Ok(copied as i64)
                })
            },
        )
        .map_err(link_error("read_file"))?;

    linker
        .func_wrap_async(
            "harness",
            "write_file",
            |mut caller: Caller<'_, HostState>,
             (path_ptr, path_len, data_ptr, data_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let path = read_string(&mut caller, path_ptr, path_len, MAX_STRING_BYTES)?;
                    let (root, cap) = {
                        let state = caller.data();
                        require(state, Capability::FileWrite)?;
                        (state.root.clone(), state.max_read_bytes)
                    };
                    if data_len < 0 || data_len as usize > cap {
                        return Err(Error::msg(format!(
                            "plugin passed {data_len} bytes to write, over the {cap} byte cap"
                        )));
                    }
                    let resolved = resolve_guest_path(&root, &path)?;
                    let data = read_bytes(&mut caller, data_ptr, data_len)?;

                    match std::fs::write(&resolved, &data) {
                        Ok(()) => Ok(data.len() as i64),
                        Err(_) => Ok(-1),
                    }
                })
            },
        )
        .map_err(link_error("write_file"))?;

    linker
        .func_wrap_async(
            "harness",
            "http_get",
            |mut caller: Caller<'_, HostState>,
             (url_ptr, url_len, dest_ptr, dest_cap): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let url = read_string(&mut caller, url_ptr, url_len, MAX_STRING_BYTES)?;
                    // The client is cloned out before the await: holding a
                    // borrow of the store across a suspension point would tie
                    // the future to data the guest could reach.
                    let (client, cap) = {
                        let state = caller.data();
                        require(state, Capability::NetworkAccess)?;
                        (
                            state.http.clone(),
                            state.max_read_bytes.min(dest_cap.max(0) as usize),
                        )
                    };
                    // Parsed here rather than handed to `reqwest` as text, so
                    // the scheme check cannot be skipped by a request builder
                    // that would rather be lenient.
                    let parsed = resolve_url(&url)?;

                    let mut response = match client.get(parsed).send().await {
                        Ok(response) => response,
                        Err(_) => return Ok(-1),
                    };

                    let mut body = Vec::new();
                    loop {
                        let chunk = match response.chunk().await {
                            Ok(Some(chunk)) => chunk,
                            Ok(None) => break,
                            Err(_) => return Ok(-1),
                        };
                        let room = cap.saturating_sub(body.len());
                        body.extend_from_slice(&chunk[..room.min(chunk.len())]);
                        if body.len() >= cap {
                            break;
                        }
                    }

                    write_guest(&mut caller, dest_ptr, &body)?;
                    Ok(body.len() as i64)
                })
            },
        )
        .map_err(link_error("http_get"))?;

    Ok(linker)
}

fn link_error(name: &str) -> impl Fn(Error) -> HarnessError + '_ {
    move |err| HarnessError::Sandbox(format!("failed to define harness.{name}: {err}"))
}

/// Refuses a call the manifest did not grant, on the host's authority.
fn require(state: &HostState, capability: Capability) -> wasmtime::Result<()> {
    if state.capabilities.contains(&capability) {
        return Ok(());
    }
    let function = capability.host_function().unwrap_or("no host function");
    Err(Error::msg(format!(
        "plugin `{}` is not granted `{}`, so `{function}` is refused",
        state.plugin,
        capability.as_str()
    )))
}

/// Resolves a guest-supplied path against the workspace root.
///
/// `resolve_within` is the single guard every file access goes through: it
/// resolves `..` and symlinks and rejects anything that lands outside the root.
/// Capabilities say which *kinds* of access a plugin can have; this says *where*.
fn resolve_guest_path(root: &Path, path: &str) -> wasmtime::Result<PathBuf> {
    resolve_within(root, Path::new(path))
        .map_err(|err| Error::msg(format!("refused guest path `{path}`: {err}")))
}

/// Accepts only absolute `http`/`https` URLs.
///
/// `reqwest` would happily follow a `file://` URL out of the workspace, so the
/// scheme is checked here rather than trusted to the client's defaults.
fn resolve_url(url: &str) -> wasmtime::Result<reqwest::Url> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|err| Error::msg(format!("guest url `{url}` is not a url: {err}")))?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed),
        other => Err(Error::msg(format!(
            "refused guest url `{url}`: scheme `{other}` is not http or https"
        ))),
    }
}

fn guest_memory(caller: &mut Caller<'_, HostState>) -> wasmtime::Result<Memory> {
    caller
        .get_export(MEMORY_EXPORT)
        .and_then(|export| export.into_memory())
        .ok_or_else(|| Error::msg(format!("plugin does not export `{MEMORY_EXPORT}`")))
}

fn read_bytes(caller: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> wasmtime::Result<Vec<u8>> {
    if ptr < 0 || len < 0 {
        return Err(Error::msg(format!(
            "plugin passed a negative pointer or length ({ptr}, {len})"
        )));
    }
    let memory = guest_memory(caller)?;
    let start = ptr as usize;
    let size = memory.data_size(&*caller);
    let end = start
        .checked_add(len as usize)
        .ok_or_else(|| Error::msg("plugin passed a pointer and length that overflow"))?;
    if end > size {
        return Err(Error::msg(format!(
            "plugin read [{start}, {end}) which is outside its {size}-byte memory"
        )));
    }

    let mut buffer = vec![0u8; len as usize];
    memory
        .read(&mut *caller, start, &mut buffer)
        .map_err(|err| Error::msg(format!("could not read plugin memory: {err}")))?;
    Ok(buffer)
}

/// Reads a guest string, refusing anything longer than `cap` rather than
/// truncating it: a shortened path or URL is a different path or URL.
fn read_string(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    len: i32,
    cap: usize,
) -> wasmtime::Result<String> {
    if len < 0 || len as usize > cap {
        return Err(Error::msg(format!(
            "plugin passed a {len} byte string, over the {cap} byte limit"
        )));
    }
    let bytes = read_bytes(caller, ptr, len)?;
    String::from_utf8(bytes)
        .map_err(|_| Error::msg("plugin passed a string that is not valid UTF-8"))
}

fn write_guest(
    caller: &mut Caller<'_, HostState>,
    dest_ptr: i32,
    data: &[u8],
) -> wasmtime::Result<()> {
    if dest_ptr < 0 {
        return Err(Error::msg(format!(
            "plugin passed a negative destination pointer ({dest_ptr})"
        )));
    }
    let memory = guest_memory(caller)?;
    memory
        .write(&mut *caller, dest_ptr as usize, data)
        .map_err(|err| {
            Error::msg(format!(
                "plugin's destination buffer at {dest_ptr} could not hold {} bytes: {err}",
                data.len()
            ))
        })
}
