use std::fmt::Write as _;
use std::io::{self, Read};

use duit_format::{ImportError, JsonExport, import};
use duit_model::{EntryType, Extended, Sink, Special, Stat};
use insta::assert_snapshot;

use crate::json_export::{directory, file, many_files};
#[cfg(feature = "test-ncdu")]
use crate::ncdu_import;

fn reexport(input: impl Read, extended: bool) -> Result<Vec<u8>, ImportError> {
    let mut input = input;
    let mut head = [0; 8];
    input.read_exact(&mut head).expect("input has 8 bytes");
    let mut export = JsonExport::new(Vec::new(), 0, extended, None).expect("header is written");
    import(&mut export, input, &head)?;
    Ok(export.finish().expect("export is flushed"))
}

fn export(
    extended: bool,
    compress_level: Option<u8>,
    build: impl FnOnce(&mut JsonExport<Vec<u8>>) -> io::Result<()>,
) -> Vec<u8> {
    let mut export =
        JsonExport::new(Vec::new(), 0, extended, compress_level).expect("header is written");
    build(&mut export).expect("items are written");
    export.finish().expect("export is flushed")
}

fn tree(export: &mut JsonExport<Vec<u8>>) -> io::Result<()> {
    let mut root = export.create_root(b"/root", &directory(1))?;
    export.add_stat(&mut root, b"file", &file(100, 8))?;
    export.add_stat(
        &mut root,
        b"quote\" backslash\\ \n\r\t\x08\x0c \x01\x1f\x7f \xc3\xa9 \xff\xfe",
        &file(1, 1),
    )?;
    let link = Stat {
        entry_type: EntryType::HardLink,
        inode: 42,
        link_count: 3,
        ..file(10, 8)
    };
    export.add_stat(&mut root, b"link", &link)?;
    let fifo = Stat {
        entry_type: EntryType::NonRegular,
        ..file(0, 0)
    };
    export.add_stat(&mut root, b"fifo", &fifo)?;
    let extended = Stat {
        extended: Extended {
            mtime: Some(1_700_000_000),
            uid: Some(501),
            gid: None,
            mode: Some(0o100_644),
        },
        ..file(1, 1)
    };
    export.add_stat(&mut root, b"extended", &extended)?;
    export.add_special(&mut root, b"error", Special::ReadError)?;
    export.add_special(&mut root, b"pattern", Special::Pattern)?;
    export.add_special(&mut root, b"otherfs", Special::OtherFilesystem)?;
    export.add_special(&mut root, b"kernfs", Special::KernelFilesystem)?;
    let mut unreadable = export.add_directory(&mut root, b"unreadable", &directory(1))?;
    export.set_read_error(&mut unreadable)?;
    export.add_stat(&mut unreadable, b"file", &file(1, 1))?;
    export.finish_directory(unreadable, Some(&mut root))?;
    let mut other = export.add_directory(&mut root, b"other", &directory(7))?;
    let mut nested = export.add_directory(&mut other, b"nested", &directory(7))?;
    let empty = export.add_directory(&mut nested, b"empty", &directory(7))?;
    export.finish_directory(empty, Some(&mut nested))?;
    export.finish_directory(nested, Some(&mut other))?;
    let back = export.add_directory(&mut other, b"back", &directory(1))?;
    export.finish_directory(back, Some(&mut other))?;
    export.finish_directory(other, Some(&mut root))?;
    export.finish_directory(root, None)
}

#[test]
fn round_trip() {
    for extended in [false, true] {
        let tree = export(extended, None, tree);
        assert_eq!(
            reexport(tree.as_slice(), extended).expect("tree is imported"),
            tree
        );
    }
    let many_files = export(false, None, many_files);
    assert_eq!(
        reexport(many_files.as_slice(), false).expect("files are imported"),
        many_files
    );
}

#[test]
fn round_trip_compressed() {
    for build in [tree, many_files] {
        let plain = export(true, None, build);
        let compressed = export(true, Some(3), build);
        assert_eq!(
            reexport(compressed.as_slice(), true).expect("export is imported"),
            plain
        );

        let (first, second) = plain.split_at(plain.len() / 2);
        let mut frames = zstd::encode_all(first, 3).expect("first frame is compressed");
        frames.extend(zstd::encode_all(second, 3).expect("second frame is compressed"));
        assert_eq!(
            reexport(frames.as_slice(), true).expect("frames are imported"),
            plain
        );
    }
}

#[test]
fn items() {
    let input: &[u8] =
        br#"[1,1,{"progname":"x","nested":{"a":[1,-2.5e+3,true,false,null,"s\""],"b":{}}},
[{"name":"/root","dev":3,"unknown":{"name":"ignored","list":[[],{}]},"asize":4096},
{"name":"mtime","mtime":1700000000.75,"uid":0,"gid":4294967295,"mode":33188},
{"name":"mtime-dot","mtime":5.,"mode":72820},
{"name":"dsize","dsize":1023},
{"name":"nlink","nlink":2,"ino":5},
{"name":"single-link","hlnkc":false,"nlink":1},
{"name":"excluded-then-nlink","excluded":"pattern","nlink":3},
{"name":"nlink-then-excluded","nlink":3,"excluded":"pattern"},
{"name":"othfs","excluded":"othfs"},
{"name":"kernfs","excluded":"kernfs"},
{"name":"frmlnk","excluded":"frmlnk"},
{"name":"unknown","excluded":"unknown"},
{"name":"read-error","read_error":true},
{"name":"no-read-error","read_error":false},
{"name":"notreg","notreg":true},
{"name":"hlnkc-then-notreg","hlnkc":true,"notreg":true},
[{"name":"unreadable","read_error":true,"dev":3},{"name":"inside"}],
[{"name":"directory-notreg","notreg":true}],
[{"name":"directory-hlnkc","hlnkc":true,"ino":9,"nlink":1}],
[{"name":"directory-excluded","excluded":"otherfs"}],
[{"name":"other-device","dev":4},[{"name":"inherits"}]],
{"name":"","name":"second-name"},
{ "name" : "spaces" , "asize" : 1 } ,
{"name":"escapes \" \\ \/ \b \f \n \r \t \u00e9 \ud83d\ude00 \u0000 \u001F end"},
{"name":"zero","asize":0,"dsize":0},
"#;
    let input = [
        input,
        b"{\"name\":\"raw \xff\xfe bytes\"}\n],\n\"trailing\",{\"elements\":[1]}]\n",
    ]
    .concat();
    let input = input.as_slice();
    let output = reexport(input, true).expect("items are imported");
    #[cfg(feature = "test-ncdu")]
    {
        let ncdu = ncdu_import(input, &["-o-", "-e"]);
        assert!(ncdu.status.success());
        let items = |export: &[u8]| -> Vec<u8> {
            let start = export.iter().position(|&byte| byte == b'\n').unwrap_or(0);
            export[start..].to_vec()
        };
        assert_eq!(
            String::from_utf8_lossy(&items(&ncdu.stdout)),
            String::from_utf8_lossy(&items(&output))
        );
        assert_eq!(items(&ncdu.stdout), items(&output));
    }
    assert_snapshot!(String::from_utf8_lossy(&output).replace(env!("CARGO_PKG_VERSION"), "[VERSION]"), @r#"
    [1,2,{"progname":"duit","progver":"[VERSION]","timestamp":0},
    [{"name":"/root","asize":4096,"dev":3},
    {"name":"mtime","uid":0,"gid":4294967295,"mode":33188,"mtime":1700000000},
    {"name":"mtime-dot","mode":7284,"mtime":5},
    {"name":"dsize","dsize":512},
    {"name":"nlink","ino":5,"hlnkc":true,"nlink":2},
    {"name":"single-link"},
    {"name":"excluded-then-nlink","ino":0,"hlnkc":true,"nlink":3},
    {"name":"nlink-then-excluded","excluded":"pattern"},
    [{"name":"othfs","excluded":"otherfs"}],
    [{"name":"kernfs","excluded":"kernfs"}],
    {"name":"frmlnk","excluded":"pattern"},
    {"name":"unknown","excluded":"pattern"},
    {"name":"read-error","read_error":true},
    {"name":"no-read-error"},
    {"name":"notreg","notreg":true},
    {"name":"hlnkc-then-notreg","notreg":true},
    [{"name":"unreadable","read_error":true},
    {"name":"inside"}],
    {"name":"directory-notreg","notreg":true},
    {"name":"directory-hlnkc","ino":9,"hlnkc":true,"nlink":1},
    [{"name":"directory-excluded","excluded":"otherfs"}],
    [{"name":"other-device","dev":4},
    [{"name":"inherits"}]],
    {"name":"second-name"},
    {"name":"spaces","asize":1},
    {"name":"escapes \" \\ / \b \f \n \r \t é 😀 \u0000 \u001f end"},
    {"name":"zero"},
    {"name":"raw �� bytes"}]]
    "#);
}

fn result(input: impl Read) -> String {
    match reexport(input, false) {
        Ok(_) => "ok".to_string(),
        Err(ImportError::Input {
            line,
            byte,
            message,
        }) => format!("{line}:{byte}: {message}"),
        Err(ImportError::Sink(error)) => format!("sink error: {error}"),
    }
}

#[cfg(feature = "test-ncdu")]
fn ncdu_result(input: &[u8]) -> String {
    let output = ncdu_import(input, &["-o-"]);
    if output.status.success() {
        return "ok".to_string();
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr
        .strip_prefix("\x1b[JError importing file on line ")
        .and_then(|rest| rest.strip_suffix(".\n"))
        .unwrap_or(&stderr)
        .to_string()
}

fn label(input: &[u8]) -> String {
    let input = String::from_utf8_lossy(input).escape_debug().to_string();
    if input.len() > 80 {
        format!("{}...", input.chars().take(60).collect::<String>())
    } else {
        input
    }
}

#[test]
fn malformed() {
    let long_name = [
        &br#"[1,0,{},[{"name":""#[..],
        &[b'a'; 32 * 1024 - 4],
        br#""}]]"#,
    ]
    .concat();
    let truncated = export(false, Some(3), tree);
    let cases: &[&[u8]] = &[
        b"abcdefgh",
        br#"[2,0,{},[{"name":"a"}]]"#,
        b"[1]      ",
        b"[1,0]    ",
        b"[1,0,[]] ",
        b"[1,0,{}] ",
        br#"[1,0,{"a" 1},[{"name":"a"}]]"#,
        br#"[1,0,{"a":[1,]},[{"name":"a"}]]"#,
        br#"[1,0,{"a":tx},[{"name":"a"}]]"#,
        br#"[1,0,{},{"name":"a"}]"#,
        br#"[1,0,{},"x"]"#,
        b"[1,0,{},[1]]",
        br#"[1,0,{},[{"name":"a"},2]]"#,
        br#"[1,0,{},[{"name":"a"},]]"#,
        b"[1,0,{},[{}]]",
        br#"[1,0,{},[{"name":""}]]"#,
        br#"[1,0,{},[{"name":"a","name":"b"}]]"#,
        br#"[1,0,{},[{"name":"a",}]]"#,
        br#"[1,0,{},[{"name":"a" "asize":1}]]"#,
        br#"[1,0,{},[{"name":"a","asize":-1}]]"#,
        br#"[1,0,{},[{"name":"a","asize":"1"}]]"#,
        br#"[1,0,{},[{"name":"a","asize":007}]]"#,
        br#"[1,0,{},[{"name":"a","asize":1.5}]]"#,
        br#"[1,0,{},[{"name":"a","asize":18446744073709551616}]]"#,
        br#"[1,0,{},[{"name":"a","mode":65536}]]"#,
        br#"[1,0,{},[{"name":"a","nlink":2147483648}]]"#,
        br#"[1,0,{},[{"name":"a","uid":4294967296}]]"#,
        br#"[1,0,{},[{"name":"a","hlnkc":1}]]"#,
        br#"[1,0,{},[{"name":"a","hlnkc":tx}]]"#,
        br#"[1,0,{},[{"name":"a","excluded":1}]]"#,
        b"[1,0,{},[{\"name\":\"a\x01\"}]]",
        br#"[1,0,{},[{"name":"a\q"}]]"#,
        br#"[1,0,{},[{"name":"\u12g4"}]]"#,
        br#"[1,0,{},[{"name":"\udc00"}]]"#,
        br#"[1,0,{},[{"name":"\ud800\u0041"}]]"#,
        br#"[1,0,{},[{"name":"\ud800x"}]]"#,
        &long_name,
        br#"[1,0,{},[{"name":"d"},[{"name":"x","excluded":"kernfs"},{"name":"y"}]]]"#,
        br#"[1,0,{},[{"name":"a"}]] x"#,
        b"[1,0,{},[{\"name\":\"a\"}]]\0trailing",
        br#"[1,0,{},[{"name":"a"}]"#,
        br#"[1,0,{},[{"name":"a","asize":1"#,
        br#"[1,0,{},[{"name":"a"#,
        b"[1,0,{},\n[{\"name\":\"a\"},\n  {\"name\":\"b\",\r\n\t\"asize\":x}]]",
        b"\x28\xb5\x2f\xfdjunkjunk",
        &truncated[..truncated.len() - 1],
    ];
    let mut rendered = String::new();
    for &input in cases {
        let result = result(input);
        #[cfg(feature = "test-ncdu")]
        assert_eq!(ncdu_result(input), result, "input: {}", label(input));
        let _ = writeln!(rendered, "{}\n  {result}", label(input));
    }
    assert_snapshot!(rendered, @r#"
    abcdefgh
      1:2: expected array
    [2,0,{},[{\"name\":\"a\"}]]
      1:3: incompatible major format version
    [1]      
      1:4: expected array element
    [1,0]    
      1:6: expected array element
    [1,0,[]] 
      1:7: expected object
    [1,0,{}] 
      1:9: expected array element
    [1,0,{\"a\" 1},[{\"name\":\"a\"}]]
      1:12: invalid JSON
    [1,0,{\"a\":[1,]},[{\"name\":\"a\"}]]
      1:15: invalid JSON
    [1,0,{\"a\":tx},[{\"name\":\"a\"}]]
      1:13: invalid JSON
    [1,0,{},{\"name\":\"a\"}]
      1:10: parent item must be a directory
    [1,0,{},\"x\"]
      1:10: expected object or array
    [1,0,{},[1]]
      1:11: expected object
    [1,0,{},[{\"name\":\"a\"},2]]
      1:24: expected object or array
    [1,0,{},[{\"name\":\"a\"},]]
      1:24: expected object or array
    [1,0,{},[{}]]
      1:12: missing "name" field
    [1,0,{},[{\"name\":\"\"}]]
      1:21: missing "name" field
    [1,0,{},[{\"name\":\"a\",\"name\":\"b\"}]]
      1:29: duplicate key
    [1,0,{},[{\"name\":\"a\",}]]
      1:23: expected string
    [1,0,{},[{\"name\":\"a\" \"asize\":1}]]
      1:23: invalid JSON
    [1,0,{},[{\"name\":\"a\",\"asize\":-1}]]
      1:31: expected number
    [1,0,{},[{\"name\":\"a\",\"asize\":\"1\"}]]
      1:31: expected number
    [1,0,{},[{\"name\":\"a\",\"asize\":007}]]
      1:32: invalid JSON
    [1,0,{},[{\"name\":\"a\",\"asize\":1.5}]]
      1:32: invalid JSON
    [1,0,{},[{\"name\":\"a\",\"asize\":18446744073709551616}]]
      1:50: integer out of range
    [1,0,{},[{\"name\":\"a\",\"mode\":65536}]]
      1:34: integer out of range
    [1,0,{},[{\"name\":\"a\",\"nlink\":2147483648}]]
      1:40: integer out of range
    [1,0,{},[{\"name\":\"a\",\"uid\":4294967296}]]
      1:38: integer out of range
    [1,0,{},[{\"name\":\"a\",\"hlnkc\":1}]]
      1:31: expected boolean
    [1,0,{},[{\"name\":\"a\",\"hlnkc\":tx}]]
      1:32: invalid JSON
    [1,0,{},[{\"name\":\"a\",\"excluded\":1}]]
      1:34: expected string
    [1,0,{},[{\"name\":\"a\u{1}\"}]]
      1:21: invalid character in string
    [1,0,{},[{\"name\":\"a\\q\"}]]
      1:22: invalid escape sequence
    [1,0,{},[{\"name\":\"\\u12g4\"}]]
      1:24: invalid hex digit
    [1,0,{},[{\"name\":\"\\udc00\"}]]
      1:25: Unexpected low surrogate
    [1,0,{},[{\"name\":\"\\ud800\\u0041\"}]]
      1:31: Invalid low surrogate
    [1,0,{},[{\"name\":\"\\ud800x\"}]]
      1:26: invalid JSON
    [1,0,{},[{\"name\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa...
      1:32784: too long file name
    [1,0,{},[{\"name\":\"d\"},[{\"name\":\"x\",\"excluded\":\"ke...
      1:57: unexpected contents in an excluded directory
    [1,0,{},[{\"name\":\"a\"}]] x
      1:26: trailing garbage
    [1,0,{},[{\"name\":\"a\"}]]\0trailing
      ok
    [1,0,{},[{\"name\":\"a\"}]
      1:23: invalid JSON
    [1,0,{},[{\"name\":\"a\",\"asize\":1
      1:31: invalid JSON
    [1,0,{},[{\"name\":\"a
      1:20: invalid character in string
    [1,0,{},\n[{\"name\":\"a\"},\n  {\"name\":\"b\",\r\n\t\"asize\":x}]]
      4:11: expected number
    (�/�junkjunk
      1:1: decompression error
    (�/�\0X�\n\0&\u{11}9-��I\u{7}��n�]�����o�nI�\'{\u{13}\u{2}k햛...
      1:1: decompression error
    "#);
}

#[test]
fn unlike_ncdu() {
    let cases: &[&[u8]] = &[
        br#"[1,0,{},[{"name":"/r","excluded":"pattern"}]]"#,
        br#"[1,0,{},[{"name":"/r","notreg":true}]]"#,
        br#"[1,0,{},[{"name":"/r","hlnkc":true}]]"#,
    ];
    let mut rendered = String::new();
    for &input in cases {
        let _ = writeln!(rendered, "{}\n  {}", label(input), result(input));
    }
    assert_snapshot!(rendered, @r#"
    [1,0,{},[{\"name\":\"/r\",\"excluded\":\"pattern\"}]]
      1:44: parent item must be a directory
    [1,0,{},[{\"name\":\"/r\",\"notreg\":true}]]
      1:37: parent item must be a directory
    [1,0,{},[{\"name\":\"/r\",\"hlnkc\":true}]]
      1:36: parent item must be a directory
    "#);
}

#[test]
fn truncated() {
    for compress_level in [None, Some(3)] {
        let export = export(false, compress_level, tree);
        for length in 8..export.len() {
            let prefix = &export[..length];
            let complete = compress_level.is_none() && length == export.len() - 1;
            assert_eq!(
                reexport(prefix, false).is_ok(),
                complete,
                "prefix of {length} bytes"
            );
        }
    }
}

#[test]
fn deep_nesting() {
    const DEPTH: usize = 1_000_000;
    let directories = [
        &b"[1,0,{},"[..],
        &br#"[{"name":"d"},"#.repeat(DEPTH),
        br#"{"name":"f"}"#,
        &b"]".repeat(DEPTH),
        b"]",
    ]
    .concat();
    assert!(reexport(directories.as_slice(), false).is_ok());
    let values = [
        &br#"[1,0,{"a":"#[..],
        &b"[{\"b\":".repeat(DEPTH),
        b"0",
        &b"}]".repeat(DEPTH),
        br#"},[{"name":"/r"}]]"#,
    ]
    .concat();
    assert!(reexport(values.as_slice(), false).is_ok());
}

struct FailingReader {
    input: &'static [u8],
    error: io::ErrorKind,
}

impl Read for FailingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.input.is_empty() {
            return Err(self.error.into());
        }
        let length = self.input.read(buf)?;
        Ok(length)
    }
}

#[test]
fn read_error() {
    let mut rendered = String::new();
    for error in [
        io::ErrorKind::IsADirectory,
        io::ErrorKind::OutOfMemory,
        io::ErrorKind::PermissionDenied,
    ] {
        let input = FailingReader {
            input: b"[1,0,{},[{\"name\":",
            error,
        };
        let _ = writeln!(rendered, "{error:?}: {}", result(input));
    }
    assert_snapshot!(rendered, @"
    IsADirectory: 1:18: not a file
    OutOfMemory: 1:18: out of memory
    PermissionDenied: 1:18: I/O error
    ");
}

struct BrokenPipe;

impl io::Write for BrokenPipe {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn sink_error() {
    let input = export(false, None, many_files);
    let mut export = JsonExport::new(BrokenPipe, 0, false, None).expect("header is buffered");
    let kind = match import(&mut export, &input[8..], &input[..8]) {
        Err(ImportError::Sink(error)) => Some(error.kind()),
        Ok(()) | Err(ImportError::Input { .. }) => None,
    };
    assert_eq!(kind, Some(io::ErrorKind::BrokenPipe));
}
