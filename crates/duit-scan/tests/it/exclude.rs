use duit_scan::{DirectoryPatterns, ExcludePatterns, Excluded};

fn exclude(patterns: &[&[u8]]) -> ExcludePatterns {
    let mut exclude = ExcludePatterns::default();
    for pattern in patterns {
        exclude.add(pattern);
    }
    exclude
}

/// The checks that upstream runs inside `/foo`.
fn assert_foo(patterns: &DirectoryPatterns<'_>) {
    assert_eq!(patterns.matches(c"root"), Excluded::No);
    assert_eq!(patterns.matches(c"bar"), Excluded::Always);
    assert_eq!(patterns.matches(c"qoo"), Excluded::Always);
    assert_eq!(patterns.matches(c"xyz"), Excluded::Always);
    assert_eq!(patterns.matches(c"okay"), Excluded::No);
    assert_eq!(patterns.matches(c"somefile"), Excluded::Always);
    let okay = patterns.enter(c"okay");
    assert_eq!(okay.matches(c"bar"), Excluded::No);
    assert_eq!(okay.matches(c"xyz"), Excluded::No);
    assert_eq!(okay.matches(c"notokay"), Excluded::Always);
}

/// Port of the upstream test "Matching".
#[test]
fn matching() {
    let exclude = exclude(&[
        b"/foo/bar",
        b"/foo/qoo/",
        b"/foo/qoo",
        b"/foo/qoo/",
        b"/f??/xyz",
        b"/f??/xyz/",
        b"/*o/somefile",
        b"/a??/okay",
        b"/roo?",
        b"/root/",
        b"excluded",
        b"somefile/",
        b"o*y/not[o]kay",
    ]);

    let root = exclude.for_path(c"/");
    assert_eq!(root.matches(c"a"), Excluded::No);
    assert_eq!(root.matches(c"excluded"), Excluded::Always);
    assert_eq!(root.matches(c"somefile"), Excluded::IfDirectory);
    assert_eq!(root.matches(c"root"), Excluded::Always);
    assert_foo(&root.enter(c"foo"));

    let somewhere = exclude.for_path(c"/somedir/somewhere");
    assert_eq!(somewhere.matches(c"a"), Excluded::No);
    assert_eq!(somewhere.matches(c"excluded"), Excluded::Always);
    assert_eq!(somewhere.matches(c"root"), Excluded::No);
    assert_eq!(somewhere.matches(c"okay"), Excluded::No);
    let okay = somewhere.enter(c"okay");
    assert_eq!(okay.matches(c"excluded"), Excluded::Always);
    assert_eq!(okay.matches(c"okay"), Excluded::No);
    assert_eq!(okay.matches(c"notokay"), Excluded::Always);

    assert_foo(&exclude.for_path(c"/foo/"));
}

/// `fnmatch` runs without `FNM_PERIOD`, so a wildcard matches a leading dot.
#[test]
fn wildcard_matches_dot() {
    let exclude = exclude(&[b"*"]);
    assert_eq!(exclude.for_path(c"/").matches(c".hidden"), Excluded::Always);
}

/// A backslash makes a component a wildcard, in which it escapes the next character.
#[test]
fn backslash_escapes() {
    let exclude = exclude(&[b"a\\b"]);
    let root = exclude.for_path(c"/");
    assert_eq!(root.matches(c"ab"), Excluded::Always);
    assert_eq!(root.matches(c"a\\b"), Excluded::No);
}

#[test]
fn empty_pattern() {
    let exclude = exclude(&[b""]);
    assert_eq!(exclude.for_path(c"/").matches(c""), Excluded::No);
}

/// `fnmatch` reads a wildcard up to its first NUL byte, which a line of an exclude file may
/// contain. A literal with a NUL byte matches no name.
#[test]
fn nul_byte() {
    let exclude = exclude(&[b"a\0*", b"b\0c"]);
    let root = exclude.for_path(c"/");
    assert_eq!(root.matches(c"a"), Excluded::Always);
    assert_eq!(root.matches(c"ab"), Excluded::No);
    assert_eq!(root.matches(c"b"), Excluded::No);
}
