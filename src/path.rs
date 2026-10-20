use napi::Result;
use napi_derive::napi;

use crate::error::err;

/// The default amount of symlinks followed when resolving a path with
/// [`pathRealpath()`] (matches `gix::path::realpath::MAX_SYMLINKS`).
const PATH_MAX_SYMLINKS: u8 = 32;

fn path_to_string(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

fn bstr_path_to_string(b: &gix::bstr::BStr) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Check each component of `path` and resolve symlinks, like `realpath(3)`.
///
/// Non-existing components are kept as-is. If `path` is relative, the current
/// working directory is used to make it absolute. Throws when more than
/// `maxSymlinks` (default 32) symlinks are followed, when a `..` component
/// escapes the root, or when the current directory cannot be determined.
#[napi]
pub fn path_realpath(path: String, max_symlinks: Option<u32>) -> Result<String> {
    let max = max_symlinks.unwrap_or(PATH_MAX_SYMLINKS as u32) as u8;
    gix::path::realpath_opts(
        std::path::Path::new(&path),
        &std::env::current_dir().map_err(|e| err("current_dir", e))?,
        max,
    )
    .map(|p| path_to_string(&p))
    .map_err(|e| err("realpath", e))
}

/// Like [`pathRealpath()`], but resolves relative paths against an explicit
/// `cwd` instead of the current working directory.
#[napi]
pub fn path_realpath_with_cwd(
    path: String,
    cwd: String,
    max_symlinks: Option<u32>,
) -> Result<String> {
    let max = max_symlinks.unwrap_or(PATH_MAX_SYMLINKS as u32) as u8;
    gix::path::realpath_opts(std::path::Path::new(&path), std::path::Path::new(&cwd), max)
        .map(|p| path_to_string(&p))
        .map_err(|e| err("realpath", e))
}

/// Resolve relative components virtually, eliminating intermediate `..` without
/// accessing the filesystem, e.g. `a/./b/c/.././..` → `a`.
///
/// Returns `null` when normalization exhausts `currentDir` (e.g. popping `..`
/// beyond the root), which indicates an inconsistent path. Leading `.` components
/// and duplicate separators are left untouched; symlinks are never resolved.
#[napi]
pub fn path_normalize(path: String, current_dir: String) -> Option<String> {
    gix::path::normalize(
        std::path::PathBuf::from(path).into(),
        std::path::Path::new(&current_dir),
    )
    .map(|p| path_to_string(&p))
}

/// Like [`pathNormalize()`], but treats `..` components beyond the filesystem
/// root as no-ops instead of returning `null`.
///
/// Throws when normalization exhausts an empty or relative `currentDir`.
#[napi]
pub fn path_normalize_saturating(path: String, current_dir: String) -> Result<String> {
    gix::path::normalize_saturating(
        std::path::PathBuf::from(path).into(),
        std::path::Path::new(&current_dir),
    )
    .map(|p| path_to_string(&p))
    .map_err(|e| err("normalize_saturating", e))
}

/// Like [`pathNormalize()`], but also removes `.` components and duplicate or
/// trailing separators. If cleaning leaves no components, `currentDir` is
/// returned. Returns `null` when normalization exhausts `currentDir`.
#[napi]
pub fn path_normalize_and_clean(path: String, current_dir: String) -> Option<String> {
    gix::path::normalize_and_clean(
        std::path::PathBuf::from(path).into(),
        std::path::Path::new(&current_dir),
    )
    .map(|p| path_to_string(&p))
}

/// Rebuild the worktree-relative `relativePath` to be relative to `prefix`,
/// the worktree-relative equivalent of the user's position (like git printing
/// `../foo` from a subdirectory). A no-op when `prefix` is empty; returns `.`
/// when the result would be empty.
///
/// Both inputs are assumed to be normalized and case-equal.
#[napi]
pub fn path_relativize_with_prefix(relative_path: String, prefix: String) -> String {
    path_to_string(&gix::path::relativize_with_prefix(
        std::path::Path::new(&relative_path),
        std::path::Path::new(&prefix),
    ))
}

/// Whether `path` is absolute by platform rules, or looks like a Unix path
/// (starts with `/`), matching git's interpretation.
#[napi]
pub fn path_is_absolute(path: String) -> bool {
    gix::path::is_absolute(std::path::Path::new(&path))
}

/// Replace backslashes with slashes unconditionally, typically turning a
/// Windows path into a Unix-style path. A no-op for paths without backslashes.
#[napi]
pub fn path_to_unix_separators(path: String) -> String {
    bstr_path_to_string(&gix::path::to_unix_separators(gix::bstr::BStr::new(
        path.as_bytes(),
    )))
}

/// Assure the given path bytes use the native path separator: a no-op on Unix,
/// slashes → backslashes on Windows.
#[napi]
pub fn path_to_native_separators(path: String) -> String {
    bstr_path_to_string(&gix::path::to_native_separators(gix::bstr::BStr::new(
        path.as_bytes(),
    )))
}
