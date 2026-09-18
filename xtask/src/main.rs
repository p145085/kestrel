//! Build and packaging tasks.
//!
//!     cargo xtask bundle [--debug]
//!
//! Produces a folder that runs on a machine with none of this installed, which
//! is the only way to know whether what we ship actually works.

mod pe;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Programs that go into the bundle.
const BINARIES: [&str; 3] = ["kestrel-ui", "kestrel", "kestreld"];

/// GStreamer plugins a call needs.
///
/// Loaded by name at runtime rather than linked, so nothing in the import
/// tables leads to them and they have to be listed. The set is small on
/// purpose: the full plugin directory is the better part of a gigabyte, and a
/// chat client that also makes calls needs a few dozen megabytes of it.
const PLUGINS: [&str; 19] = [
    "gstapp",
    "gstaudioconvert",
    "gstaudioresample",
    "gstaudiotestsrc",
    "gstautodetect",
    "gstcoreelements",
    "gstdtls",
    // What the window draws a call into.
    "gstgtk4",
    "gstnice",
    "gstopus",
    "gstplayback",
    "gstrtp",
    "gstrtpmanager",
    "gstsrtp",
    "gsttypefindfunctions",
    "gstvideoconvertscale",
    "gstvideotestsrc",
    "gstvpx",
    "gstwebrtc",
];

/// Windows plugins for real capture devices.
///
/// Separate because they exist only here; a bundle without them still runs,
/// and still makes calls with test media.
#[cfg(windows)]
const CAPTURE_PLUGINS: [&str; 2] = ["gstmediafoundation", "gstwasapi2"];

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("bundle") => {
            let debug = args.any(|arg| arg == "--debug");
            bundle(debug)
        }
        Some(other) => bail!("unknown task {other}; try `cargo xtask bundle`"),
        None => {
            println!("usage: cargo xtask bundle [--debug]");
            Ok(())
        }
    }
}

fn bundle(debug: bool) -> Result<()> {
    if !cfg!(windows) {
        bail!("bundling is only implemented for Windows so far");
    }

    let root = workspace_root()?;
    let profile = if debug { "debug" } else { "release" };

    build(&root, debug)?;

    let prefix = gstreamer_prefix().context(
        "GStreamer was not found. Install it with: winget install gstreamerproject.gstreamer",
    )?;
    let bin = prefix.join("bin");

    let out = root.join("dist").join("kestrel");
    if out.exists() {
        std::fs::remove_dir_all(&out).with_context(|| format!("clearing {}", out.display()))?;
    }
    std::fs::create_dir_all(&out)?;

    // The programs first: everything else follows from what they need.
    let mut roots = Vec::new();
    for name in BINARIES {
        let built = root
            .join("target")
            .join(profile)
            .join(format!("{name}.exe"));
        if !built.is_file() {
            bail!("{} was not built", built.display());
        }
        let placed = out.join(format!("{name}.exe"));
        std::fs::copy(&built, &placed)?;
        roots.push(placed);
    }

    // Plugins are loaded by name, so they are copied before the closure is
    // taken: whatever they pull in has to come along too.
    let plugin_dir = out.join("lib").join("gstreamer-1.0");
    std::fs::create_dir_all(&plugin_dir)?;
    let mut plugins: Vec<&str> = PLUGINS.to_vec();
    #[cfg(windows)]
    plugins.extend(CAPTURE_PLUGINS);

    let source_plugins = prefix.join("lib").join("gstreamer-1.0");
    let mut copied_plugins = 0;
    for name in plugins {
        let from = source_plugins.join(format!("{name}.dll"));
        if !from.is_file() {
            bail!(
                "plugin {name} was not found in {}",
                source_plugins.display()
            );
        }
        let to = plugin_dir.join(format!("{name}.dll"));
        std::fs::copy(&from, &to)?;
        roots.push(to);
        copied_plugins += 1;
    }

    let needed = pe::closure(&roots, &bin)?;
    for name in &needed {
        let from = bin.join(name);
        // The closure matched case-insensitively; find what the directory
        // actually calls it.
        let from = if from.is_file() {
            from
        } else {
            match find_ignoring_case(&bin, name)? {
                Some(path) => path,
                None => continue,
            }
        };
        std::fs::copy(&from, out.join(from.file_name().unwrap_or_default()))?;
    }

    // GTK reads its settings from a compiled schema and refuses to start
    // without one, which is a blank window and no explanation.
    let schemas = out.join("share").join("glib-2.0").join("schemas");
    std::fs::create_dir_all(&schemas)?;
    let compiled = prefix
        .join("share")
        .join("glib-2.0")
        .join("schemas")
        .join("gschemas.compiled");
    if compiled.is_file() {
        std::fs::copy(&compiled, schemas.join("gschemas.compiled"))?;
    } else {
        eprintln!("warning: no compiled GSettings schemas found; the interface may not start");
    }

    write_launcher(&out)?;
    write_server_config(&out)?;
    verify(&out)?;

    let size = directory_size(&out)?;
    println!(
        "bundled {} programs, {copied_plugins} plugins and {} libraries into {}",
        BINARIES.len(),
        needed.len(),
        out.display()
    );
    println!("{} MB", size / (1024 * 1024));
    println!("run it with {}", out.join("kestrel-ui.exe").display());
    Ok(())
}

/// Check the bundle can do what it claims, before anyone relies on it.
///
/// Run with the environment stripped of anything pointing at an installed
/// GStreamer, because a bundle that only works on the machine that built it is
/// exactly the failure this is meant to catch. A plugin missing from the list
/// above shows up here as a named element rather than as a call that silently
/// will not start.
fn verify(out: &Path) -> Result<()> {
    let client = out.join("kestrel.exe");
    let output = std::process::Command::new(&client)
        .arg("--check-media")
        // A bare PATH: the bundled libraries sit beside the executable, which
        // is where Windows looks first, so nothing else should be needed.
        .env("PATH", system_path())
        .env_remove("GSTREAMER_1_0_ROOT_MSVC_X86_64")
        .env_remove("GST_PLUGIN_PATH")
        .env_remove("GST_PLUGIN_SYSTEM_PATH")
        .output()
        .with_context(|| format!("running {}", client.display()))?;

    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stdout);
        let complained = String::from_utf8_lossy(&output.stderr);
        bail!(
            "the bundle cannot make calls: {}{}",
            said.trim(),
            complained.trim()
        );
    }
    Ok(())
}

/// Just the system directories, so nothing of a developer's setup leaks in.
fn system_path() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_owned());
    format!("{root}\\system32;{root}")
}

/// A batch file for starting a second client.
///
/// Double-clicking the executable works, but starting several from one place
/// is what testing a chat client actually looks like.
fn write_launcher(out: &Path) -> Result<()> {
    let script = "\
@echo off
rem Start one Kestrel client. Run this as many times as you want clients;
rem each gets its own window, its own connection and its own nickname.
start \"\" \"%~dp0kestrel-ui.exe\" %*
";
    std::fs::write(out.join("kestrel.cmd"), script)?;

    let server = "\
@echo off
rem Start the server, reading kestreld.toml from this folder.
\"%~dp0kestreld.exe\" \"%~dp0kestreld.toml\"
";
    std::fs::write(out.join("kestreld.cmd"), server)?;
    Ok(())
}

/// A server configuration that works out of the box.
///
/// Loopback only: a bundle that started listening on every interface the
/// moment it was unzipped would be a surprise, and not a welcome one.
fn write_server_config(out: &Path) -> Result<()> {
    let config = "# Kestrel's server. Listening on loopback only; change this to 0.0.0.0:6667
# to let other machines on your network connect.
listen = [\"127.0.0.1:6667\"]
server_name = \"kestrel.local\"
network_name = \"Kestrel\"
calls_enabled = true
motd = [\"A Kestrel server.\"]
";
    std::fs::write(out.join("kestreld.toml"), config)?;
    Ok(())
}

fn build(root: &Path, debug: bool) -> Result<()> {
    let mut command = std::process::Command::new(env!("CARGO"));
    command.current_dir(root).arg("build");
    if !debug {
        command.arg("--release");
    }
    for name in ["kestrel-ui", "kestrel-cli", "kestreld"] {
        command.arg("-p").arg(name);
    }

    let status = command.status().context("running cargo build")?;
    if !status.success() {
        bail!("the build failed");
    }
    Ok(())
}

fn workspace_root() -> Result<PathBuf> {
    // The manifest directory is `xtask`; the workspace is its parent.
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent()
        .map(Path::to_path_buf)
        .context("the workspace root should be the parent of xtask")
}

/// Where GStreamer, and with it GTK, was installed.
fn gstreamer_prefix() -> Option<PathBuf> {
    if let Ok(root) = std::env::var("GSTREAMER_1_0_ROOT_MSVC_X86_64") {
        let path = PathBuf::from(root);
        if path.join("bin").is_dir() {
            return Some(path);
        }
    }

    let candidates = [
        std::env::var("LOCALAPPDATA")
            .ok()
            .map(|local| PathBuf::from(local).join("Programs/gstreamer/1.0/msvc_x86_64")),
        Some(PathBuf::from("C:/gstreamer/1.0/msvc_x86_64")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|path| path.join("bin").is_dir())
}

fn find_ignoring_case(directory: &Path, name: &str) -> Result<Option<PathBuf>> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(name)
        {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

fn directory_size(path: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        total += if meta.is_dir() {
            directory_size(&entry.path())?
        } else {
            meta.len()
        };
    }
    Ok(total)
}
