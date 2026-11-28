use std::ops::{Index, IndexMut};

/// The bits of an index into a chunk of a [`Chunked::in_chunks`] list.
pub(crate) const CHUNK_BITS: u32 = 14;

/// A list for a [`crate::Tree`] that grows like a `Vec`, or, made with [`Chunked::in_chunks`], in
/// chunks that it never moves.
///
/// A `Vec` that doubles frees its old buffer at each step. Measured on macOS, a tree whose lists
/// double peaks below the size of its data while it is not read again, as after a scan into
/// memory, but above it, by the freed buffers, once all of it is read, as when a JSON export
/// replays it. The tree of such an export grows in chunks. The first chunk grows like a `Vec`, so
/// a small list stays small.
#[derive(Debug, Clone)]
pub(crate) struct Chunked<T> {
    chunks: Vec<Vec<T>>,
    length: usize,
    chunk_bits: u32,
}

impl<T> Default for Chunked<T> {
    fn default() -> Self {
        Self {
            chunks: Vec::new(),
            length: 0,
            chunk_bits: u32::BITS,
        }
    }
}

impl<T> Chunked<T> {
    /// A list that grows in chunks of 2^[`CHUNK_BITS`] items.
    pub(crate) fn in_chunks() -> Self {
        Self {
            chunk_bits: CHUNK_BITS,
            ..Self::default()
        }
    }

    fn chunk_length(&self) -> usize {
        1_usize.checked_shl(self.chunk_bits).unwrap_or(usize::MAX)
    }

    fn split(&self, index: usize) -> (usize, usize) {
        match 1_usize.checked_shl(self.chunk_bits) {
            Some(length) => (index >> self.chunk_bits, index & (length - 1)),
            None => (0, index),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.length
    }

    pub(crate) fn push(&mut self, item: T) {
        let chunk_length = self.chunk_length();
        match self.chunks.last_mut() {
            Some(chunk) if chunk.len() < chunk_length => {
                if chunk.len() == chunk.capacity() {
                    let capacity = (chunk.capacity() * 2).clamp(16, chunk_length);
                    chunk.reserve_exact(capacity - chunk.len());
                }
                chunk.push(item);
            }
            Some(_) | None => {
                let capacity = if self.chunks.is_empty() {
                    16
                } else {
                    chunk_length
                };
                let mut chunk = Vec::with_capacity(capacity);
                chunk.push(item);
                self.chunks.push(chunk);
            }
        }
        self.length += 1;
    }

    pub(crate) fn get(&self, index: usize) -> Option<&T> {
        let (chunk, index) = self.split(index);
        self.chunks.get(chunk)?.get(index)
    }

    /// The index of the first item for which `predicate` is false, as
    /// [`slice::partition_point`].
    pub(crate) fn partition_point(&self, mut predicate: impl FnMut(&T) -> bool) -> usize {
        let (mut low, mut high) = (0, self.length);
        while low < high {
            let middle = low + (high - low) / 2;
            if self.get(middle).is_some_and(&mut predicate) {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low
    }
}

impl<T> Index<usize> for Chunked<T> {
    type Output = T;

    fn index(&self, index: usize) -> &T {
        let (chunk, index) = self.split(index);
        &self.chunks[chunk][index]
    }
}

impl<T> IndexMut<usize> for Chunked<T> {
    fn index_mut(&mut self, index: usize) -> &mut T {
        let (chunk, index) = self.split(index);
        &mut self.chunks[chunk][index]
    }
}

#[cfg(test)]
mod tests {
    use super::{CHUNK_BITS, Chunked};

    #[test]
    fn chunks() {
        const CHUNK_LENGTH: usize = 1 << CHUNK_BITS;
        let mut list = Chunked::in_chunks();
        let length = 2 * CHUNK_LENGTH + 3;
        for item in 0..length {
            list.push(item);
        }
        assert_eq!(list.len(), length);
        assert!((0..length).all(|item| list[item] == item));
        assert_eq!(list.get(length), None);
        assert_eq!(
            list.partition_point(|&item| item < CHUNK_LENGTH + 5),
            CHUNK_LENGTH + 5
        );
        assert_eq!(list.partition_point(|_| true), length);
    }
}
