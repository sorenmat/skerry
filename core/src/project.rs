//! Project/workspace support for the_editor.
//!
//! A *project* is a directory on disk that contains related files. The
//! editor detects a project by walking up from the active document's
//! path and looking for well-known root markers (`.git`, `Cargo.toml`,
//! `package.json`, `go.mod`, etc.). Once a project root is known, the
//! frontends can render a file tree, scope search to the project, and
//! offer "open file in project" commands.
//!
//! This module is intentionally small for v1: it provides root
//! detection and a safe, bounded directory walk. The frontends own
//! the tree UI and caching.

use std::fs;
use std::path::{Path, PathBuf};

/// Files/directories that identify a project root when found in an
/// ancestor of the current file.
const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "pom.xml",
    "build.gradle",
    "CMakeLists.txt",
    "Makefile",
];

/// A detected project on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// Absolute path to the project root.
    pub root: PathBuf,
}

/// A node in the project filesystem tree. Directories carry their
/// immediate children; files are leaves. Paths are relative to the
/// project root.
#[derive(Debug, Clone)]
pub enum FsNode {
    File {
        name: String,
        rel_path: PathBuf,
    },
    Dir {
        name: String,
        rel_path: PathBuf,
        children: Vec<FsNode>,
    },
}

impl FsNode {
    /// The node's display name.
    pub fn name(&self) -> &str {
        match self {
            FsNode::File { name, .. } | FsNode::Dir { name, .. } => name,
        }
    }

    /// The node's path relative to the project root.
    pub fn rel_path(&self) -> &Path {
        match self {
            FsNode::File { rel_path, .. } | FsNode::Dir { rel_path, .. } => rel_path,
        }
    }

    /// `true` for directories.
    pub fn is_dir(&self) -> bool {
        matches!(self, FsNode::Dir { .. })
    }
}

/// Expansion state for the project tree. Holds the root node and the
/// set of directory paths that are currently expanded. The root is
/// always expanded by default.
#[derive(Debug, Clone, Default)]
pub struct ProjectTree {
    pub root: Option<FsNode>,
    pub expanded: std::collections::HashSet<PathBuf>,
}

impl ProjectTree {
    /// Build a new tree state from a root node. All directories are
    /// expanded by default so the tree behaves like the previous flat
    /// file list until the user explicitly collapses folders.
    pub fn new(root: FsNode) -> Self {
        let mut expanded = std::collections::HashSet::new();
        Self::collect_dirs(&root, &mut expanded);
        Self {
            root: Some(root),
            expanded,
        }
    }

    fn collect_dirs(node: &FsNode, expanded: &mut std::collections::HashSet<PathBuf>) {
        if let FsNode::Dir {
            rel_path, children, ..
        } = node
        {
            expanded.insert(rel_path.to_path_buf());
            for child in children {
                Self::collect_dirs(child, expanded);
            }
        }
    }

    /// Toggle the expansion state of the directory at `rel_path`.
    pub fn toggle(&mut self, rel_path: &Path) {
        if self.expanded.contains(rel_path) {
            self.expanded.remove(rel_path);
        } else {
            self.expanded.insert(rel_path.to_path_buf());
        }
    }

    /// Return the visible rows of the tree as `(depth, node)` pairs,
    /// respecting the current expansion state. Directories themselves
    /// are included even when collapsed; their children are only
    /// included when the parent is expanded.
    pub fn visible_rows(&self) -> Vec<(usize, &FsNode)> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            Self::walk_visible(root, 0, &self.expanded, &mut out);
        }
        out
    }

    fn walk_visible<'a>(
        node: &'a FsNode,
        depth: usize,
        expanded: &std::collections::HashSet<PathBuf>,
        out: &mut Vec<(usize, &'a FsNode)>,
    ) {
        out.push((depth, node));
        if let FsNode::Dir {
            rel_path, children, ..
        } = node
        {
            if expanded.contains(rel_path) {
                for child in children {
                    Self::walk_visible(child, depth + 1, expanded, out);
                }
            }
        }
    }
}

/// One match from a project-wide search.
#[derive(Debug, Clone)]
pub struct ProjectSearchResult {
    /// Path to the file, relative to the project root.
    pub rel_path: PathBuf,
    /// 1-based line number where the match begins.
    pub line: usize,
    /// 1-based character column where the match begins.
    pub col: usize,
    /// The full text of the matched line.
    pub text: String,
}

/// One line that will be changed by a project-wide replace.
#[derive(Debug, Clone)]
pub struct ReplacePreview {
    /// Path to the file, relative to the project root.
    pub rel_path: PathBuf,
    /// 1-based line number of the changed line.
    pub line: usize,
    /// Line text before replacement.
    pub before: String,
    /// Line text after replacement.
    pub after: String,
    /// How many occurrences of the query are replaced on this line.
    pub occurrence_count: usize,
}

/// Error type for project-wide replace operations.
#[derive(Debug, Clone)]
pub struct ReplaceError {
    pub rel_path: PathBuf,
    pub message: String,
}

/// Report returned by a successful project-wide replace.
#[derive(Debug, Clone, Default)]
pub struct ReplaceReport {
    /// Total number of occurrences replaced.
    pub total: usize,
    /// Absolute paths of files that were actually modified.
    pub changed_files: Vec<PathBuf>,
}

impl Project {
    /// Try to detect a project root starting from `path`. If `path` is
    /// a file, the search begins in its parent directory. The search
    /// walks up the filesystem looking for any [`PROJECT_MARKERS`].
    /// Returns `None` for unsaved buffers or when no marker is found.
    pub fn from_path(path: &Path) -> Option<Self> {
        let mut dir = if path.is_file() { path.parent()? } else { path };

        loop {
            for marker in PROJECT_MARKERS {
                if dir.join(marker).exists() {
                    return Some(Self {
                        root: dir.to_path_buf(),
                    });
                }
            }
            let parent = dir.parent()?;
            // Stop at the filesystem root or a bare relative component
            // like `"[No Name]"` whose parent is the empty path.
            if parent.as_os_str().is_empty() {
                return None;
            }
            dir = parent;
        }
    }

    /// Build a filesystem tree under the project root, skipping hidden
    /// directories and respecting `.gitignore` if one exists at the
    /// root. Stops after `max_files` *files* have been collected to keep
    /// the operation bounded for huge trees. Returns the root directory
    /// node, or `None` if the project root itself cannot be read.
    pub fn tree(&self, max_files: usize) -> Option<FsNode> {
        let gitignore = self.root.join(".gitignore");
        let mut ignore = Gitignore::empty();
        if gitignore.exists() {
            if let Ok(contents) = fs::read_to_string(&gitignore) {
                ignore = Gitignore::parse(&contents);
            }
        }
        let mut seen = 0usize;
        Self::walk_node(&self.root, Path::new(""), &ignore, &mut seen, max_files)
    }

    /// Return every file path under the project root, relative to the
    /// root. Skips hidden directories and respects `.gitignore`. Stops
    /// after `max_files` files.
    pub fn all_files(&self, max_files: usize) -> Vec<PathBuf> {
        let gitignore = self.root.join(".gitignore");
        let mut ignore = Gitignore::empty();
        if gitignore.exists() {
            if let Ok(contents) = fs::read_to_string(&gitignore) {
                ignore = Gitignore::parse(&contents);
            }
        }
        let mut files = Vec::new();
        let mut seen = 0usize;
        Self::collect_files(
            &self.root,
            Path::new(""),
            &ignore,
            &mut seen,
            max_files,
            &mut files,
        );
        files
    }

    fn collect_files(
        root: &Path,
        rel_dir: &Path,
        ignore: &Gitignore,
        seen: &mut usize,
        max_files: usize,
        files: &mut Vec<PathBuf>,
    ) {
        if *seen >= max_files {
            return;
        }
        let abs_dir = root.join(rel_dir);
        let Ok(entries) = fs::read_dir(&abs_dir) else {
            return;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            if *seen >= max_files {
                return;
            }
            let name = entry.file_name();
            let name_lossy = name.to_string_lossy();
            if name_lossy.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if ignore.matches_dir(rel) {
                    continue;
                }
                Self::collect_files(root, rel, ignore, seen, max_files, files);
            } else {
                if ignore.matches_file(rel) {
                    continue;
                }
                files.push(rel.to_path_buf());
                *seen += 1;
            }
        }
    }

    /// Search all project files for `query` using literal substring
    /// matching. Returns up to `max_results` matches. Files larger than
    /// `max_file_bytes` are skipped to avoid freezing on huge/binary
    /// files. Binary files (invalid UTF-8) are also skipped for v1.
    pub fn search(&self, query: &str, max_results: usize) -> Vec<ProjectSearchResult> {
        if query.is_empty() || max_results == 0 {
            return Vec::new();
        }
        let needle = query.as_bytes();
        let tree = match self.tree(10_000) {
            Some(FsNode::Dir { children, .. }) => children,
            _ => return Vec::new(),
        };
        let mut results = Vec::new();
        Self::search_nodes(
            &self.root,
            &tree,
            needle,
            max_results,
            1024 * 1024, // 1 MiB per-file limit
            &mut results,
        );
        results
    }

    fn search_nodes(
        root: &Path,
        nodes: &[FsNode],
        needle: &[u8],
        max_results: usize,
        max_file_bytes: usize,
        results: &mut Vec<ProjectSearchResult>,
    ) {
        for node in nodes {
            if results.len() >= max_results {
                return;
            }
            match node {
                FsNode::File { rel_path, .. } => {
                    Self::search_file(root, rel_path, needle, max_results, max_file_bytes, results);
                }
                FsNode::Dir { children, .. } => {
                    Self::search_nodes(
                        root,
                        children,
                        needle,
                        max_results,
                        max_file_bytes,
                        results,
                    );
                }
            }
        }
    }

    fn search_file(
        root: &Path,
        rel_path: &Path,
        needle: &[u8],
        max_results: usize,
        max_file_bytes: usize,
        results: &mut Vec<ProjectSearchResult>,
    ) {
        let abs_path = root.join(rel_path);
        let Ok(metadata) = fs::metadata(&abs_path) else {
            return;
        };
        if metadata.len() as usize > max_file_bytes {
            return;
        }
        let Ok(bytes) = fs::read(&abs_path) else {
            return;
        };
        // Skip binary files for v1.
        let Ok(text) = String::from_utf8(bytes) else {
            return;
        };
        for (line_idx, line) in text.lines().enumerate() {
            if results.len() >= max_results {
                return;
            }
            let line_bytes = line.as_bytes();
            for offset in memchr::memmem::find_iter(line_bytes, needle) {
                if results.len() >= max_results {
                    return;
                }
                let col = line[..offset].chars().count() + 1;
                results.push(ProjectSearchResult {
                    rel_path: rel_path.to_path_buf(),
                    line: line_idx + 1,
                    col,
                    text: line.to_string(),
                });
            }
        }
    }

    /// Preview a project-wide literal replace. Returns up to
    /// `max_results` changed lines without modifying any files.
    pub fn replace_preview(
        &self,
        query: &str,
        replacement: &str,
        max_results: usize,
    ) -> Vec<ReplacePreview> {
        if query.is_empty() || max_results == 0 {
            return Vec::new();
        }
        let needle = query.as_bytes();
        let tree = match self.tree(10_000) {
            Some(FsNode::Dir { children, .. }) => children,
            _ => return Vec::new(),
        };
        let mut previews = Vec::new();
        Self::preview_replace_nodes(
            &self.root,
            &tree,
            needle,
            replacement,
            max_results,
            1024 * 1024,
            &mut previews,
        );
        previews
    }

    /// Apply a project-wide literal replace, writing changed files back
    /// to disk. Returns the total number of replacements made, or the
    /// first error encountered. Files larger than 1 MiB or invalid UTF-8
    /// are skipped.
    pub fn replace_all(
        &self,
        query: &str,
        replacement: &str,
    ) -> Result<ReplaceReport, ReplaceError> {
        if query.is_empty() {
            return Ok(ReplaceReport::default());
        }
        let needle = query.as_bytes();
        let tree = match self.tree(10_000) {
            Some(FsNode::Dir { children, .. }) => children,
            _ => return Ok(ReplaceReport::default()),
        };
        let mut report = ReplaceReport::default();
        Self::replace_all_nodes(
            &self.root,
            &tree,
            needle,
            replacement,
            1024 * 1024,
            &mut report,
        )?;
        Ok(report)
    }

    fn preview_replace_nodes(
        root: &Path,
        nodes: &[FsNode],
        needle: &[u8],
        replacement: &str,
        max_results: usize,
        max_file_bytes: usize,
        previews: &mut Vec<ReplacePreview>,
    ) {
        for node in nodes {
            if previews.len() >= max_results {
                return;
            }
            match node {
                FsNode::File { rel_path, .. } => {
                    Self::preview_replace_file(
                        root,
                        rel_path,
                        needle,
                        replacement,
                        max_results,
                        max_file_bytes,
                        previews,
                    );
                }
                FsNode::Dir { children, .. } => {
                    Self::preview_replace_nodes(
                        root,
                        children,
                        needle,
                        replacement,
                        max_results,
                        max_file_bytes,
                        previews,
                    );
                }
            }
        }
    }

    fn preview_replace_file(
        root: &Path,
        rel_path: &Path,
        needle: &[u8],
        replacement: &str,
        max_results: usize,
        max_file_bytes: usize,
        previews: &mut Vec<ReplacePreview>,
    ) {
        let abs_path = root.join(rel_path);
        let Ok(metadata) = fs::metadata(&abs_path) else {
            return;
        };
        if metadata.len() as usize > max_file_bytes {
            return;
        }
        let Ok(bytes) = fs::read(&abs_path) else {
            return;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return;
        };
        for (line_idx, line) in text.lines().enumerate() {
            if previews.len() >= max_results {
                return;
            }
            let count = memchr::memmem::find_iter(line.as_bytes(), needle).count();
            if count > 0 {
                let query = std::str::from_utf8(needle).unwrap_or("");
                let after = line.replace(query, replacement);
                if after != line {
                    previews.push(ReplacePreview {
                        rel_path: rel_path.to_path_buf(),
                        line: line_idx + 1,
                        before: line.to_string(),
                        after,
                        occurrence_count: count,
                    });
                }
            }
        }
    }

    fn replace_all_nodes(
        root: &Path,
        nodes: &[FsNode],
        needle: &[u8],
        replacement: &str,
        max_file_bytes: usize,
        report: &mut ReplaceReport,
    ) -> Result<(), ReplaceError> {
        for node in nodes {
            match node {
                FsNode::File { rel_path, .. } => {
                    Self::replace_all_file(
                        root,
                        rel_path,
                        needle,
                        replacement,
                        max_file_bytes,
                        report,
                    )?;
                }
                FsNode::Dir { children, .. } => {
                    Self::replace_all_nodes(
                        root,
                        children,
                        needle,
                        replacement,
                        max_file_bytes,
                        report,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn replace_all_file(
        root: &Path,
        rel_path: &Path,
        needle: &[u8],
        replacement: &str,
        max_file_bytes: usize,
        report: &mut ReplaceReport,
    ) -> Result<(), ReplaceError> {
        let abs_path = root.join(rel_path);
        let metadata = fs::metadata(&abs_path).map_err(|e| ReplaceError {
            rel_path: rel_path.to_path_buf(),
            message: format!("metadata: {e}"),
        })?;
        if metadata.len() as usize > max_file_bytes {
            return Ok(());
        }
        let bytes = fs::read(&abs_path).map_err(|e| ReplaceError {
            rel_path: rel_path.to_path_buf(),
            message: format!("read: {e}"),
        })?;
        let text = String::from_utf8(bytes).map_err(|_| ReplaceError {
            rel_path: rel_path.to_path_buf(),
            message: "invalid UTF-8".to_string(),
        })?;
        let query = std::str::from_utf8(needle).map_err(|_| ReplaceError {
            rel_path: rel_path.to_path_buf(),
            message: "invalid UTF-8 query".to_string(),
        })?;
        let new_text = text.replace(query, replacement);
        let count = memchr::memmem::find_iter(text.as_bytes(), needle).count();
        if count > 0 {
            fs::write(&abs_path, new_text).map_err(|e| ReplaceError {
                rel_path: rel_path.to_path_buf(),
                message: format!("write: {e}"),
            })?;
            report.total += count;
            report.changed_files.push(abs_path);
        }
        Ok(())
    }

    fn walk_node(
        root: &Path,
        rel_dir: &Path,
        ignore: &Gitignore,
        seen: &mut usize,
        max_files: usize,
    ) -> Option<FsNode> {
        let abs_dir = root.join(rel_dir);
        let name = if rel_dir.as_os_str().is_empty() {
            root.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string()
        } else {
            rel_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string()
        };
        let mut children = Vec::new();

        let Ok(entries) = fs::read_dir(&abs_dir) else {
            return Some(FsNode::Dir {
                name,
                rel_path: rel_dir.to_path_buf(),
                children,
            });
        };

        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by(|a, b| {
            let a_is_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let b_is_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
            b_is_dir
                .cmp(&a_is_dir)
                .then_with(|| a.file_name().cmp(&b.file_name()))
        });

        for entry in entries {
            let name = entry.file_name();
            let name_lossy = name.to_string_lossy();
            if name_lossy.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);

            if is_dir {
                if ignore.matches_dir(rel) {
                    continue;
                }
                if let Some(node) = Self::walk_node(root, rel, ignore, seen, max_files) {
                    children.push(node);
                }
            } else {
                if ignore.matches_file(rel) {
                    continue;
                }
                if *seen >= max_files {
                    break;
                }
                children.push(FsNode::File {
                    name: name_lossy.to_string(),
                    rel_path: rel.to_path_buf(),
                });
                *seen += 1;
            }
        }

        Some(FsNode::Dir {
            name,
            rel_path: rel_dir.to_path_buf(),
            children,
        })
    }
}

/// Very small, line-oriented `.gitignore` parser for project tree
/// browsing. Supports literal names, `*` wildcards, and directory-only
/// rules (trailing `/`). Does not handle `!` negation, character
/// ranges, or `**` — those are acceptable limitations for a v1 file
/// tree.
#[derive(Debug, Clone)]
struct Gitignore {
    file_patterns: Vec<String>,
    dir_patterns: Vec<String>,
}

impl Gitignore {
    fn empty() -> Self {
        Self {
            file_patterns: Vec::new(),
            dir_patterns: Vec::new(),
        }
    }

    fn parse(contents: &str) -> Self {
        let mut file_patterns = Vec::new();
        let mut dir_patterns = Vec::new();
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let is_dir = line.ends_with('/');
            let pattern = if is_dir {
                line.trim_end_matches('/').to_string()
            } else {
                line.to_string()
            };
            if is_dir {
                dir_patterns.push(pattern);
            } else {
                file_patterns.push(pattern);
            }
        }
        Self {
            file_patterns,
            dir_patterns,
        }
    }

    fn matches_file(&self, rel: &Path) -> bool {
        let name = rel.file_name().map(|s| s.to_string_lossy());
        let name = name.as_deref().unwrap_or("");
        let rel_str = rel.to_string_lossy();
        for p in &self.file_patterns {
            if gitignore_match(name, &rel_str, p) {
                return true;
            }
        }
        false
    }

    fn matches_dir(&self, rel: &Path) -> bool {
        let name = rel.file_name().map(|s| s.to_string_lossy());
        let name = name.as_deref().unwrap_or("");
        let rel_str = rel.to_string_lossy();
        // Directory entries are matched by both directory-only patterns
        // (trailing `/`) and plain patterns without a slash. This means
        // `node_modules` in `.gitignore` skips the directory even if the
        // user didn't write `node_modules/`.
        for p in self.dir_patterns.iter().chain(self.file_patterns.iter()) {
            if gitignore_match(name, &rel_str, p) {
                return true;
            }
        }
        false
    }
}

fn gitignore_match(file_name: &str, rel_path: &str, pattern: &str) -> bool {
    // Patterns without a slash match against any path component.
    if !pattern.contains('/') {
        if pattern.starts_with('*') && pattern.ends_with('*') && pattern.len() > 1 {
            let middle = &pattern[1..pattern.len() - 1];
            return file_name.contains(middle);
        }
        if let Some(prefix) = pattern.strip_prefix('*') {
            return file_name.ends_with(prefix);
        }
        if let Some(suffix) = pattern.strip_suffix('*') {
            return file_name.starts_with(suffix);
        }
        return file_name == pattern;
    }
    // Patterns with a slash are anchored to the project root.
    let anchored = pattern.strip_prefix('/').unwrap_or(pattern);
    rel_path == anchored || rel_path.starts_with(&format!("{anchored}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir_with(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(prefix);
        fs::create_dir_all(&path).unwrap();
        (dir, path)
    }

    #[test]
    fn detects_git_root() {
        let (dir, sub) = temp_dir_with("src/nested");
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        let proj = Project::from_path(&sub).unwrap();
        assert_eq!(proj.root, dir.path());
    }

    #[test]
    fn detects_cargo_toml() {
        let (dir, file) = temp_dir_with("src/main.rs");
        fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        let proj = Project::from_path(&file).unwrap();
        assert_eq!(proj.root, dir.path());
    }

    #[test]
    fn no_marker_means_no_project() {
        let (dir, file) = temp_dir_with("src/main.rs");
        assert!(Project::from_path(&file).is_none());
        let _ = dir;
    }

    #[test]
    fn unsaved_buffer_has_no_project() {
        assert!(Project::from_path(Path::new("[No Name]")).is_none());
    }

    fn collect_files(node: &FsNode) -> Vec<String> {
        let mut out = Vec::new();
        fn walk(node: &FsNode, out: &mut Vec<String>) {
            match node {
                FsNode::File { rel_path, .. } => {
                    out.push(rel_path.to_string_lossy().to_string());
                }
                FsNode::Dir { children, .. } => {
                    for child in children {
                        walk(child, out);
                    }
                }
            }
        }
        walk(node, &mut out);
        out
    }

    #[test]
    fn project_tree_toggle_hides_children() {
        let (dir, _) = temp_dir_with("");
        let root = dir.path();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();

        let proj = Project::from_path(root).unwrap();
        let tree_root = proj.tree(100).unwrap();
        let mut tree = ProjectTree::new(tree_root);
        assert!(tree
            .visible_rows()
            .iter()
            .any(|(_, n)| n.name() == "lib.rs"));

        tree.toggle(Path::new("src"));
        assert!(!tree
            .visible_rows()
            .iter()
            .any(|(_, n)| n.name() == "lib.rs"));

        tree.toggle(Path::new("src"));
        assert!(tree
            .visible_rows()
            .iter()
            .any(|(_, n)| n.name() == "lib.rs"));
    }

    #[test]
    fn tree_respects_gitignore() {
        let (dir, _) = temp_dir_with("");
        let root = dir.path();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/out"), "").unwrap();
        fs::write(root.join("app.log"), "").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "").unwrap();

        let proj = Project::from_path(root).unwrap();
        let tree = proj.tree(100).unwrap();
        let files = collect_files(&tree);

        assert!(files.contains(&"Cargo.toml".to_string()));
        assert!(files.contains(&"main.rs".to_string()));
        assert!(files.contains(&"src/lib.rs".to_string()));
        assert!(!files.contains(&"target/out".to_string()));
        assert!(!files.contains(&"app.log".to_string()));
        assert!(!files.iter().any(|f| f.starts_with(".git")));
    }

    #[test]
    fn tree_is_bounded() {
        let (dir, _) = temp_dir_with("");
        let root = dir.path();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        fs::write(root.join("marker.txt"), "").unwrap();
        for i in 0..10 {
            fs::write(root.join(format!("file{i}.txt")), "").unwrap();
        }
        let proj = Project::from_path(root).unwrap();
        let tree = proj.tree(5).unwrap();
        assert_eq!(collect_files(&tree).len(), 5);
    }

    #[test]
    fn gitignore_wildcards() {
        let gi = Gitignore::parse("*.log\nbuild/\nfoo.*\n");
        assert!(gi.matches_file(Path::new("debug.log")));
        assert!(!gi.matches_file(Path::new("main.rs")));
        assert!(gi.matches_dir(Path::new("build")));
        assert!(gi.matches_file(Path::new("foo.txt")));
    }

    #[test]
    fn search_finds_matches_across_files() {
        let (dir, _) = temp_dir_with("");
        let root = dir.path();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\nfn helper() {}").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn helper() {}").unwrap();

        let proj = Project::from_path(root).unwrap();
        let results = proj.search("fn", 100);
        assert_eq!(results.len(), 3, "expected 3 'fn' matches");
        assert!(results
            .iter()
            .any(|r| r.rel_path == Path::new("main.rs") && r.line == 1));
        assert!(results
            .iter()
            .any(|r| r.rel_path == Path::new("main.rs") && r.line == 2));
        assert!(results
            .iter()
            .any(|r| r.rel_path == Path::new("src/lib.rs")));
    }

    #[test]
    fn search_respects_max_results() {
        let (dir, _) = temp_dir_with("");
        let root = dir.path();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        for i in 0..5 {
            fs::write(root.join(format!("file{i}.txt")), "foo\nfoo\n").unwrap();
        }
        let proj = Project::from_path(root).unwrap();
        assert_eq!(proj.search("foo", 3).len(), 3);
    }

    #[test]
    fn search_skips_binary_files() {
        let (dir, _) = temp_dir_with("");
        let root = dir.path();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        fs::write(root.join("data.bin"), vec![0u8, 159, 146, 150]).unwrap();
        fs::write(root.join("plain.txt"), "hello").unwrap();

        let proj = Project::from_path(root).unwrap();
        let results = proj.search("hello", 100);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rel_path, Path::new("plain.txt"));
    }

    #[test]
    fn replace_preview_shows_changed_lines() {
        let (dir, _) = temp_dir_with("");
        let root = dir.path();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\nfn helper() {}").unwrap();

        let proj = Project::from_path(root).unwrap();
        let previews = proj.replace_preview("fn", "pub fn", 100);
        assert_eq!(previews.len(), 2);
        assert!(previews.iter().any(|p| {
            p.rel_path == Path::new("main.rs")
                && p.before == "fn main() {}"
                && p.after == "pub fn main() {}"
        }));
    }

    #[test]
    fn replace_all_writes_files() {
        let (dir, _) = temp_dir_with("");
        let root = dir.path();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\nfn helper() {}").unwrap();

        let proj = Project::from_path(root).unwrap();
        let report = proj.replace_all("fn", "pub fn").unwrap();
        assert_eq!(report.total, 2);
        assert_eq!(report.changed_files.len(), 1);
        assert!(report.changed_files[0].ends_with("main.rs"));
        let text = fs::read_to_string(root.join("main.rs")).unwrap();
        assert!(text.contains("pub fn main() {}"));
        assert!(text.contains("pub fn helper() {}"));
    }

    #[test]
    fn replace_preview_counts_multiple_occurrences_on_same_line() {
        let (dir, _) = temp_dir_with("");
        let root = dir.path();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        fs::write(root.join("main.rs"), "foo foo foo").unwrap();

        let proj = Project::from_path(root).unwrap();
        let previews = proj.replace_preview("foo", "bar", 100);
        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0].occurrence_count, 3);
        assert_eq!(previews[0].before, "foo foo foo");
        assert_eq!(previews[0].after, "bar bar bar");
    }

    #[test]
    fn replace_all_returns_changed_files() {
        let (dir, _) = temp_dir_with("");
        let root = dir.path();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        fs::write(root.join("a.rs"), "fn a() {}").unwrap();
        fs::write(root.join("b.rs"), "fn b() {}").unwrap();
        fs::write(root.join("c.rs"), "// no match").unwrap();

        let proj = Project::from_path(root).unwrap();
        let report = proj.replace_all("fn", "pub fn").unwrap();
        assert_eq!(report.total, 2);
        assert_eq!(report.changed_files.len(), 2);
        assert!(report.changed_files.iter().any(|p| p.ends_with("a.rs")));
        assert!(report.changed_files.iter().any(|p| p.ends_with("b.rs")));
        assert!(!report.changed_files.iter().any(|p| p.ends_with("c.rs")));
    }
}
