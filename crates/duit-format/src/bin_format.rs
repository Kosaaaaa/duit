#[derive(Clone, Copy)]
pub(crate) enum ItemKey {
    Type = 0,
    Name = 1,
    Prev = 2,
    Asize = 3,
    Dsize = 4,
    Dev = 5,
    Rderr = 6,
    Cumasize = 7,
    Cumdsize = 8,
    Shrasize = 9,
    Shrdsize = 10,
    Items = 11,
    Sub = 12,
    Ino = 13,
    Nlink = 14,
    Uid = 15,
    Gid = 16,
    Mode = 17,
    Mtime = 18,
}

impl ItemKey {
    pub(crate) fn from_u64(key: u64) -> Option<Self> {
        Some(match key {
            0 => Self::Type,
            1 => Self::Name,
            2 => Self::Prev,
            3 => Self::Asize,
            4 => Self::Dsize,
            5 => Self::Dev,
            6 => Self::Rderr,
            7 => Self::Cumasize,
            8 => Self::Cumdsize,
            9 => Self::Shrasize,
            10 => Self::Shrdsize,
            11 => Self::Items,
            12 => Self::Sub,
            13 => Self::Ino,
            14 => Self::Nlink,
            15 => Self::Uid,
            16 => Self::Gid,
            17 => Self::Mode,
            18 => Self::Mtime,
            19..=u64::MAX => return None,
        })
    }
}

pub(crate) const ITEM_KEY_COUNT: usize = ItemKey::Mtime as usize + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Major {
    Positive = 0,
    Negative = 1,
    Bytes = 2,
    Text = 3,
    Array = 4,
    Map = 5,
    Tag = 6,
    Simple = 7,
}

pub(crate) const SIMPLE_FALSE: u64 = 20;
pub(crate) const SIMPLE_TRUE: u64 = 21;
pub(crate) const INDEFINITE_OR_BREAK: u8 = 31;
pub(crate) const MAX_HEAD_LENGTH: usize = 9;

pub(crate) const DATA_BLOCK: u32 = 0;
pub(crate) const INDEX_BLOCK: u32 = 1;

pub(crate) const BLOCK_LENGTH_BITS: u32 = 28;
pub(crate) const MAX_BLOCK_LENGTH: u32 = (1 << BLOCK_LENGTH_BITS) - 1;
pub(crate) const HEADER_LENGTH: usize = 4;
pub(crate) const NUMBER_LENGTH: usize = 4;
pub(crate) const FOOTER_LENGTH: usize = 4;
pub(crate) const INDEX_ENTRY_LENGTH: usize = 8;
pub(crate) const ROOT_REFERENCE_LENGTH: usize = 8;

const LOW_FIELD_BITS: u32 = 24;
const LOW_FIELD_MASK: u64 = (1 << LOW_FIELD_BITS) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ItemRef(pub(crate) u64);

impl ItemRef {
    pub(crate) fn new(block: u64, offset: u64) -> Self {
        Self((block << LOW_FIELD_BITS) | offset)
    }

    pub(crate) fn block(self) -> u64 {
        self.0 >> LOW_FIELD_BITS
    }

    pub(crate) fn offset(self) -> u64 {
        self.0 & LOW_FIELD_MASK
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockPosition(pub(crate) u64);

impl BlockPosition {
    pub(crate) fn new(offset: u64, length: u64) -> Self {
        Self((offset << LOW_FIELD_BITS) + length)
    }

    pub(crate) fn offset(self) -> u64 {
        self.0 >> LOW_FIELD_BITS
    }

    pub(crate) fn length(self) -> u64 {
        self.0 & LOW_FIELD_MASK
    }
}
