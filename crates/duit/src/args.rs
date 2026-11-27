use std::mem;

/// An argument, as [`Args::next_argument`] returns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg<'a> {
    /// A short option, `-x`: the byte after the dash.
    Short(u8),
    /// A long option, `--name` or `--name=value`: the name without the dashes.
    Long(&'a [u8]),
    /// A positional argument.
    Value(&'a [u8]),
}

impl Arg<'_> {
    /// The option as it is written, without its value: `-x` or `--name`.
    pub fn to_bytes(self) -> Vec<u8> {
        match self {
            Self::Short(byte) => vec![b'-', byte],
            Self::Long(name) => [b"--", name].concat(),
            Self::Value(value) => value.to_vec(),
        }
    }
}

/// A parser for arguments in the style of `getopt_long`, as ncdu parses them.
///
/// Short options can be grouped, `-xe`. A short option takes its value from the rest of its group
/// or from the next argument: `-ofile` or `-o file`. A long option takes its value after `=` or
/// from the next argument. `--` ends the options. Options and positional arguments can be mixed.
///
/// Errors are the messages that ncdu prints, as bytes, because arguments need not be UTF-8.
pub struct Args<'a> {
    list: &'a [Vec<u8>],
    short: &'a [u8],
    last: Option<Arg<'a>>,
    last_value: Option<&'a [u8]>,
    options_ended: bool,
}

impl<'a> Args<'a> {
    pub fn new(list: &'a [Vec<u8>]) -> Self {
        Self {
            list,
            short: &[],
            last: None,
            last_value: None,
            options_ended: false,
        }
    }

    fn pop(&mut self) -> Option<&'a [u8]> {
        let (first, rest) = self.list.split_first()?;
        self.list = rest;
        Some(first)
    }

    fn short_option(&mut self, group: &'a [u8]) -> Option<Arg<'a>> {
        let (&byte, rest) = group.split_first()?;
        self.short = rest;
        let option = Arg::Short(byte);
        self.last = Some(option);
        Some(option)
    }

    fn message(&self, suffix: &[u8]) -> Vec<u8> {
        let option = self.last.map(Arg::to_bytes).unwrap_or_default();
        [b"Option '", option.as_slice(), suffix].concat()
    }

    /// Return the next option or positional argument.
    ///
    /// Fails if the value after `=` of the last option was not read with [`Args::value`].
    pub fn next_argument(&mut self) -> Result<Option<Arg<'a>>, Vec<u8>> {
        if self.last_value.is_some() {
            return Err(self.message(b"' does not expect an argument.\n"));
        }
        if let Some(option) = self.short_option(self.short) {
            return Ok(Some(option));
        }
        let Some(argument) = self.pop() else {
            return Ok(None);
        };
        let Some(option) = argument.strip_prefix(b"-").filter(|_| !self.options_ended) else {
            return Ok(Some(Arg::Value(argument)));
        };
        match option {
            [] => Err(b"Invalid option '-'.\n".to_vec()),
            [b'-'] => {
                self.options_ended = true;
                self.next_argument()
            }
            [b'-', long @ ..] => {
                let option = if let Some(separator) = long.iter().position(|&byte| byte == b'=') {
                    if separator == 0 {
                        return Err([b"Invalid option '", argument, b"'.\n"].concat());
                    }
                    self.last_value = Some(&long[separator + 1..]);
                    Arg::Long(&long[..separator])
                } else {
                    Arg::Long(long)
                };
                self.last = Some(option);
                Ok(Some(option))
            }
            group => Ok(self.short_option(group)),
        }
    }

    /// Return the value of the last option.
    pub fn value(&mut self) -> Result<&'a [u8], Vec<u8>> {
        if !self.short.is_empty() {
            return Ok(mem::take(&mut self.short));
        }
        if let Some(value) = self.last_value.take() {
            return Ok(value);
        }
        self.pop()
            .ok_or_else(|| self.message(b"' requires an argument.\n"))
    }
}
