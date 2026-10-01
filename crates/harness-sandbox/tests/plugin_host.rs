//! End-to-end tests against the hand-written plugins in `plugins/`.
//!
//! Every test drives a real module through a real `PluginHost`: the point of the
//! crate is the boundary, so nothing here is worth testing with a stub.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use harness_sandbox::{
    public_key_of, sign_module, Capability, PluginHost, PluginManifest, SandboxLimits,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The plugins live at the workspace root, two levels above this crate.
fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

fn manifest(name: &str) -> PluginManifest {
    let path = plugins_dir().join(format!("{name}.plugin.toml"));
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));
    PluginManifest::from_toml(&text).expect("example manifest should parse")
}

fn module_path(name: &str) -> PathBuf {
    plugins_dir().join(format!("{name}.wat"))
}

fn host(root: &Path) -> PluginHost {
    PluginHost::new(root, SandboxLimits::default()).expect("host should start")
}

/// Loads a plugin using the capabilities its checked-in manifest declares.
fn load(host: &mut PluginHost, name: &str) {
    host.load(manifest(name), &module_path(name))
        .unwrap_or_else(|err| panic!("could not load {name}: {err}"));
}

/// Loads a plugin with its capabilities stripped, to exercise the refusal path
/// against the very same module.
fn load_ungranted(host: &mut PluginHost, name: &str) {
    let mut stripped = manifest(name);
    stripped.capabilities.clear();
    host.load(stripped, &module_path(name))
        .unwrap_or_else(|err| panic!("could not load {name}: {err}"));
}

fn workspace_with_cargo_toml() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("temp dir");
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[workspace]\nname = \"snoop-target\"\n",
    )
    .expect("write fixture");
    tmp
}

const EXAMPLES: [&str; 8] = [
    "echo",
    "logger",
    "file_snoop",
    "file_dump",
    "path_escape",
    "http_fetch",
    "spin",
    "memory_hog",
];

#[tokio::test]
async fn every_example_plugin_loads_with_its_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = host(tmp.path());

    for name in EXAMPLES {
        load(&mut host, name);
    }

    let names: Vec<&str> = host
        .plugins()
        .iter()
        .map(|plugin| plugin.manifest.name.as_str())
        .collect();
    assert_eq!(names, EXAMPLES);

    assert!(host.plugins()[0].manifest.capabilities.is_empty());
    assert_eq!(
        host.plugins()[2].manifest.capabilities,
        vec![Capability::FileRead]
    );
    assert_eq!(
        host.plugins()[3].manifest.capabilities,
        vec![Capability::FileWrite]
    );
    assert_eq!(
        host.plugins()[4].manifest.capabilities,
        vec![Capability::FileRead, Capability::FileWrite]
    );
    assert_eq!(
        host.plugins()[5].manifest.capabilities,
        vec![Capability::NetworkAccess]
    );
}

#[tokio::test]
async fn a_repeated_plugin_name_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = host(tmp.path());
    load(&mut host, "echo");

    let err = host
        .load(manifest("echo"), &module_path("echo"))
        .unwrap_err();
    assert!(err.to_string().contains("already loaded"), "{err}");
}

#[tokio::test]
async fn calling_an_unknown_plugin_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let host = host(tmp.path());

    let err = host.call("ghost", "x").await.unwrap_err();
    assert!(err.to_string().contains("ghost"), "{err}");
}

#[tokio::test]
async fn echo_round_trips_a_string_through_the_host() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = host(tmp.path());
    load(&mut host, "echo");

    assert_eq!(
        host.call("echo", "hello, sandbox").await.unwrap(),
        "hello, sandbox"
    );
    assert_eq!(host.call("echo", "").await.unwrap(), "");
    assert_eq!(
        host.call("echo", "日本語 also works").await.unwrap(),
        "日本語 also works"
    );

    let long = "abcdefghij".repeat(600);
    let reply = host.call("echo", &long).await.unwrap();
    assert_eq!(reply.len(), long.len());
    assert_eq!(reply, long);
}

#[tokio::test]
async fn an_input_over_the_per_call_cap_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let limits = SandboxLimits {
        max_read_bytes: 256,
        ..SandboxLimits::default()
    };
    let mut host = PluginHost::new(tmp.path(), limits).unwrap();
    load(&mut host, "echo");

    assert_eq!(
        host.call("echo", &"a".repeat(256)).await.unwrap().len(),
        256
    );

    let err = host.call("echo", &"a".repeat(257)).await.unwrap_err();
    assert!(err.to_string().contains("per-call cap"), "{err}");
}

#[tokio::test]
async fn logger_calls_the_host_import_and_the_host_saw_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = host(tmp.path());
    load(&mut host, "logger");

    assert!(host.logs().is_empty());

    let reply = host.call("logger", "agent started").await.unwrap();
    assert_eq!(reply, "hello: agent started");
    assert_eq!(host.logs(), vec!["agent started".to_string()]);

    // A second call keeps appending rather than replacing.
    assert_eq!(host.call("logger", "again").await.unwrap(), "hello: again");
    assert_eq!(
        host.logs(),
        vec!["agent started".to_string(), "again".to_string()]
    );

    host.clear_logs();
    assert!(host.logs().is_empty());
}

#[tokio::test]
async fn file_snoop_reads_the_workspace_when_file_read_is_granted() {
    let tmp = workspace_with_cargo_toml();
    let mut host = host(tmp.path());
    load(&mut host, "file_snoop");

    let reply = host.call("file_snoop", "").await.unwrap();
    assert_eq!(reply, "[workspace]\nname = \"snoop-target\"\n");
}

#[tokio::test]
async fn file_snoop_is_refused_without_the_capability() {
    let tmp = workspace_with_cargo_toml();
    let mut host = host(tmp.path());
    load_ungranted(&mut host, "file_snoop");

    let err = host.call("file_snoop", "").await.unwrap_err();
    let text = err.to_string();

    assert!(
        text.contains("file_read"),
        "should name the capability: {text}"
    );
    assert!(
        text.contains("refused"),
        "should say it was refused: {text}"
    );
    assert!(
        !text.contains("snoop-target"),
        "no file contents may leak into the error: {text}"
    );
}

#[tokio::test]
async fn file_dump_writes_into_the_workspace_when_file_write_is_granted() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = host(tmp.path());
    load(&mut host, "file_dump");

    let reply = host.call("file_dump", "written by a plugin").await.unwrap();
    assert_eq!(reply, "written by a plugin");
    assert_eq!(
        fs::read_to_string(tmp.path().join("dump.txt")).unwrap(),
        "written by a plugin"
    );
}

#[tokio::test]
async fn file_dump_is_refused_without_the_capability() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = host(tmp.path());
    load_ungranted(&mut host, "file_dump");

    let err = host
        .call("file_dump", "should never land")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("file_write"), "{err}");
    assert!(!tmp.path().join("dump.txt").exists());
}

#[tokio::test]
async fn a_path_that_escapes_the_workspace_is_refused_even_with_file_read() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("workspace");
    fs::create_dir_all(&root).unwrap();
    // The file the plugin is aiming for really exists, one level above the root.
    fs::write(tmp.path().join("escape.txt"), "top-secret").unwrap();

    let mut host = host(&root);
    // The manifest grants `file_read` *and* `file_write` on purpose.
    load(&mut host, "path_escape");

    let err = host.call("path_escape", "").await.unwrap_err();
    let text = err.to_string();

    assert!(
        text.contains("path escapes the workspace root"),
        "should be the containment guard that refused: {text}"
    );
    assert!(
        !text.contains("top-secret"),
        "no file contents may leak: {text}"
    );
}

#[tokio::test]
async fn an_infinite_loop_is_interrupted_at_the_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let limits = SandboxLimits {
        timeout: Duration::from_secs(1),
        ..SandboxLimits::default()
    };
    let mut host = PluginHost::new(tmp.path(), limits).unwrap();
    load(&mut host, "spin");

    let started = Instant::now();
    let err = host.call("spin", "").await.unwrap_err();
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_millis(500),
        "the plugin must actually have run: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the timeout must interrupt, not hang: {elapsed:?}"
    );
    assert!(
        err.to_string().contains("was interrupted"),
        "the error should say it was interrupted: {err}"
    );

    // Interruption is per call: the host is still usable afterwards.
    load(&mut host, "echo");
    assert_eq!(
        host.call("echo", "still alive").await.unwrap(),
        "still alive"
    );
}

#[tokio::test]
async fn the_memory_limit_stops_a_runaway_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let ceiling = 2 * 1024 * 1024;
    let limits = SandboxLimits {
        max_memory_bytes: ceiling,
        ..SandboxLimits::default()
    };
    let mut host = PluginHost::new(tmp.path(), limits).unwrap();
    load(&mut host, "memory_hog");

    let reply = host.call("memory_hog", "").await.unwrap();
    let pages = i32::from_le_bytes(reply.as_bytes().try_into().unwrap());

    assert_eq!(
        pages as usize * 64 * 1024,
        ceiling,
        "the plugin should have grown to exactly the ceiling and no further"
    );
}

#[tokio::test]
async fn signed_modules_load_and_tampered_ones_do_not() {
    let tmp = tempfile::tempdir().unwrap();
    let key = [7u8; 32];
    let public_key = public_key_of(&key);
    let path = module_path("echo");
    let bytes = fs::read(&path).unwrap();
    let signature = sign_module(&bytes, &key).unwrap();

    let mut host = host(tmp.path());
    host.load_signed(manifest("echo"), &path, &signature, &public_key)
        .expect("a correctly signed module should load");
    assert_eq!(host.call("echo", "signed").await.unwrap(), "signed");

    // One byte appended after signing must break verification.
    let tampered = tmp.path().join("tampered.wat");
    let mut text = String::from_utf8(bytes).unwrap();
    text.push_str("\n;; appended after signing\n");
    fs::write(&tampered, text).unwrap();
    let err = host
        .load_signed(manifest("echo"), &tampered, &signature, &public_key)
        .unwrap_err();
    assert!(err.to_string().contains("signature"), "{err}");

    // A signature that is valid, but not from the key we were told to trust.
    let err = host
        .load_signed(
            manifest("echo"),
            &path,
            &signature,
            &public_key_of(&[9u8; 32]),
        )
        .unwrap_err();
    assert!(err.to_string().contains("signature"), "{err}");
}

#[tokio::test]
async fn http_get_is_refused_without_the_capability() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = host(tmp.path());
    load_ungranted(&mut host, "http_fetch");

    // Nothing is listening on port 1; the refusal must happen before any
    // connection is even attempted.
    let err = host
        .call("http_fetch", "http://127.0.0.1:1/")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("network_access"), "{err}");
}

#[tokio::test]
async fn http_get_refuses_schemes_that_are_not_http() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = host(tmp.path());
    load(&mut host, "http_fetch");

    let err = host
        .call("http_fetch", "file:///C:/Windows/win.ini")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("scheme"), "{err}");

    let err = host.call("http_fetch", "not a url").await.unwrap_err();
    assert!(err.to_string().contains("not a url"), "{err}");
}

#[tokio::test]
async fn http_get_fetches_from_a_server_over_the_real_network_stack() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a loopback port");
    let address = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept one connection");
        let mut request = [0u8; 1024];
        let _ = socket.read(&mut request).await;
        let body = "sandbox-ok";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.shutdown().await;
    });

    let tmp = tempfile::tempdir().unwrap();
    let mut host = host(tmp.path());
    load(&mut host, "http_fetch");

    let reply = host
        .call("http_fetch", &format!("http://{address}/plugin"))
        .await
        .unwrap();
    assert_eq!(reply, "sandbox-ok");

    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_serves_calls_concurrently_without_leaking_between_them() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = host(tmp.path());
    load(&mut host, "echo");
    load(&mut host, "logger");
    let host = std::sync::Arc::new(host);

    let mut tasks = Vec::new();
    for index in 0..8 {
        let host = std::sync::Arc::clone(&host);
        tasks.push(tokio::spawn(async move {
            let text = format!("call-{index}");
            assert_eq!(host.call("echo", &text).await.unwrap(), text);
            assert_eq!(
                host.call("logger", &text).await.unwrap(),
                format!("hello: {text}")
            );
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }

    // One store per call is what makes this safe; the only shared mutable state
    // is the log buffer, which must have collected one line per call.
    let mut logs = host.logs();
    logs.sort();
    let expected: Vec<String> = (0..8).map(|index| format!("call-{index}")).collect();
    assert_eq!(logs, expected);
}

#[tokio::test]
async fn a_plugin_cannot_import_anything_outside_the_harness_module() {
    let tmp = tempfile::tempdir().unwrap();
    let rogue = tmp.path().join("rogue.wat");
    // A module that reaches for the ambient filesystem the way a WASI plugin
    // would. The host only defines the `harness` imports, so this cannot link.
    fs::write(
        &rogue,
        r#"
(module
  (import "wasi_snapshot_preview1" "fd_write"
    (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 0))
  (func (export "run") (param i32 i32 i32 i32) (result i32) (i32.const 0)))
"#,
    )
    .unwrap();

    let manifest = PluginManifest::from_toml(
        "name = \"rogue\"\nversion = \"0.1.0\"\nentry = \"run\"\ncapabilities = []\n",
    )
    .unwrap();
    let mut host = host(tmp.path());
    // It compiles -- a module is just bytes until something links it.
    host.load(manifest, &rogue).unwrap();

    let err = host.call("rogue", "").await.unwrap_err();
    assert!(err.to_string().contains("fd_write"), "{err}");
}
