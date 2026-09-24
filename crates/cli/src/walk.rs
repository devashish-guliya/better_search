//! Fallback source: walks a folder tree with the normal file APIs. Slower than reading
//! NTFS directly, but needs no admin rights and works on any file system.

use std::fs;
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use bs_index::IndexBuilder;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

/// Returns the number of entries added.
pub fn walk(builder: &mut IndexBuilder, root: &Path) -> io::Result<usize> {
    if !fs::metadata(root)?.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotADirectory, "not a folder"));
    }
    let label = root.to_string_lossy();
    let label = label.trim_end_matches(['\\', '/']);
    builder.begin_volume(label, 0);

    let mut next_id = 1u64;
    let mut stack: Vec<(PathBuf, u64)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, dir_id)) = stack.pop() {
        // Folders we are not allowed to read are skipped, like Explorer does.
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let attributes = entry.metadata().map(|m| m.file_attributes()).unwrap_or(0);
            let id = next_id;
            next_id += 1;
            builder.push(
                id,
                dir_id,
                &entry.file_name().to_string_lossy(),
                attributes & FILE_ATTRIBUTE_DIRECTORY != 0,
                attributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0,
            );
            // Junctions and symlinks report as symlinks, not dirs, so they are listed
            // but not followed. This avoids loops and double counting.
            if file_type.is_dir() {
                stack.push((entry.path(), id));
            }
        }
    }
    builder.end_volume();
    Ok((next_id - 1) as usize)
}
