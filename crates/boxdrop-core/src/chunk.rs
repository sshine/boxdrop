use serde::{Deserialize, Serialize};

use crate::hash::{Hash32, hash_bytes};

/// The smallest block Boxdrop will use. Small files are a single block.
pub const MIN_BLOCK: u64 = 128 * 1024;
/// The largest block Boxdrop will use.
pub const MAX_BLOCK: u64 = 16 * 1024 * 1024;

/// Pick a block size that keeps files below roughly 1024 blocks while avoiding
/// an unreasonable number of tiny blocks for small files.
pub fn block_size_for(size: u64) -> u64 {
    if size <= MIN_BLOCK {
        return MIN_BLOCK;
    }
    let scaled = size / 1024;
    let mut block = MIN_BLOCK;
    while block < scaled && block < MAX_BLOCK {
        block *= 2;
    }
    block.min(MAX_BLOCK)
}

/// The content hash, size, block size and block hashes of a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentManifest {
    pub content_id: Hash32,
    pub size: u64,
    pub block_size: u64,
    pub blocks: Vec<Hash32>,
}

impl ContentManifest {
    pub fn from_bytes(bytes: &[u8]) -> (Self, Vec<Vec<u8>>) {
        let block_size = block_size_for(bytes.len() as u64);
        let mut blocks = Vec::new();
        let mut hashes = Vec::new();
        for chunk in bytes.chunks(block_size as usize) {
            hashes.push(hash_bytes(chunk));
            blocks.push(chunk.to_vec());
        }
        let manifest = Self {
            content_id: hash_bytes(bytes),
            size: bytes.len() as u64,
            block_size,
            blocks: hashes,
        };
        (manifest, blocks)
    }

    pub fn is_complete(&self) -> bool {
        self.blocks.len() as u64 == self.expected_blocks()
    }

    pub fn expected_blocks(&self) -> u64 {
        if self.size == 0 {
            0
        } else {
            self.size.div_ceil(self.block_size)
        }
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn small_file_has_one_block() {
        let (manifest, blocks) = ContentManifest::from_bytes(b"hello");
        assert_eq!(manifest.block_size, MIN_BLOCK);
        assert_eq!(manifest.blocks.len(), 1);
        assert_eq!(blocks, vec![b"hello".to_vec()]);
        assert_eq!(manifest.content_id, hash_bytes(b"hello"));
    }

    #[test]
    fn empty_file_has_no_blocks() {
        let (manifest, blocks) = ContentManifest::from_bytes(b"");
        assert_eq!(manifest.blocks.len(), 0);
        assert!(blocks.is_empty());
        assert_eq!(manifest.content_id, hash_bytes(b""));
    }

    #[test]
    fn large_file_has_expected_block_count() {
        let bytes = vec![42u8; (MIN_BLOCK * 2 + 7) as usize];
        let (manifest, blocks) = ContentManifest::from_bytes(&bytes);
        assert_eq!(manifest.block_size, MIN_BLOCK);
        assert_eq!(manifest.blocks.len(), 3);
        assert_eq!(blocks.len(), 3);
        assert!(manifest.is_complete());
    }

    #[test]
    fn block_size_grows_with_file_size() {
        assert_eq!(block_size_for(1), MIN_BLOCK);
        assert_eq!(block_size_for(100 * 1024 * 1024), 128 * 1024);
        assert!(block_size_for(10 * 1024 * 1024 * 1024) > MIN_BLOCK);
    }

    #[test]
    fn manifests_are_deterministic() {
        let (left, _) = ContentManifest::from_bytes(b"same content");
        let (right, _) = ContentManifest::from_bytes(b"same content");
        assert_eq!(left, right);
    }
}
