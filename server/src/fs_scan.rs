use std::fs;
use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::Url;

pub(crate) fn collect_php_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if should_skip_dir(&path) {
                continue;
            }
            collect_php_files(&path, out);
            continue;
        }

        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("php"))
        {
            out.push(path);
        }
    }
}

pub(crate) fn should_skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    matches!(name, ".git" | "node_modules" | "vendor" | "target")
}

pub(crate) fn is_php_uri(uri: &Url) -> bool {
    let path = uri.path().to_ascii_lowercase();
    path.ends_with(".php") || path.ends_with(".blade.php")
}

pub(crate) fn is_blade_uri(uri: &Url) -> bool {
    uri.path().to_ascii_lowercase().ends_with(".blade.php")
}
