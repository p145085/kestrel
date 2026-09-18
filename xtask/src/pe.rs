//! Just enough of the PE format to ask a binary what it needs.
//!
//! Bundling by hand means a list that is wrong the moment a dependency
//! changes, and copying the whole toolchain means shipping two hundred
//! megabytes of things nobody loads. Reading the import table gives the
//! answer the loader itself will use.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};

/// A section's place in the file, for turning addresses into offsets.
struct Section {
    virtual_address: u32,
    raw_size: u32,
    raw_offset: u32,
}

/// The names of the DLLs a binary imports directly.
///
/// Only the names: resolving them to files is the caller's business, because
/// which directory a name resolves to is what bundling is deciding.
pub fn imports(path: &Path) -> Result<Vec<String>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    parse_imports(&bytes).with_context(|| format!("reading the imports of {}", path.display()))
}

fn parse_imports(bytes: &[u8]) -> Result<Vec<String>> {
    let pe = u32(bytes, 0x3c)? as usize;
    if bytes.get(pe..pe + 4) != Some(b"PE\0\0") {
        bail!("not a PE binary");
    }

    let sections = u16(bytes, pe + 6)? as usize;
    let optional_size = u16(bytes, pe + 20)? as usize;
    let optional = pe + 24;

    // The data directories sit at the end of the optional header, whose length
    // differs between 32- and 64-bit images.
    let magic = u16(bytes, optional)?;
    let directories = match magic {
        0x10b => optional + 96,
        0x20b => optional + 112,
        other => bail!("unrecognised optional header magic {other:#x}"),
    };

    // The second directory is the import table.
    let import_rva = u32(bytes, directories + 8)?;
    if import_rva == 0 {
        return Ok(Vec::new());
    }

    let table = optional + optional_size;
    let mut layout = Vec::with_capacity(sections);
    for index in 0..sections {
        let header = table + 40 * index;
        layout.push(Section {
            virtual_address: u32(bytes, header + 12)?,
            raw_size: u32(bytes, header + 16)?,
            raw_offset: u32(bytes, header + 20)?,
        });
    }

    let offset_of = |rva: u32| -> Option<usize> {
        layout.iter().find_map(|section| {
            let end = section.virtual_address.checked_add(section.raw_size)?;
            (rva >= section.virtual_address && rva < end)
                .then(|| (section.raw_offset + (rva - section.virtual_address)) as usize)
        })
    };

    let mut names = Vec::new();
    let mut entry = offset_of(import_rva).context("the import table is outside every section")?;
    // Each descriptor is twenty bytes, and a run of zeroes ends the list.
    while let Some(descriptor) = bytes.get(entry..entry + 20) {
        if descriptor.iter().all(|byte| *byte == 0) {
            break;
        }
        let name_rva = u32(bytes, entry + 12)?;
        if name_rva == 0 {
            break;
        }
        if let Some(at) = offset_of(name_rva) {
            names.push(read_c_string(bytes, at));
        }
        entry += 20;
    }
    Ok(names)
}

/// Every DLL in `from` that `roots` need, directly or otherwise.
///
/// Names that do not resolve to a file in `from` are left alone: those are the
/// system's own libraries, which must come from the system rather than be
/// copied out of somebody's toolchain.
pub fn closure(roots: &[std::path::PathBuf], from: &Path) -> Result<BTreeSet<String>> {
    let mut needed = BTreeSet::new();
    let mut pending: Vec<std::path::PathBuf> = roots.to_vec();

    while let Some(binary) = pending.pop() {
        for name in imports(&binary)? {
            let candidate = from.join(&name);
            if !candidate.is_file() {
                continue;
            }
            // Compared case-insensitively, because the import table and the
            // file system disagree about capitalisation often enough.
            let key = name.to_ascii_lowercase();
            if needed.insert(key) {
                pending.push(candidate);
            }
        }
    }
    Ok(needed)
}

fn read_c_string(bytes: &[u8], at: usize) -> String {
    let end = bytes[at..]
        .iter()
        .position(|byte| *byte == 0)
        .map_or(bytes.len(), |len| at + len);
    String::from_utf8_lossy(&bytes[at..end]).into_owned()
}

fn u16(bytes: &[u8], at: usize) -> Result<u16> {
    let slice = bytes.get(at..at + 2).context("truncated")?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn u32(bytes: &[u8], at: usize) -> Result<u32> {
    let slice = bytes.get(at..at + 4).context("truncated")?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binary_that_is_not_a_pe_is_refused_rather_than_misread() {
        assert!(parse_imports(b"this is not a binary at all").is_err());
    }

    #[test]
    fn an_empty_input_does_not_panic() {
        // The input is a file on disk, so being wrong about its shape must
        // produce an error rather than an index out of bounds.
        assert!(parse_imports(&[]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn a_real_binary_lists_the_system_libraries_it_uses() {
        // Our own test binary will do: every Windows executable imports
        // kernel32, so this checks the walk reaches real names.
        let me = std::env::current_exe().expect("the test binary exists");
        let names = imports(&me).expect("should parse");
        assert!(
            names
                .iter()
                .any(|name| name.eq_ignore_ascii_case("KERNEL32.dll")),
            "got {names:?}"
        );
    }
}
