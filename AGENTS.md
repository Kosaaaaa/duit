- Read CONTRIBUTING.md for guidelines on how to run tools
- Read PLAN.md for the scope, the architecture, the decisions already made, and the milestones
- duit is a drop-in replacement for ncdu 2.9.2. When behavior is unclear, the upstream source at
  <https://code.blicky.net/yorhel/ncdu>, tag `v2.9.2`, decides. Where the code and the man page
  disagree, follow the code
- NEVER add a flag, option, key binding, or feature that ncdu does not have before milestone M4 in
  PLAN.md is complete
- Port behavior, not Zig idioms. Global mutable state, `extern` struct layouts, and pointer casts
  from upstream do not carry over
- ALWAYS ensure that new tests use the same style as existing tests for all parts of the test
- ALWAYS check whether the behavior of a new test is already covered by an existing test
- PREFER integration tests, e.g., at `crates/duit/tests/it/...` over unit tests
- PREFER `insta` snapshots following patterns in nearby tests over substring assertions
- PREFER running specific tests over running the entire test suite
- ALWAYS check if a new or modified test needs a `#[cfg(feature = "test-...")]` gate, e.g.,
  `test-ncdu` for tests that run the real ncdu
- NEVER perform builds with the release profile, unless asked or reproducing performance issues
- AVOID using `panic!`, `unreachable!`, `.unwrap()`, unsafe code, and clippy rule ignores
- PREFER patterns like `if let` to handle fallibility
- PREFER exhaustive `match` expressions without wildcard (`_`) arms over `matches!`, so new enum
  variants require explicit handling
- ALWAYS write `SAFETY` comments when writing `unsafe` code
- PREFER `#[expect()]` over `[allow()]` if clippy must be disabled
- PREFER let chains (`if let` combined with `&&`) over nested `if let` statements
- Add a dependency only together with the code that uses it. Declare it in
  `[workspace.dependencies]` in the root `Cargo.toml`. A dependency with a build script also needs
  an entry in `.cargo/deny.toml`
- NEVER update all dependencies in the lockfile and ALWAYS use `cargo update --precise` to make
  lockfile changes
- NEVER assume clippy warnings or test failures are pre-existing, it is very rare that `main` has
  warnings
- PREFER top-level imports over local imports or fully qualified names
- AVOID shortening variable names, e.g., use `directory` instead of `dir`, and `block_count` instead
  of `bc`
- PREFER [`TypeName`] references when writing Rust doc comments
- DO NOT leak our conversation, prompt, or iteration history into code comments, pull request
  descriptions, or other maintainer-facing prose. Write for readers who have not seen our
  conversation.
- PREFER comments that explain the current behavior and rationale. Avoid past-facing wording like
  "preserve the existing behavior"; explain the actual backwards-compatibility constraint instead.
- NEVER use em-dashes in prose, comments, or commit messages. Use a comma, a colon, parentheses, or
  a new sentence
