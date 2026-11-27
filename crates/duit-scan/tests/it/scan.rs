use std::ffi::CString;
use std::fmt::Write as _;
use std::fs::{self, File, FileTimes, Permissions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::PathBuf;
#[cfg(feature = "test-ncdu")]
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

#[cfg(feature = "test-ncdu")]
use duit_format::JsonExport;
use duit_model::{
    DirectoryId, EntryKind, EntryType, Extended, MemorySink, Producer, Progress, ProgressSink,
    Sink, Special, Stat, Tree,
};
use duit_scan::{ExcludePatterns, ScanOptions, scan};
use insta::assert_snapshot;
use rustix::process::geteuid;
use tempfile::TempDir;

#[derive(Default)]
struct Node {
    label: String,
    children: Vec<Self>,
}

impl Node {
    /// Render the tree with the entries of each directory sorted, because the order of `readdir`
    /// differs between filesystems.
    fn render(&mut self, depth: usize, output: &mut String) {
        let _ = writeln!(output, "{}{}", "  ".repeat(depth), self.label);
        self.children.sort_by(|a, b| a.label.cmp(&b.label));
        for child in &mut self.children {
            child.render(depth + 1, output);
        }
    }
}

/// A [`Sink`] that keeps the fields that are the same on every filesystem. Directories have no
/// size or link count, because those differ between APFS, ext4, and tmpfs.
#[derive(Default)]
struct Recorder {
    root: Option<Node>,
    calls: usize,
    stats: Vec<(String, Stat)>,
}

fn name(name: &[u8]) -> String {
    String::from_utf8_lossy(name).into_owned()
}

impl Sink for Recorder {
    type Directory = Node;

    fn create_root(&mut self, _path: &[u8], stat: &Stat) -> io::Result<Node> {
        self.calls += 1;
        Ok(Node {
            label: format!("[ROOT] {:?}", stat.entry_type),
            children: Vec::new(),
        })
    }

    fn add_special(
        &mut self,
        directory: &mut Node,
        name_: &[u8],
        special: Special,
    ) -> io::Result<()> {
        self.calls += 1;
        directory.children.push(Node {
            label: format!("{} {special:?}", name(name_)),
            children: Vec::new(),
        });
        Ok(())
    }

    fn add_stat(&mut self, directory: &mut Node, name_: &[u8], stat: &Stat) -> io::Result<()> {
        self.calls += 1;
        let mut label = format!("{} {:?} size={}", name(name_), stat.entry_type, stat.size);
        if stat.entry_type == EntryType::HardLink {
            let _ = write!(label, " links={}", stat.link_count);
        }
        directory.children.push(Node {
            label,
            children: Vec::new(),
        });
        self.stats.push((name(name_), *stat));
        Ok(())
    }

    fn add_directory(&mut self, _parent: &mut Node, name_: &[u8], stat: &Stat) -> io::Result<Node> {
        self.calls += 1;
        Ok(Node {
            label: format!("{}/ {:?}", name(name_), stat.entry_type),
            children: Vec::new(),
        })
    }

    fn set_read_error(&mut self, directory: &mut Node) -> io::Result<()> {
        self.calls += 1;
        directory.label.push_str(" ReadError");
        Ok(())
    }

    fn finish_directory(&mut self, directory: Node, parent: Option<&mut Node>) -> io::Result<()> {
        self.calls += 1;
        match parent {
            Some(parent) => parent.children.push(directory),
            None => self.root = Some(directory),
        }
        Ok(())
    }
}

/// A temporary directory with its canonical path, which is what ncdu scans after `realpath`.
struct Fixture {
    _directory: TempDir,
    root: PathBuf,
    restricted: Vec<PathBuf>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for path in &self.restricted {
            let _ = fs::set_permissions(path, Permissions::from_mode(0o755));
        }
    }
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new().expect("temporary directory is created");
        let root = fs::canonicalize(directory.path()).expect("temporary directory exists");
        Self {
            _directory: directory,
            root,
            restricted: Vec::new(),
        }
    }

    fn path(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }

    fn write(&self, path: &str, contents: &[u8]) {
        let path = self.path(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory is created");
        }
        fs::write(path, contents).expect("file is written");
    }

    fn directory(&self, path: &str) {
        fs::create_dir_all(self.path(path)).expect("directory is created");
    }

    fn symlink(&self, path: &str, target: &str) {
        symlink(target, self.path(path)).expect("symlink is created");
    }

    fn hard_link(&self, path: &str, target: &str) {
        fs::hard_link(self.path(target), self.path(path)).expect("hard link is created");
    }

    fn chmod(&self, path: &str, mode: u32) {
        fs::set_permissions(self.path(path), Permissions::from_mode(mode))
            .expect("permissions are set");
    }

    fn restrict(&mut self, path: &str, mode: u32) {
        assert_not_root();
        self.chmod(path, mode);
        self.restricted.push(self.path(path));
    }

    fn c_path(&self) -> CString {
        CString::new(self.root.as_os_str().as_bytes()).expect("path has no NUL byte")
    }
}

fn assert_not_root() {
    assert!(
        !geteuid().is_root(),
        "restricted permissions do not apply to root, run this test as another user"
    );
}

fn record(fixture: &Fixture, options: ScanOptions, patterns: &[&[u8]]) -> Recorder {
    let mut exclude = ExcludePatterns::default();
    for pattern in patterns {
        exclude.add(pattern);
    }
    let mut recorder = Recorder::default();
    scan(&mut recorder, &fixture.c_path(), options, &exclude).expect("scan succeeds");
    recorder
}

fn render(fixture: &Fixture, options: ScanOptions, patterns: &[&[u8]]) -> String {
    let mut output = String::new();
    if let Some(mut root) = record(fixture, options, patterns).root {
        root.render(0, &mut output);
    }
    output
}

#[test]
fn tree() {
    let fixture = Fixture::new();
    fixture.write("file", b"hello");
    fixture.write("empty", b"");
    fixture.symlink("link", "file");
    fixture.write("hard1", b"hard");
    fixture.hard_link("hard2", "hard1");
    fixture.write("sub/nested", b"abc");
    fixture.directory("sub/empty");

    assert_snapshot!(render(&fixture, ScanOptions::default(), &[]), @r"
    [ROOT] Directory
      empty Regular size=0
      file Regular size=5
      hard1 HardLink size=4 links=2
      hard2 HardLink size=4 links=2
      link NonRegular size=4
      sub/ Directory
        empty/ Directory
        nested Regular size=3
    ");
}

#[test]
fn exclude_patterns() {
    let fixture = Fixture::new();
    fixture.write("excluded", b"x");
    fixture.write("sub/excluded/file", b"x");
    fixture.write("a.d", b"file");
    fixture.write("b.d/file", b"x");
    fixture.write("sub/c.d/file", b"x");
    fixture.write("kept", b"x");
    fixture.write("nested", b"x");
    fixture.write("sub/nested", b"x");
    let anchored = [fixture.root.as_os_str().as_bytes(), b"/sub/nested"].concat();

    assert_snapshot!(render(&fixture, ScanOptions::default(), &[b"excluded", b"*.d/", &anchored]), @r"
    [ROOT] Directory
      a.d Regular size=4
      b.d Pattern
      excluded Pattern
      kept Regular size=1
      nested Regular size=1
      sub/ Directory
        c.d Pattern
        excluded Pattern
        nested Pattern
    ");
}

#[test]
fn follow_symlinks() {
    let fixture = Fixture::new();
    fixture.write("file", b"hello");
    fixture.write("hard1", b"hard");
    fixture.hard_link("hard2", "hard1");
    fixture.directory("sub");
    fixture.symlink("to_file", "file");
    fixture.symlink("to_hard", "hard1");
    fixture.symlink("to_sub", "sub");
    fixture.symlink("to_null", "/dev/null");
    fixture.symlink("dangling", "missing");

    let options = ScanOptions {
        follow_symlinks: true,
        ..ScanOptions::default()
    };
    assert_snapshot!(render(&fixture, options, &[]), @r"
    [ROOT] Directory
      dangling NonRegular size=7
      file Regular size=5
      hard1 HardLink size=4 links=2
      hard2 HardLink size=4 links=2
      sub/ Directory
      to_file Regular size=5
      to_hard HardLink size=4 links=2
      to_null NonRegular size=0
      to_sub NonRegular size=3
    ");
}

/// `/dev/null` is on devfs or devtmpfs, so a followed symlink to it is on another filesystem.
#[test]
fn same_filesystem() {
    let fixture = Fixture::new();
    fixture.symlink("to_null", "/dev/null");
    fixture.write("sub/file", b"x");

    let same_filesystem = ScanOptions {
        same_filesystem: true,
        ..ScanOptions::default()
    };
    assert_snapshot!(render(&fixture, same_filesystem, &[]), @r"
    [ROOT] Directory
      sub/ Directory
        file Regular size=1
      to_null NonRegular size=9
    ");

    let follow = ScanOptions {
        same_filesystem: true,
        follow_symlinks: true,
        ..ScanOptions::default()
    };
    assert_snapshot!(render(&fixture, follow, &[]), @r"
    [ROOT] Directory
      sub/ Directory
        file Regular size=1
      to_null OtherFilesystem
    ");
}

const CACHEDIR_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

#[test]
fn exclude_caches() {
    let fixture = Fixture::new();
    fixture.write(
        "cache/CACHEDIR.TAG",
        &[CACHEDIR_TAG, b"\n# This file is a cache directory tag.\n"].concat(),
    );
    fixture.write("cache/file", b"x");
    fixture.write(
        "short/CACHEDIR.TAG",
        &CACHEDIR_TAG[..CACHEDIR_TAG.len() - 1],
    );
    fixture.write("short/file", b"x");

    let options = ScanOptions {
        exclude_caches: true,
        ..ScanOptions::default()
    };
    assert_snapshot!(render(&fixture, options, &[]), @r"
    [ROOT] Directory
      cache Pattern
      short/ Directory
        CACHEDIR.TAG Regular size=42
        file Regular size=1
    ");
    assert_snapshot!(render(&fixture, ScanOptions::default(), &[]), @r"
    [ROOT] Directory
      cache/ Directory
        CACHEDIR.TAG Regular size=82
        file Regular size=1
      short/ Directory
        CACHEDIR.TAG Regular size=42
        file Regular size=1
    ");
}

#[test]
fn permission_denied() {
    let mut fixture = Fixture::new();
    fixture.write("unreadable/file", b"x");
    fixture.write("unsearchable/file", b"x");
    fixture.write("unsearchable/excluded", b"x");
    fixture.directory("unsearchable/x.d");
    fixture.restrict("unreadable", 0o000);
    fixture.restrict("unsearchable", 0o444);

    assert_snapshot!(render(&fixture, ScanOptions::default(), &[b"excluded", b"*.d/"]), @r"
    [ROOT] Directory
      unreadable/ Directory ReadError
      unsearchable/ Directory
        excluded Pattern
        file ReadError
        x.d ReadError
    ");
}

#[test]
fn root_errors() {
    let fixture = Fixture::new();
    fixture.write("file", b"x");
    for path in [fixture.path("missing"), fixture.path("file")] {
        let mut recorder = Recorder::default();
        let path = CString::new(path.as_os_str().as_bytes()).expect("path has no NUL byte");
        let result = scan(
            &mut recorder,
            &path,
            ScanOptions::default(),
            &ExcludePatterns::default(),
        );
        assert!(result.is_err());
        assert_eq!(recorder.calls, 0);
    }
}

#[test]
fn extended() {
    let fixture = Fixture::new();
    fixture.write("file", b"x");
    fixture.chmod("file", 0o600);
    File::options()
        .write(true)
        .open(fixture.path("file"))
        .expect("file is opened")
        .set_times(
            FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)),
        )
        .expect("modification time is set");
    let metadata = fs::metadata(fixture.path("file")).expect("file exists");

    let recorder = record(&fixture, ScanOptions::default(), &[]);
    assert_eq!(
        recorder
            .stats
            .iter()
            .find(|(name, _)| name == "file")
            .map(|(_, stat)| stat.extended),
        Some(Extended {
            mtime: Some(1_000_000),
            uid: Some(metadata.uid()),
            gid: Some(metadata.gid()),
            mode: Some(0o100_600),
        })
    );
}

#[cfg(feature = "test-ncdu")]
fn assert_ncdu(fixture: &Fixture, flags: &[&str], options: ScanOptions, patterns: &[&[u8]]) {
    let result = Command::new("ncdu")
        .args(["--ignore-config", "-o-"])
        .args(flags)
        .arg(&fixture.root)
        .output()
        .expect("ncdu 2.9.2 is on PATH");
    assert!(
        result.status.success(),
        "ncdu failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let mut exclude = ExcludePatterns::default();
    for pattern in patterns {
        exclude.add(pattern);
    }
    let mut export =
        JsonExport::new(Vec::new(), 0, flags.contains(&"-e"), None).expect("header is written");
    scan(&mut export, &fixture.c_path(), options, &exclude).expect("scan succeeds");
    let output = export.finish().expect("export is flushed");

    let without_metadata = |export: &[u8]| -> Vec<u8> {
        let start = export.iter().position(|&byte| byte == b'\n').unwrap_or(0);
        export[start..].to_vec()
    };
    assert_eq!(
        String::from_utf8_lossy(&without_metadata(&result.stdout)),
        String::from_utf8_lossy(&without_metadata(&output)),
    );
    assert_eq!(without_metadata(&result.stdout), without_metadata(&output));
}

#[cfg(feature = "test-ncdu")]
#[test]
fn ncdu() {
    let mut fixture = Fixture::new();
    fixture.write("file", b"hello");
    fixture.write("empty", b"");
    fixture.write("hard1", b"hard");
    fixture.hard_link("hard2", "hard1");
    fixture.symlink("to_file", "file");
    fixture.symlink("to_hard", "hard1");
    fixture.symlink("to_sub", "sub");
    fixture.symlink("to_null", "/dev/null");
    fixture.symlink("dangling", "missing");
    fixture.write("nested", b"x");
    fixture.write("sub/nested", b"abc");
    fixture.write("sub/excluded", b"x");
    fixture.write("sub/c.d/file", b"x");
    fixture.directory("sub/empty");
    fixture.write("a.d", b"file");
    fixture.write("b.d/file", b"x");
    fixture.write("cache/CACHEDIR.TAG", CACHEDIR_TAG);
    fixture.write(
        "short/CACHEDIR.TAG",
        &CACHEDIR_TAG[..CACHEDIR_TAG.len() - 1],
    );
    fixture.write("unreadable/file", b"x");
    fixture.write("unsearchable/file", b"x");
    fixture.write("unsearchable/excluded", b"x");
    fixture.directory("unsearchable/x.d");
    fixture.restrict("unreadable", 0o000);
    fixture.restrict("unsearchable", 0o444);
    let anchored = format!("{}/sub/nested", fixture.root.display());

    assert_ncdu(&fixture, &["-e"], ScanOptions::default(), &[]);
    assert_ncdu(
        &fixture,
        &["-x"],
        ScanOptions {
            same_filesystem: true,
            ..ScanOptions::default()
        },
        &[],
    );
    assert_ncdu(
        &fixture,
        &[
            "-L",
            "-x",
            "--exclude-caches",
            "--exclude",
            "excluded",
            "--exclude",
            "*.d/",
            "--exclude",
            &anchored,
        ],
        ScanOptions {
            same_filesystem: true,
            follow_symlinks: true,
            exclude_kernfs: false,
            exclude_caches: true,
            threads: 1,
        },
        &[b"excluded", b"*.d/", anchored.as_bytes()],
    );
}

fn scan_tree(fixture: &Fixture, options: ScanOptions) -> Tree {
    let mut memory = MemorySink::new(false);
    scan(
        &mut memory,
        &fixture.c_path(),
        options,
        &ExcludePatterns::default(),
    )
    .expect("scan succeeds");
    memory.finish(&mut |_, _| {})
}

fn refresh_tree(tree: Tree, path: &[&[u8]], options: ScanOptions) -> Tree {
    let mut directory = tree.root().expect("tree has a root");
    for name in path {
        directory = tree
            .entries(directory)
            .find_map(|entry| match tree.entry(entry).kind {
                EntryKind::Directory(child) if tree.name(entry) == *name => Some(child),
                EntryKind::Directory(_)
                | EntryKind::HardLink(_)
                | EntryKind::Regular
                | EntryKind::NonRegular
                | EntryKind::Special(_) => None,
            })
            .expect("directory exists");
    }
    let mut full_path = Vec::new();
    tree.format_path(directory, true, &mut full_path);
    let full_path = CString::new(full_path).expect("path has no NUL byte");
    let mut memory = MemorySink::refresh(tree, directory, false);
    scan(
        &mut memory,
        &full_path,
        options,
        &ExcludePatterns::default(),
    )
    .expect("scan succeeds");
    memory.finish(&mut |_, _| {})
}

fn summary(tree: &Tree) -> Vec<String> {
    fn walk(tree: &Tree, directory: DirectoryId, path: &str, lines: &mut Vec<String>) {
        let data = tree.directory(directory);
        let entry = tree.entry(data.entry);
        lines.push(format!(
            "{path}/ asize={} blocks={} items={} shared_asize={} shared_blocks={} errors={} {}",
            entry.size,
            entry.blocks,
            data.items,
            data.shared_size,
            data.shared_blocks,
            data.read_error,
            data.sub_error,
        ));
        for id in tree.entries(directory) {
            let path = format!("{path}/{}", name(tree.name(id)));
            let entry = tree.entry(id);
            match entry.kind {
                EntryKind::Directory(child) => walk(tree, child, &path, lines),
                EntryKind::HardLink(link) => lines.push(format!(
                    "{path} link asize={} blocks={} nlink={}",
                    entry.size,
                    entry.blocks,
                    tree.link(link).link_count()
                )),
                kind @ (EntryKind::Regular | EntryKind::NonRegular | EntryKind::Special(_)) => {
                    lines.push(format!(
                        "{path} {kind:?} asize={} blocks={}",
                        entry.size, entry.blocks
                    ));
                }
            }
        }
    }
    let mut lines = Vec::new();
    walk(tree, tree.root().expect("tree has a root"), "", &mut lines);
    lines.sort();
    lines
}

#[test]
fn refresh() {
    let fixture = Fixture::new();
    fixture.write("other", &[0; 3000]);
    fixture.write("top/sub/keep", &[0; 5000]);
    fixture.write("top/sub/gone", &[0; 7000]);
    fixture.write("top/sub/grow", &[0; 100]);
    fixture.write("top/sub/swap", &[0; 9000]);
    fixture.write("top/sub/nested/old", &[0; 11_000]);
    let trees = [1, 4].map(|threads| {
        let options = ScanOptions {
            threads,
            ..ScanOptions::default()
        };
        (options, scan_tree(&fixture, options))
    });

    fs::remove_file(fixture.path("top/sub/gone")).expect("file is removed");
    fixture.write("top/sub/grow", &[0; 20_000]);
    fs::remove_file(fixture.path("top/sub/swap")).expect("file is removed");
    fixture.write("top/sub/swap/inside", &[0; 13_000]);
    fs::remove_file(fixture.path("top/sub/nested/old")).expect("file is removed");
    fixture.write("top/sub/nested/new", &[0; 15_000]);
    fixture.write("top/sub/added", &[0; 17_000]);
    for (options, tree) in trees {
        let refreshed = refresh_tree(tree, &[b"top", b"sub"], options);
        assert_eq!(
            summary(&refreshed),
            summary(&scan_tree(&fixture, ScanOptions::default())),
            "{options:?}"
        );
    }
}

#[test]
fn refresh_hard_links() {
    let fixture = Fixture::new();
    fixture.write("outside", &[0; 5000]);
    fixture.write("sub/first", &[0; 7000]);
    fixture.hard_link("sub/inside", "outside");
    fixture.hard_link("sub/second", "sub/first");
    fixture.write("sub/third", &[0; 9000]);
    let trees = [1, 4].map(|threads| {
        let options = ScanOptions {
            threads,
            ..ScanOptions::default()
        };
        (options, scan_tree(&fixture, options))
    });
    fixture.hard_link("sub/fourth", "sub/third");
    let expected = summary(&scan_tree(&fixture, ScanOptions::default()));
    for (options, tree) in trees {
        let refreshed = refresh_tree(tree, &[b"sub"], options);
        assert_eq!(summary(&refreshed), expected, "{options:?}");
        let refreshed = refresh_tree(refreshed, &[], options);
        assert_eq!(summary(&refreshed), expected, "{options:?}");
    }
}

#[test]
fn threads() {
    let mut fixture = Fixture::new();
    for index in 0..40 {
        fixture.write(&format!("d{index:02}/file"), &vec![0; 1000 * (index + 1)]);
        fixture.write(&format!("d{index:02}/sub/nested"), b"x");
    }
    fixture.hard_link("d01/link", "d00/file");
    for index in 0..200 {
        fixture.write(&format!("wide/f{index:03}"), b"x");
    }
    for index in 0..4 {
        fixture.write(&format!("wide/s{index}/file"), b"x");
    }
    fixture.write("cache/CACHEDIR.TAG", CACHEDIR_TAG);
    fixture.write("unreadable/file", b"x");
    fixture.restrict("unreadable", 0o000);
    for exclude_caches in [false, true] {
        let one = ScanOptions {
            exclude_caches,
            ..ScanOptions::default()
        };
        let four = ScanOptions { threads: 4, ..one };
        assert_eq!(
            summary(&scan_tree(&fixture, four)),
            summary(&scan_tree(&fixture, one)),
            "exclude_caches: {exclude_caches}"
        );
    }
}

#[test]
fn thread_directories() {
    let fixture = Fixture::new();
    // Each directory takes more than one part, so the threads still read when the calling thread
    // stops to look.
    for index in 0..24 {
        for file in 0..100 {
            fixture.write(&format!("d{index:02}/file{file}"), b"x");
        }
    }
    let mut first = None;
    let mut reading = None;
    let mut record = |progress: &Progress| {
        if first.is_none() {
            first = Some(progress.directories());
        } else if reading.is_none() && progress.files() > 0 {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut directories = progress.directories();
            while directories.iter().any(Option::is_none) && Instant::now() < deadline {
                thread::yield_now();
                directories = progress.directories();
            }
            reading = Some(directories);
        }
        Ok(())
    };
    let mut sink = ProgressSink::new(MemorySink::new(false), Producer::Scan, &mut record);
    let options = ScanOptions {
        threads: 3,
        ..ScanOptions::default()
    };
    scan(
        &mut sink,
        &fixture.c_path(),
        options,
        &ExcludePatterns::default(),
    )
    .expect("scan succeeds");
    drop(sink);
    assert_eq!(first, Some(vec![None; 3]));
    let directories: Vec<Vec<u8>> = (0..24)
        .map(|index| {
            fixture
                .path(&format!("d{index:02}"))
                .as_os_str()
                .as_bytes()
                .to_vec()
        })
        .collect();
    let reading = reading.expect("the root is reported");
    assert_eq!(reading.len(), 3);
    for directory in reading {
        let directory = directory.expect("each thread reads a directory");
        assert!(directories.contains(&directory), "{}", name(&directory));
    }
}
