//! Exclude patterns, as given with `--exclude` and `-X`.
//!
//! A pattern is split on `/` into components, and each component is matched against one name.
//! A pattern that starts with `/` is anchored at the filesystem root, and any other pattern is
//! matched at every level of the tree. A trailing `/` restricts a pattern to directories.

use std::collections::HashMap;
use std::ffi::{CStr, CString};

/// Whether an entry is excluded. The variants are ordered by strength, so that the strongest of
/// several matches wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Excluded {
    No,
    /// The entry is excluded if it is a directory.
    IfDirectory,
    Always,
}

/// One component of a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Component {
    /// A component without `[`, `*`, `?`, or `\`, which is compared byte by byte.
    Literal(Box<[u8]>),
    /// A component that is matched with `fnmatch`, which reads it up to the first NUL byte.
    Wildcard(CString),
}

struct Node {
    component: Component,
    /// Whether the component was followed by a `/`.
    is_directory: bool,
    /// The next component, matched against the entries of a matching directory.
    sub: Option<usize>,
}

/// The patterns that apply to the entries of one directory, as indices into
/// [`ExcludePatterns::nodes`].
#[derive(Default)]
struct Level {
    /// Final components by their bytes, with whether they match directories only. A literal that
    /// is given both with and without a trailing `/` matches any entry.
    final_literals: HashMap<Box<[u8]>, bool>,
    final_wildcards: Vec<usize>,
    /// Components that are followed by more, by their bytes, with their next components.
    inner_literals: HashMap<Box<[u8]>, Vec<usize>>,
    inner_wildcards: Vec<usize>,
}

impl Level {
    fn add(&mut self, nodes: &[Node], index: usize) {
        let node = &nodes[index];
        match (&node.component, node.sub) {
            (Component::Literal(bytes), None) => {
                let is_directory = self.final_literals.entry(bytes.clone()).or_insert(true);
                *is_directory &= node.is_directory;
            }
            (Component::Literal(bytes), Some(sub)) => {
                self.inner_literals
                    .entry(bytes.clone())
                    .or_default()
                    .push(sub);
            }
            (Component::Wildcard(_), None) => self.final_wildcards.push(index),
            (Component::Wildcard(_), Some(_)) => self.inner_wildcards.push(index),
        }
    }

    fn matches(&self, nodes: &[Node], name: &CStr) -> Excluded {
        let mut excluded = match self.final_literals.get(name.to_bytes()) {
            Some(true) => Excluded::IfDirectory,
            Some(false) => Excluded::Always,
            None => Excluded::No,
        };
        for &index in &self.final_wildcards {
            if excluded == Excluded::Always {
                break;
            }
            let node = &nodes[index];
            if wildcard_matches(&node.component, name) {
                excluded = excluded.max(if node.is_directory {
                    Excluded::IfDirectory
                } else {
                    Excluded::Always
                });
            }
        }
        excluded
    }

    /// Add the next components of the patterns that match the directory `name` to `child`.
    fn enter(&self, nodes: &[Node], name: &CStr, child: &mut Self) {
        if let Some(subs) = self.inner_literals.get(name.to_bytes()) {
            for &sub in subs {
                child.add(nodes, sub);
            }
        }
        for &index in &self.inner_wildcards {
            let node = &nodes[index];
            if let Some(sub) = node.sub
                && wildcard_matches(&node.component, name)
            {
                child.add(nodes, sub);
            }
        }
    }
}

fn wildcard_matches(component: &Component, name: &CStr) -> bool {
    match component {
        Component::Wildcard(pattern) => fnmatch(pattern, name),
        Component::Literal(_) => false,
    }
}

/// Match `name` against the shell wildcard `pattern` with the libc `fnmatch`, which ncdu calls
/// too. Like ncdu, it passes no flags: `*` and `?` match a leading `.`, and `\` escapes. A
/// multibyte character counts as one character only in a UTF-8 locale, which the process has to
/// set up with `setlocale`.
#[expect(unsafe_code, reason = "libc::fnmatch is a foreign function")]
fn fnmatch(pattern: &CStr, name: &CStr) -> bool {
    // SAFETY: Both pointers point to NUL-terminated strings that outlive the call, and `fnmatch`
    // only reads them.
    unsafe { libc::fnmatch(pattern.as_ptr(), name.as_ptr(), 0) == 0 }
}

/// All exclude patterns of a scan.
#[derive(Default)]
pub struct ExcludePatterns {
    /// The components of all patterns. A pattern is a chain of nodes linked by [`Node::sub`].
    nodes: Vec<Node>,
    /// Patterns that start with `/`, which apply from the filesystem root.
    anchored: Level,
    /// Patterns that apply at every level.
    unanchored: Level,
}

impl ExcludePatterns {
    /// Add a pattern. An empty pattern is ignored.
    pub fn add(&mut self, pattern: &[u8]) {
        if pattern.is_empty() {
            return;
        }
        let index = self.parse(pattern);
        if pattern.starts_with(b"/") {
            self.anchored.add(&self.nodes, index);
        } else {
            self.unanchored.add(&self.nodes, index);
        }
    }

    /// Add the components of `pattern` as a chain of nodes and return the index of the first.
    fn parse(&mut self, pattern: &[u8]) -> usize {
        let first = self.nodes.len();
        let mut rest = trim_start_slashes(pattern);
        loop {
            let slash = rest.iter().position(|&byte| byte == b'/');
            let is_directory = slash.is_some();
            let component = &rest[..slash.unwrap_or(rest.len())];
            rest = slash.map_or(&[], |slash| &rest[slash + 1..]);
            let is_last = !is_directory || rest.iter().all(|&byte| byte == b'/');
            let index = self.nodes.len();
            self.nodes.push(Node {
                component: parse_component(component),
                is_directory,
                sub: (!is_last).then_some(index + 1),
            });
            if is_last {
                return first;
            }
        }
    }

    /// The patterns for the entries of the directory at the absolute `path`, which is the root of
    /// a scan. The path itself is not checked. A scan finds the patterns of its subdirectories
    /// with [`DirectoryPatterns::enter`], which is faster.
    pub fn for_path(&self, path: &CStr) -> DirectoryPatterns<'_> {
        let mut patterns = DirectoryPatterns {
            exclude: self,
            level: None,
        };
        let path = trim_end_slashes(trim_start_slashes(path.to_bytes()));
        if path.is_empty() {
            return patterns;
        }
        for component in path.split(|&byte| byte == b'/') {
            // A slice of a `CStr` holds no NUL byte.
            patterns = patterns.enter(&CString::new(component).unwrap_or_default());
        }
        patterns
    }
}

fn parse_component(component: &[u8]) -> Component {
    if component
        .iter()
        .any(|byte| matches!(byte, b'[' | b'*' | b'?' | b'\\'))
    {
        let until_nul = component
            .split(|&byte| byte == 0)
            .next()
            .unwrap_or_default();
        // The slice up to the first NUL byte holds no NUL byte.
        Component::Wildcard(CString::new(until_nul).unwrap_or_default())
    } else {
        Component::Literal(component.into())
    }
}

fn trim_start_slashes(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|&byte| byte != b'/')
        .unwrap_or(bytes.len());
    &bytes[start..]
}

fn trim_end_slashes(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|&byte| byte != b'/')
        .map_or(0, |last| last + 1);
    &bytes[..end]
}

/// The exclude patterns for the entries of one directory.
pub struct DirectoryPatterns<'a> {
    exclude: &'a ExcludePatterns,
    /// The patterns of this directory, or `None` for the filesystem root, whose patterns are
    /// [`ExcludePatterns::anchored`].
    level: Option<Level>,
}

impl DirectoryPatterns<'_> {
    fn level(&self) -> &Level {
        self.level.as_ref().unwrap_or(&self.exclude.anchored)
    }

    /// Whether the entry `name` in this directory is excluded.
    pub fn matches(&self, name: &CStr) -> Excluded {
        let nodes = &self.exclude.nodes;
        let excluded = self.level().matches(nodes, name);
        if excluded == Excluded::Always {
            return excluded;
        }
        excluded.max(self.exclude.unanchored.matches(nodes, name))
    }

    /// The patterns for the entries of the subdirectory `name`.
    #[must_use]
    pub fn enter(&self, name: &CStr) -> Self {
        let nodes = &self.exclude.nodes;
        let mut child = Level::default();
        self.level().enter(nodes, name, &mut child);
        self.exclude.unanchored.enter(nodes, name, &mut child);
        Self {
            exclude: self.exclude,
            level: Some(child),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;

    use super::{Component, ExcludePatterns};

    /// The components of `pattern`, each with whether it matches directories only.
    fn parse(pattern: &[u8]) -> Vec<(Component, bool)> {
        let mut exclude = ExcludePatterns::default();
        let mut next = Some(exclude.parse(pattern));
        let mut components = Vec::new();
        while let Some(index) = next {
            let node = &exclude.nodes[index];
            components.push((node.component.clone(), node.is_directory));
            next = node.sub;
        }
        components
    }

    fn literal(component: &str) -> Component {
        Component::Literal(component.as_bytes().into())
    }

    fn wildcard(component: &str) -> Component {
        Component::Wildcard(CString::new(component).unwrap_or_default())
    }

    /// Port of the upstream test "parse".
    #[test]
    fn parse_components() {
        assert_eq!(parse(b""), [(literal(""), false)]);
        assert_eq!(parse(b"//a//"), [(literal("a"), true)]);
        assert_eq!(
            parse(b"a//b"),
            [
                (literal("a"), true),
                (literal(""), true),
                (literal("b"), false)
            ]
        );
        assert_eq!(
            parse(b"foo*/bar.zig"),
            [(wildcard("foo*"), true), (literal("bar.zig"), false)]
        );
        assert_eq!(
            parse(b"/?/sub/dir/"),
            [
                (wildcard("?"), true),
                (literal("sub"), true),
                (literal("dir"), true),
            ]
        );
    }
}
