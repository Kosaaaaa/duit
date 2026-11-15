use std::ffi::CString;
use std::fmt::Write as _;
use std::fs::{self, File, FileTimes, Permissions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::PathBuf;
#[cfg(feature = "test-ncdu")]
use std::process::Command;
use std::time::{Duration, SystemTime};

#[cfg(feature = "test-ncdu")]
use duit_format::JsonExport;
use duit_model::{EntryType, Extended, Sink, Special, Stat};
use duit_scan::{ExcludePatterns, ScanOptions, scan};
use insta::assert_snapshot;
use tempfile::TempDir;

/// A directory as the [`Recorder`] receives it.
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
    /// Directories whose permissions are restored on drop, so that the [`TempDir`] can be
    /// removed.
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

    /// Set the permissions of the directory `path` until the fixture is dropped.
    fn restrict(&mut self, path: &str, mode: u32) {
        self.chmod(path, mode);
        self.restricted.push(self.path(path));
    }

    fn c_path(&self) -> CString {
        CString::new(self.root.as_os_str().as_bytes()).expect("path has no NUL byte")
    }
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

/// A pattern that matches any entry applies before `stat`, and a pattern for directories only
/// after it. Assumes that the tests do not run as root, which ignores permissions.
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

/// The root is checked before the sink sees anything.
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

/// Assert that `scan` into a [`JsonExport`] writes the same items as `ncdu -o-` on `fixture`.
/// The first line holds the metadata block, which differs between the two programs.
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

    let items = |export: &[u8]| -> Vec<u8> {
        let start = export.iter().position(|&byte| byte == b'\n').unwrap_or(0);
        export[start..].to_vec()
    };
    assert_eq!(
        String::from_utf8_lossy(&items(&result.stdout)),
        String::from_utf8_lossy(&items(&output)),
    );
    assert_eq!(items(&result.stdout), items(&output));
}

/// Both programs scan the same tree, so that inodes and the order of `readdir` are equal.
/// Assumes that the tests do not run as root, which ignores permissions.
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
        },
        &[b"excluded", b"*.d/", anchored.as_bytes()],
    );
}
