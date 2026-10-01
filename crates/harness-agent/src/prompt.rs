//! Assembly of the system prompt: the agent's declared body plus a generated
//! context block.
//!
//! The declarative layer fixes *who* an agent is (`.agent.md`), but a model
//! cannot see the machine it is running on. Left to guess it emits POSIX paths
//! and `rm -rf` on Windows, re-derives today's date from its training data, and
//! never reads the workspace's own `AGENTS.md`. The block is generated here
//! rather than written into every agent file so it cannot drift from the
//! environment it describes, and it stays short: it is a context injection, not
//! a second prompt.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use harness_core::ToolsConfig;

/// The file a workspace uses for project-level instructions to a coding agent.
pub const AGENTS_FILE: &str = "AGENTS.md";

/// How much of `AGENTS.md` is injected.
///
/// It is context, not the whole document: an enormous file would crowd out the
/// conversation it is meant to inform, and the model can still read the rest
/// with a tool.
pub const AGENTS_MAX_BYTES: usize = 8 * 1024;

/// How much of a manifest is scanned to recover the project's name.
const MANIFEST_SCAN_BYTES: usize = 16 * 1024;

/// How many dependency versions the block reports before it stops.
///
/// The line is context, not an inventory: enough to anchor the versions that
/// change what the model writes, without crowding the conversation.
const INSTALLED_MAX: usize = 24;

/// How much of a lockfile is scanned for dependency versions.
const INSTALLED_SCAN_BYTES: usize = 1024 * 1024;

/// The marker files that identify a project, cheapest and most specific first.
const PROJECT_MARKERS: &[(&str, &str)] = &[
    ("Cargo.toml", "Rust"),
    ("package.json", "JavaScript/TypeScript"),
    ("pyproject.toml", "Python"),
    ("go.mod", "Go"),
    ("pom.xml", "Java"),
];

/// Everything the generated context block reports about where the agent is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    pub workspace_root: PathBuf,
    /// `std::env::consts::OS`, e.g. `windows`.
    pub os: String,
    /// `std::env::consts::ARCH`, e.g. `x86_64`.
    pub arch: String,
    /// The shell the `shell` tool invokes, as a human-readable phrase.
    pub shell: String,
    /// Today's date as `YYYY-MM-DD`.
    pub date: String,
    /// One line naming the project, when a marker file identifies it.
    pub project: Option<String>,
    /// `name version` for each dependency, as it exists on disk.
    ///
    /// A declared range is not what is installed: a manifest asking for
    /// `^1.6.4` resolves to whatever the lockfile or `node_modules` holds, and
    /// a model that reads the range alone describes behaviour the project does
    /// not have.
    pub installed: Vec<String>,
    /// The workspace-root `AGENTS.md`, already bounded to [`AGENTS_MAX_BYTES`].
    pub agents_md: Option<String>,
}

impl Environment {
    /// Reads everything the block reports from the workspace and the process.
    ///
    /// Every read is best-effort: a workspace with no `AGENTS.md` and no
    /// recognisable manifest still gets a block naming its root, its OS and the
    /// shell, which is the part that changes what the model does.
    pub fn detect(workspace_root: &Path, shell: &str) -> Self {
        Self {
            workspace_root: workspace_root.to_path_buf(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            shell: shell.to_string(),
            date: today(),
            project: detect_project(workspace_root),
            installed: installed_dependencies(workspace_root),
            agents_md: read_bounded(&workspace_root.join(AGENTS_FILE), AGENTS_MAX_BYTES),
        }
    }
}

/// One skill's cheap metadata: what the model sees before deciding to activate
/// it.
///
/// The body is deliberately absent. Progressive disclosure is the whole point
/// of the skill layer: the index is paid on every request, the instructions only
/// when the agent engages the skill, which it does by reading `source_path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    /// The `SKILL.md` the full instructions live in.
    pub source_path: PathBuf,
}

/// Renders the declared body followed by the generated context block.
///
/// The body is emitted first and verbatim, so an agent file remains the thing
/// that defines the agent; the block only adds what the file cannot know.
/// `skills` is the index of the skills the agent declared — names and
/// descriptions only, never their bodies.
pub fn assemble(body: &str, env: &Environment, skills: &[SkillSummary]) -> String {
    let mut prompt = String::with_capacity(body.len() + 1024);
    prompt.push_str(body.trim_end());
    prompt.push_str("\n\n## Environment\n\n");
    // Writing into a `String` cannot fail; the results are dropped rather than
    // unwrapped so the crate keeps its no-`unwrap` rule.
    let _ = writeln!(
        prompt,
        "- Workspace root: `{}`",
        env.workspace_root.display()
    );
    let _ = writeln!(prompt, "- Operating system: {} ({})", env.os, env.arch);
    let _ = writeln!(prompt, "- Shell for the `shell` tool: {}", env.shell);
    let _ = writeln!(prompt, "- Date: {}", env.date);
    let _ = writeln!(
        prompt,
        "- Project: {}",
        env.project.as_deref().unwrap_or("not recognised")
    );
    let installed = if env.installed.is_empty() {
        "not detected".to_string()
    } else {
        env.installed.join(", ")
    };
    let _ = writeln!(prompt, "- Installed dependencies: {installed}");
    prompt.push_str(platform_note(&env.os));

    if !skills.is_empty() {
        prompt.push_str(&skills_section(skills));
    }

    prompt.push_str(
        "\n\n## Working agreement\n\n\
         - When the request is a question, a critique or a review, answer it from what you \
         read and change nothing.\n\
         - Only modify files when the request asks for a change, and say which files you \
         changed.\n\
         - A command exiting 0, a file existing and a string matching are evidence that \
         output was produced, not that a feature works.\n\
         - Before you claim a feature works, exercise the path that would break if it did \
         not, and quote what you observed.\n\
         - When only production evidence exists, say the feature is unverified rather than \
         calling it working.\n",
    );

    if let Some(instructions) = &env.agents_md {
        prompt.push_str("\n## Project instructions (AGENTS.md)\n\n");
        prompt.push_str(instructions.trim_end());
        prompt.push('\n');
    }

    prompt
}

/// The skills index appended to the prompt.
///
/// Names and descriptions only. Each entry also names the `SKILL.md` so the
/// agent can pull the body in with `read_file` at the moment it engages the
/// skill — that read is the activation step, and it is what keeps the body off
/// every request that does not need it.
fn skills_section(skills: &[SkillSummary]) -> String {
    let mut text = String::from(
        "\n\n## Skills\n\n\
         These skills are available to you. Each entry is an index only: the full \
         instructions are not loaded yet. When a task matches a skill, read its \
         `SKILL.md` with `read_file` and follow it before you start.\n\n",
    );
    for skill in skills {
        let _ = writeln!(text, "- `{}`: {}", skill.name, skill.description);
        let _ = writeln!(text, "  instructions: `{}`", skill.source_path.display());
    }
    text
}

/// One line of platform-specific guidance, derived from the OS name.
fn platform_note(os: &str) -> &'static str {
    if os == "windows" {
        "- This is Windows: `shell` commands run through the shell above, so use backslash \
         paths and that shell's built-ins. POSIX-only commands and paths (`rm -rf`, `/tmp`, \
         `~`) fail here.\n"
    } else {
        "- Commands run through the shell above; keep paths and built-ins consistent with it.\n"
    }
}

/// The shell the `shell` tool will use, described for the model.
///
/// This mirrors the tool's own selection (`harness-tools`' `shell_invocation`),
/// which is private to that crate: the prompt must name the shell the tool will
/// actually spawn, or the guidance is worse than none.
pub fn shell_description(config: &ToolsConfig) -> String {
    if let Some(program) = config
        .shell_program
        .as_deref()
        .map(str::trim)
        .filter(|program| !program.is_empty())
    {
        return format!("`{program} -c` (configured)");
    }
    if which("sh").is_some() {
        return "`sh -c`".to_string();
    }
    if cfg!(windows) {
        "`cmd /C`".to_string()
    } else {
        "`sh -c`".to_string()
    }
}

/// Today's date in UTC, as `YYYY-MM-DD`.
pub fn today() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    format_date(seconds)
}

/// Formats a Unix timestamp (UTC) as `YYYY-MM-DD`.
pub fn format_date(unix_seconds: u64) -> String {
    let (year, month, day) = civil_from_days((unix_seconds / 86_400) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since 1970-01-01 to a civil `(year, month, day)`.
///
/// Howard Hinnant's `civil_from_days`. It is inlined rather than pulled from a
/// calendar crate because the harness does not otherwise depend on one, and a
/// date line does not justify adding a dependency.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// One line naming the project, from the first marker file that exists.
fn detect_project(root: &Path) -> Option<String> {
    for (marker, language) in PROJECT_MARKERS {
        let path = root.join(marker);
        if !path.is_file() {
            continue;
        }
        return Some(match manifest_name(&path) {
            Some(name) => format!("{language} project `{name}` ({marker})"),
            None => format!("{language} project ({marker})"),
        });
    }
    None
}

/// The `version` of a package as its own `package.json` declares it.
///
/// Parsed rather than line-scanned: a `node_modules` manifest is not
/// guaranteed to be pretty-printed, and the first `:` in a minified one belongs
/// to whichever key happens to come first.
fn node_package_version(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let json = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    json.get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// The `name` value of a manifest, when its first `name` key is a plain string.
fn manifest_name(path: &Path) -> Option<String> {
    let text = read_bounded(path, MANIFEST_SCAN_BYTES)?;
    text.lines().find_map(name_value)
}

/// `name = "x"` (TOML) or `"name": "x"` (JSON) on one line, as `x`.
fn name_value(line: &str) -> Option<String> {
    key_value(line, "name")
}

/// `key = "x"` (TOML) or `"key": "x"` (JSON) on one line, as `x`.
fn key_value(line: &str, key: &str) -> Option<String> {
    let (found, rest) = line.split_once(['=', ':'])?;
    if found.trim().trim_matches('"') != key {
        return None;
    }
    let value = rest
        .trim()
        .trim_end_matches(',')
        .trim()
        .trim_matches('"')
        .trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// The dependencies that are actually on disk, as `name version`.
///
/// The manifest supplies the names and the lockfile or `node_modules` supplies
/// the version, because the declared range is not the installed one: a model
/// that reads `^1.6.4` alone reaches for the 2.0 documentation and describes
/// behaviour the project does not have. Everything here is best-effort — a
/// missing or malformed manifest yields an empty list rather than an error,
/// since a broken dependency line must never cost the agent its prompt.
fn installed_dependencies(root: &Path) -> Vec<String> {
    if root.join("Cargo.toml").is_file() {
        let rust = rust_dependencies(root);
        if !rust.is_empty() {
            return rust;
        }
    }
    if root.join("package.json").is_file() {
        return node_dependencies(root);
    }
    Vec::new()
}

/// Rust dependencies, named by `Cargo.toml` and versioned by `Cargo.lock`.
fn rust_dependencies(root: &Path) -> Vec<String> {
    let Some(manifest) = read_bounded(&root.join("Cargo.toml"), INSTALLED_SCAN_BYTES) else {
        return Vec::new();
    };
    let Some(lock) = read_bounded(&root.join("Cargo.lock"), INSTALLED_SCAN_BYTES) else {
        return Vec::new();
    };
    let versions = lock_versions(&lock);
    let mut installed = Vec::new();
    for (name, requirement) in manifest_dependencies(&manifest) {
        let Some(version) = locked_version(&versions, &name, requirement.as_deref()) else {
            continue;
        };
        installed.push(format!("{name} {version}"));
        if installed.len() >= INSTALLED_MAX {
            break;
        }
    }
    installed
}

/// Node dependencies, named by `package.json` and versioned by the copy of each
/// package that `node_modules` holds.
///
/// A name whose on-disk copy cannot be read is dropped rather than reported at
/// its declared range: the whole point of the line is to state what is there.
fn node_dependencies(root: &Path) -> Vec<String> {
    let Some(manifest) = std::fs::read_to_string(root.join("package.json")).ok() else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&manifest) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for key in ["dependencies", "devDependencies"] {
        let Some(table) = json.get(key).and_then(serde_json::Value::as_object) else {
            continue;
        };
        for name in table.keys() {
            if !names.iter().any(|seen| seen == name) {
                names.push(name.clone());
            }
        }
    }
    let mut installed = Vec::new();
    for name in names {
        let path = root.join("node_modules").join(&name).join("package.json");
        let Some(version) = node_package_version(&path) else {
            continue;
        };
        installed.push(format!("{name} {version}"));
        if installed.len() >= INSTALLED_MAX {
            break;
        }
    }
    installed
}

/// The dependencies a `Cargo.toml` declares, in the order it declares them,
/// each with the version requirement it states.
fn manifest_dependencies(manifest: &str) -> Vec<(String, Option<String>)> {
    let mut dependencies = Vec::new();
    let mut in_dependencies = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if let Some(section) = trimmed.strip_prefix('[') {
            let section = section.trim_end_matches(']').trim().trim_matches('"');
            // `[dependencies.foo]` names its dependency in the header itself,
            // and its body is the dependency's own keys, not more dependencies.
            if let Some(name) = section
                .strip_prefix("dependencies.")
                .or_else(|| section.strip_prefix("dev-dependencies."))
                .or_else(|| section.strip_prefix("build-dependencies."))
            {
                dependencies.push((name.to_string(), None));
                in_dependencies = false;
                continue;
            }
            in_dependencies = section == "dependencies"
                || section == "dev-dependencies"
                || section == "build-dependencies"
                || section.ends_with(".dependencies")
                || section.ends_with(".dev-dependencies")
                || section.ends_with(".build-dependencies");
            continue;
        }
        if !in_dependencies || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            let name = key.trim().trim_matches('"').trim();
            if !name.is_empty() {
                dependencies.push((name.to_string(), declared_requirement(value)));
            }
        }
    }
    dependencies
}

/// The version requirement a dependency line declares, when it declares one.
///
/// `foo = "1"` states it plainly; `foo = { version = "1", features = [...] }`
/// buries it in an inline table; `foo = { path = "..." }` has none, because a
/// path dependency carries its version in its own manifest.
fn declared_requirement(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(table) = value.strip_prefix('{') {
        return table
            .split(',')
            .find_map(|entry| key_value(entry, "version"));
    }
    let plain = value.trim_matches('"').trim();
    (!plain.is_empty()).then(|| plain.to_string())
}

/// The lockfile version that answers for `name`.
///
/// A lockfile can hold several versions of one package — a transitive
/// dependency pins an old one while the direct dependency uses a newer — so the
/// declared requirement decides which is meant. Without a usable requirement
/// the newest wins, on the assumption that the dependency doing the asking is
/// the one that was updated.
fn locked_version(
    versions: &[(String, String)],
    name: &str,
    requirement: Option<&str>,
) -> Option<String> {
    let candidates: Vec<&str> = versions
        .iter()
        .filter(|(package, _)| package == name)
        .map(|(_, version)| version.as_str())
        .collect();
    if let Some(prefix) = requirement.and_then(version_prefix) {
        let dotted = format!("{prefix}.");
        let satisfying: Vec<&str> = candidates
            .iter()
            .copied()
            .filter(|version| *version == prefix || version.starts_with(&dotted))
            .collect();
        if let Some(version) = highest(satisfying) {
            return Some(version.to_string());
        }
    }
    highest(candidates).map(str::to_string)
}

/// The leading numeric part of a requirement, e.g. `^1.6.4` becomes `1.6.4`.
fn version_prefix(requirement: &str) -> Option<String> {
    let prefix: String = requirement
        .trim_start_matches(['^', '~', '=', '>', '<', ' ', '*'])
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect();
    let prefix = prefix.trim_end_matches('.');
    (!prefix.is_empty()).then(|| prefix.to_string())
}

/// The greatest of a set of version strings.
fn highest(versions: Vec<&str>) -> Option<&str> {
    versions
        .into_iter()
        .max_by_key(|version| version_key(version))
}

/// A version as its numeric components, so `2.0.21` outranks `1.0.69`.
fn version_key(version: &str) -> Vec<u64> {
    version
        .split(['.', '-', '+'])
        .filter_map(|part| part.parse().ok())
        .collect()
}

/// `(name, version)` for every package in a `Cargo.lock`.
fn lock_versions(lock: &str) -> Vec<(String, String)> {
    let mut versions = Vec::new();
    let mut name: Option<String> = None;
    for line in lock.lines() {
        if line.trim_start().starts_with("[[package]]") {
            name = None;
        } else if let Some(value) = key_value(line, "name") {
            name = Some(value);
        } else if let Some(version) = key_value(line, "version") {
            // The lockfile's own `version = 3` header precedes any package and
            // must not be read as one.
            if let Some(package) = name.take() {
                versions.push((package, version));
            }
        }
    }
    versions
}

/// Reads at most `limit` bytes of a file, cutting on a character boundary.
fn read_bounded(path: &Path, limit: usize) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() <= limit {
        return String::from_utf8(bytes).ok();
    }
    let end = floor_char_boundary(&bytes, limit);
    let mut text = String::from_utf8(bytes[..end].to_vec()).ok()?;
    text.push_str("\n… [truncated]");
    Some(text)
}

/// The largest index `<= end` that is a UTF-8 character boundary.
fn floor_char_boundary(bytes: &[u8], mut end: usize) -> usize {
    while end > 0 && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    end
}

/// `which`, mirrored from the shell tool: the prompt must name the shell that
/// will actually be spawned, and that decision is not exposed by `harness-tools`.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let candidate = dir.join(format!("{name}.exe"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prompt tests that are not about skills call the two-argument form.
    /// This shadowing wrapper keeps them reading as they always did.
    fn assemble(body: &str, env: &Environment) -> String {
        super::assemble(body, env, &[])
    }

    fn skill(name: &str, description: &str, path: &str) -> SkillSummary {
        SkillSummary {
            name: name.to_string(),
            description: description.to_string(),
            source_path: PathBuf::from(path),
        }
    }

    fn env(root: &Path) -> Environment {
        Environment {
            workspace_root: root.to_path_buf(),
            os: "windows".to_string(),
            arch: "x86_64".to_string(),
            shell: "`cmd /C`".to_string(),
            date: "2026-10-01".to_string(),
            project: Some("Rust project `demo` (Cargo.toml)".to_string()),
            installed: Vec::new(),
            agents_md: None,
        }
    }

    #[test]
    fn the_block_names_the_os_and_the_workspace_root() {
        let root = PathBuf::from(r"D:\work\demo");
        let assembled = assemble("You are terse.", &env(&root));

        assert!(
            assembled.contains(&root.display().to_string()),
            "{assembled}"
        );
        assert!(
            assembled.contains("Operating system: windows"),
            "{assembled}"
        );
        assert!(assembled.contains("Date: 2026-10-01"), "{assembled}");
        assert!(assembled.contains("`cmd /C`"), "{assembled}");
    }

    #[test]
    fn the_declared_body_comes_through_unchanged() {
        let body = "You are terse.\nPrefer evidence.";
        let assembled = assemble(body, &env(Path::new(".")));

        assert!(assembled.starts_with(body), "{assembled}");
        // The block is appended, never interleaved into the body.
        assert_eq!(
            assembled.find(body),
            Some(0),
            "the body must lead the prompt"
        );
    }

    #[test]
    fn the_windows_note_warns_about_posix_commands() {
        let windows = assemble("body", &env(Path::new(".")));
        assert!(windows.contains("This is Windows"), "{windows}");
        assert!(windows.contains("rm -rf"), "{windows}");

        let mut unix = env(Path::new("."));
        unix.os = "linux".to_string();
        let linux = assemble("body", &unix);
        assert!(!linux.contains("This is Windows"), "{linux}");
    }

    #[test]
    fn the_analysis_versus_edit_distinction_is_stated() {
        let assembled = assemble("body", &env(Path::new(".")));
        assert!(assembled.contains("critique or a review"), "{assembled}");
        assert!(assembled.contains("change nothing"), "{assembled}");
        assert!(
            assembled.contains("Only modify files when the request asks for a change"),
            "{assembled}"
        );
    }

    #[test]
    fn the_working_agreement_requires_behavioural_evidence() {
        let assembled = assemble("body", &env(Path::new(".")));
        assert!(
            assembled.contains("not that a feature works"),
            "a green build must not read as a working feature: {assembled}"
        );
        assert!(
            assembled.contains("exercise the path that would break"),
            "{assembled}"
        );
        assert!(
            assembled.contains("say the feature is unverified"),
            "{assembled}"
        );
    }

    #[test]
    fn installed_dependencies_report_the_version_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"vitepress":"^1.6.4"},"devDependencies":{"vue":"^3.5.0"}}"#,
        )
        .unwrap();
        let vitepress = dir.path().join("node_modules").join("vitepress");
        std::fs::create_dir_all(&vitepress).unwrap();
        std::fs::write(
            vitepress.join("package.json"),
            r#"{"name":"vitepress","version":"1.6.4"}"#,
        )
        .unwrap();

        // The installed version, not the declared range, and nothing for a
        // dependency whose copy is not on disk.
        assert_eq!(
            installed_dependencies(dir.path()),
            vec!["vitepress 1.6.4".to_string()]
        );
    }

    #[test]
    fn installed_dependencies_come_from_a_lockfile_for_rust() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n\n[dependencies]\nserde_json = \"1\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Cargo.lock"),
            "version = 3\n\n[[package]]\nname = \"serde_json\"\nversion = \"1.0.140\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
             checksum = \"abc\"\n",
        )
        .unwrap();

        // The `[package]` name and the lockfile's own format version are not
        // dependencies.
        assert_eq!(
            installed_dependencies(dir.path()),
            vec!["serde_json 1.0.140".to_string()]
        );
    }

    #[test]
    fn a_locked_version_matching_the_requirement_beats_an_older_pin() {
        let versions = vec![
            ("thiserror".to_string(), "1.0.69".to_string()),
            ("thiserror".to_string(), "2.0.21".to_string()),
        ];
        // The declared `thiserror = "2"` picks the 2.x entry, not the older
        // copy a transitive dependency pinned.
        assert_eq!(
            locked_version(&versions, "thiserror", Some("2")).as_deref(),
            Some("2.0.21")
        );
        // With no requirement to match on, the newest is the direct one.
        assert_eq!(
            locked_version(&versions, "thiserror", None).as_deref(),
            Some("2.0.21")
        );
        assert_eq!(
            locked_version(&versions, "thiserror", Some("^1.0")).as_deref(),
            Some("1.0.69")
        );
    }

    #[test]
    fn installed_dependencies_degrade_without_a_manifest() {
        let dir = tempfile::tempdir().unwrap();
        assert!(installed_dependencies(dir.path()).is_empty());

        let detected = Environment::detect(dir.path(), "`sh -c`");
        assert!(detected.installed.is_empty());
        let assembled = assemble("body", &detected);
        assert!(
            assembled.contains("Installed dependencies: not detected"),
            "{assembled}"
        );
    }

    #[test]
    fn the_skills_index_names_each_skill_and_where_its_body_lives() {
        let skills = vec![
            skill(
                "system-design",
                "Design a system.",
                "skills/system-design/SKILL.md",
            ),
            skill(
                "api-contract",
                "Write an API contract.",
                "skills/api-contract/SKILL.md",
            ),
        ];
        let assembled = super::assemble("body", &env(Path::new(".")), &skills);

        assert!(assembled.contains("## Skills"), "{assembled}");
        assert!(assembled.contains("`system-design`"), "{assembled}");
        assert!(assembled.contains("Design a system."), "{assembled}");
        assert!(assembled.contains("`api-contract`"), "{assembled}");
        assert!(
            assembled.contains("skills/system-design/SKILL.md"),
            "the index must say where the body can be read: {assembled}"
        );
        // Progressive disclosure: the index is not the body.
        assert!(
            assembled.contains("read its `SKILL.md` with `read_file`"),
            "{assembled}"
        );
    }

    #[test]
    fn an_agent_with_no_skills_gets_no_skills_section() {
        let assembled = super::assemble("body", &env(Path::new(".")), &[]);
        assert!(!assembled.contains("## Skills"), "{assembled}");
    }

    #[test]
    fn agents_md_is_included_when_present_and_absent_when_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Always run the tests.").unwrap();

        let detected = Environment::detect(dir.path(), "`sh -c`");
        assert_eq!(detected.agents_md.as_deref(), Some("Always run the tests."));
        let assembled = assemble("body", &detected);
        assert!(
            assembled.contains("Project instructions (AGENTS.md)"),
            "{assembled}"
        );
        assert!(assembled.contains("Always run the tests."), "{assembled}");

        let empty = tempfile::tempdir().unwrap();
        let detected = Environment::detect(empty.path(), "`sh -c`");
        assert!(detected.agents_md.is_none());
        let assembled = assemble("body", &detected);
        assert!(!assembled.contains("AGENTS.md"), "{assembled}");
    }

    #[test]
    fn an_oversized_agents_md_is_truncated_on_a_character_boundary() {
        let dir = tempfile::tempdir().unwrap();
        // Wide characters, so a naive byte cut would split one in half.
        let body = "é".repeat(AGENTS_MAX_BYTES);
        std::fs::write(dir.path().join("AGENTS.md"), &body).unwrap();

        let detected = Environment::detect(dir.path(), "`sh -c`");
        let included = detected.agents_md.expect("a bounded copy");
        assert!(
            included.len() <= AGENTS_MAX_BYTES + 32,
            "{}",
            included.len()
        );
        assert!(included.ends_with("[truncated]"), "{included}");
        // Valid UTF-8 is what `String::from_utf8` proves; the length check above
        // proves the cut happened.
        assert!(included.chars().count() > 0);
    }

    #[test]
    fn the_project_line_comes_from_the_first_marker() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        assert_eq!(
            detect_project(dir.path()).as_deref(),
            Some("Rust project `demo` (Cargo.toml)")
        );

        let unnamed = tempfile::tempdir().unwrap();
        std::fs::write(unnamed.path().join("go.mod"), "module example.com/x\n").unwrap();
        assert_eq!(
            detect_project(unnamed.path()).as_deref(),
            Some("Go project (go.mod)")
        );

        let none = tempfile::tempdir().unwrap();
        assert_eq!(detect_project(none.path()), None);
    }

    #[test]
    fn manifest_names_are_read_from_toml_and_json() {
        assert_eq!(name_value("name = \"demo\"").as_deref(), Some("demo"));
        assert_eq!(name_value("\"name\": \"web\",").as_deref(), Some("web"));
        assert_eq!(name_value("description = \"a thing\""), None);
        assert_eq!(name_value("name = \"\""), None);
        assert_eq!(name_value("nameless = 1"), None);
    }

    #[test]
    fn the_configured_shell_program_wins() {
        let configured = ToolsConfig {
            shell_program: Some("pwsh".to_string()),
            ..ToolsConfig::default()
        };
        assert_eq!(shell_description(&configured), "`pwsh -c` (configured)");

        let blank = ToolsConfig {
            shell_program: Some("   ".to_string()),
            ..ToolsConfig::default()
        };
        // Whitespace is not a program; the detection falls through.
        assert!(shell_description(&blank).contains("-c"));
    }

    #[test]
    fn dates_are_formatted_from_unix_seconds() {
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(1_700_000_000), "2023-11-14");
        assert_eq!(format_date(1_752_000_000), "2025-07-08");
        // A leap day, to catch an off-by-one in the civil conversion.
        assert_eq!(format_date(1_709_164_800), "2024-02-29");
    }
}
