//! The parts of the binary export format that [`crate::BinaryExport`] and [`crate::BinaryReader`]
//! share, as ncdu's reader takes them from its export.

/// The keys of the fields of an item.
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
    /// The key numbered `key`, or `None` for a number that the format does not use.
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

/// The number of item keys.
pub(crate) const ITEM_KEY_COUNT: usize = ItemKey::Mtime as usize + 1;

/// The major type of a CBOR data item.
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

/// The CBOR simple values `false` and `true`.
pub(crate) const SIMPLE_FALSE: u64 = 20;
pub(crate) const SIMPLE_TRUE: u64 = 21;

/// The block ids of a block header.
pub(crate) const DATA_BLOCK: u32 = 0;
pub(crate) const INDEX_BLOCK: u32 = 1;

/// The number of low bits of a block header that hold the length of the block. The 4 bits above
/// them hold its id.
pub(crate) const BLOCK_LENGTH_BITS: u32 = 28;
/// The largest length of a block, which its header holds in [`BLOCK_LENGTH_BITS`] bits.
pub(crate) const MAX_BLOCK_LENGTH: u32 = (1 << BLOCK_LENGTH_BITS) - 1;
/// The length of the header that starts a block.
pub(crate) const HEADER_LENGTH: usize = 4;
/// The length of the number of a data block, which follows its header.
pub(crate) const NUMBER_LENGTH: usize = 4;
/// The length of the footer that ends a block, a copy of its header.
pub(crate) const FOOTER_LENGTH: usize = 4;
