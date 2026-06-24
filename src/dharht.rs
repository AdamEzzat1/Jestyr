//! **Experimental** — a vendored copy of the D-HARHT deterministic hash/radix
//! table (Memory profile), used to evaluate replacing the compiler's randomized
//! `HashMap` symbol tables with a deterministic, build-once/lookup-many structure.
//!
//! Compiled only under `--features dharht-experiment`. Self-contained (std only).
//! Source: the D-HARHT blueprint; `seal_for_lookup()` then `get()` is the access
//! pattern that matches a compiler's "build a symbol table in typeck, read it in
//! cgen" shape. See `docs/TESTING.md` §6 for the experiment and its findings.
#![allow(dead_code)]

use std::mem::size_of;

const EMPTY: u32 = u32::MAX;
const FRONT_EMPTY: u64 = u64::MAX;
const FRONT_TAG_MASK: u64 = 0b111;
const FRONT_SINGLE: u64 = 0b001;
const FRONT_MICRO4: u64 = 0b010;
const FRONT_MICRO8: u64 = 0b011;
const FRONT_MICRO16: u64 = 0b100;
const DEFAULT_FRONT_BITS: u8 = 20;
const SPARSE_PAGE_BITS: u8 = 8;
const SPARSE_PAGE_SIZE: usize = 1 << SPARSE_PAGE_BITS;
const NO_PAGE: u32 = u32::MAX;
const NODE48_EMPTY: u8 = u8::MAX;

pub type NodeId = u32;

#[derive(Clone, Debug)]
pub struct DHarht<V> {
    shards: Vec<Shard<V>>,
    front_dir: FrontDirectory,
    micro4: Vec<MicroBucket4>,
    micro8: Vec<MicroBucket8>,
    micro16: Vec<MicroBucket16>,
    front_bits: u8,
    profile: LookupProfile,
    sealed: bool,
    shard_mask: usize,
    shard_shift: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookupProfile {
    Speed,
    Balanced,
    Memory,
}

#[derive(Clone, Debug)]
enum FrontDirectory {
    Empty,
    Dense(Vec<u64>),
    Sparse {
        page_table: Vec<u32>,
        pages: Vec<Box<[u64; SPARSE_PAGE_SIZE]>>,
    },
}

#[derive(Clone, Debug)]
pub struct Shard<V> {
    slab: Slab<V>,
    root: Option<NodeId>,
    second_jump: Box<[NodeId; 256]>,
    second_leaf: Box<[u32; 256]>,
}

#[derive(Clone, Debug)]
pub struct Slab<V> {
    entries: Vec<TaggedNode>,
    leaves: Vec<LeafNode<V>>,
    node4: Vec<Node4>,
    node16: Vec<Node16>,
    node32: Vec<Node32>,
    node48: Vec<Node48>,
    node256: Vec<Node256>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TaggedNode(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeKind {
    Leaf,
    Node4,
    Node16,
    Node32,
    Node48,
    Node256,
}

impl TaggedNode {
    const TAG_BITS: u32 = 3;
    const INDEX_SHIFT: u32 = 3;
    const LEAF: u32 = 0;
    const NODE4: u32 = 1;
    const NODE16: u32 = 2;
    const NODE32: u32 = 3;
    const NODE48: u32 = 4;
    const NODE256: u32 = 5;

    fn leaf(index: usize) -> Self {
        Self::new(index, Self::LEAF)
    }

    fn node4(index: usize) -> Self {
        Self::new(index, Self::NODE4)
    }

    fn node16(index: usize) -> Self {
        Self::new(index, Self::NODE16)
    }

    fn node48(index: usize) -> Self {
        Self::new(index, Self::NODE48)
    }

    fn node32(index: usize) -> Self {
        Self::new(index, Self::NODE32)
    }

    fn node256(index: usize) -> Self {
        Self::new(index, Self::NODE256)
    }

    fn new(index: usize, tag: u32) -> Self {
        debug_assert!(tag < (1 << Self::TAG_BITS));
        assert!(index < (1_usize << (32 - Self::INDEX_SHIFT)));
        Self(((index as u32) << Self::INDEX_SHIFT) | tag)
    }

    fn kind(self) -> NodeKind {
        match self.0 & ((1 << Self::TAG_BITS) - 1) {
            Self::LEAF => NodeKind::Leaf,
            Self::NODE4 => NodeKind::Node4,
            Self::NODE16 => NodeKind::Node16,
            Self::NODE32 => NodeKind::Node32,
            Self::NODE48 => NodeKind::Node48,
            Self::NODE256 => NodeKind::Node256,
            _ => unreachable!(),
        }
    }

    fn index(self) -> usize {
        (self.0 >> Self::INDEX_SHIFT) as usize
    }
}

#[inline(always)]
fn id_index(id: NodeId) -> usize {
    id as usize
}

#[inline(always)]
fn make_id(index: usize) -> NodeId {
    assert!(
        index <= NodeId::MAX as usize,
        "node slab exceeded u32 address space"
    );
    index as NodeId
}

#[inline(always)]
fn pack_front_leaf(shard_id: usize, leaf_id: usize) -> u64 {
    debug_assert!(shard_id <= u32::MAX as usize);
    debug_assert!(leaf_id <= u32::MAX as usize);
    ((shard_id as u64) << 32) | leaf_id as u64
}

#[inline(always)]
fn unpack_front_leaf(packed: u64) -> (usize, usize) {
    ((packed >> 32) as usize, (packed as u32) as usize)
}

#[inline(always)]
fn pack_front_single(shard_id: usize, leaf_id: usize) -> u64 {
    debug_assert!(shard_id < (1 << 29));
    debug_assert!(leaf_id < (1 << 32));
    ((shard_id as u64) << 35) | ((leaf_id as u64) << 3) | FRONT_SINGLE
}

#[inline(always)]
fn pack_front_micro4(bucket_id: usize) -> u64 {
    debug_assert!(bucket_id < (1_usize << 61));
    ((bucket_id as u64) << 3) | FRONT_MICRO4
}

#[inline(always)]
fn pack_front_micro8(bucket_id: usize) -> u64 {
    debug_assert!(bucket_id < (1_usize << 61));
    ((bucket_id as u64) << 3) | FRONT_MICRO8
}

#[inline(always)]
fn pack_front_micro16(bucket_id: usize) -> u64 {
    debug_assert!(bucket_id < (1_usize << 61));
    ((bucket_id as u64) << 3) | FRONT_MICRO16
}

#[inline(always)]
fn unpack_front_single(packed: u64) -> (usize, usize) {
    ((packed >> 35) as usize, ((packed >> 3) as u32) as usize)
}

#[inline(always)]
fn unpack_front_micro4(packed: u64) -> usize {
    (packed >> 3) as usize
}

#[inline(always)]
fn unpack_front_micro8(packed: u64) -> usize {
    (packed >> 3) as usize
}

#[inline(always)]
fn unpack_front_micro16(packed: u64) -> usize {
    (packed >> 3) as usize
}

#[inline(always)]
fn front_prefix(scattered: u64, bits: u8) -> usize {
    (scattered >> (64 - bits)) as usize
}

#[repr(C)]
#[derive(Clone, Debug)]
pub struct LeafNode<V> {
    pub key: u64,
    pub value: V,
}

#[repr(C)]
#[derive(Clone, Debug)]
pub struct Node4 {
    pub count: u8,
    pub keys: [u8; 4],
    pub children: [NodeId; 4],
}

#[repr(C)]
#[derive(Clone, Debug)]
pub struct Node16 {
    pub count: u8,
    pub keys: [u8; 16],
    pub children: [NodeId; 16],
}

#[repr(C)]
#[derive(Clone, Debug)]
pub struct Node32 {
    pub count: u8,
    pub keys: [u8; 32],
    pub children: [NodeId; 32],
}

#[repr(C)]
#[derive(Clone, Debug)]
pub struct Node48 {
    pub count: u8,
    pub child_index: [u8; 256],
    pub children: [NodeId; 48],
}

#[repr(C)]
#[derive(Clone, Debug)]
pub struct Node256 {
    pub count: u16,
    pub children: [NodeId; 256],
}

#[repr(C)]
#[derive(Clone, Debug)]
pub struct MicroBucket4 {
    pub shard_id: u16,
    pub count: u8,
    pub keys: [u64; 4],
    pub leaf_ids: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Debug)]
pub struct MicroBucket8 {
    pub shard_id: u16,
    pub count: u8,
    pub keys: [u64; 8],
    pub leaf_ids: [u32; 8],
}

#[repr(C)]
#[derive(Clone, Debug)]
pub struct MicroBucket16 {
    pub shard_id: u16,
    pub count: u8,
    pub keys: [u64; 16],
    pub leaf_ids: [u32; 16],
}

impl MicroBucket4 {
    #[inline(always)]
    fn match_mask(&self, key: u64) -> u32 {
        let mask = ((self.keys[0] == key) as u32)
            | (((self.keys[1] == key) as u32) << 1)
            | (((self.keys[2] == key) as u32) << 2)
            | (((self.keys[3] == key) as u32) << 3);
        let live = if self.count >= 4 {
            0b1111
        } else {
            (1_u32 << self.count) - 1
        };
        mask & live
    }
}

impl MicroBucket8 {
    #[inline(always)]
    fn match_mask(&self, key: u64) -> u32 {
        let mut mask = 0_u32;
        for slot in 0..8 {
            mask |= ((self.keys[slot] == key) as u32) << slot;
        }
        let live = if self.count >= 8 {
            0xff
        } else {
            (1_u32 << self.count) - 1
        };
        mask & live
    }
}

impl MicroBucket16 {
    #[inline(always)]
    fn match_mask(&self, key: u64) -> u32 {
        let mut mask = 0_u32;
        for slot in 0..16 {
            mask |= ((self.keys[slot] == key) as u32) << slot;
        }
        let live = if self.count >= 16 {
            u16::MAX as u32
        } else {
            (1_u32 << self.count) - 1
        };
        mask & live
    }
}

impl FrontDirectory {
    fn get(&self, prefix: usize) -> u64 {
        match self {
            FrontDirectory::Empty => FRONT_EMPTY,
            FrontDirectory::Dense(entries) => entries[prefix],
            FrontDirectory::Sparse { page_table, pages } => {
                let page_id = page_table[prefix >> SPARSE_PAGE_BITS];
                if page_id == NO_PAGE {
                    FRONT_EMPTY
                } else {
                    pages[page_id as usize][prefix & (SPARSE_PAGE_SIZE - 1)]
                }
            }
        }
    }

    fn memory_bytes(&self) -> usize {
        match self {
            FrontDirectory::Empty => 0,
            FrontDirectory::Dense(entries) => entries.capacity() * size_of::<u64>(),
            FrontDirectory::Sparse { page_table, pages } => {
                page_table.capacity() * size_of::<u32>()
                    + pages.capacity() * size_of::<Box<[u64; SPARSE_PAGE_SIZE]>>()
                    + pages.len() * SPARSE_PAGE_SIZE * size_of::<u64>()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchitectureBlueprint {
    pub name: &'static str,
    pub shards: usize,
    pub shard_bytes: usize,
    pub node4_bytes: usize,
    pub node16_bytes: usize,
    pub node48_bytes: usize,
    pub node256_bytes: usize,
    pub deterministic_permutation: &'static str,
    pub simd_tier: &'static str,
    pub allocator: &'static str,
}

impl<V> DHarht<V> {
    pub fn new(shards: usize) -> Self {
        assert!(
            shards.is_power_of_two(),
            "shard count must be a power of two"
        );
        assert!(
            shards > 0 && shards <= 256,
            "shard count must be in 1..=256"
        );
        let shard_shift = if shards == 1 {
            0
        } else {
            64 - shards.trailing_zeros()
        };
        let mut root = Vec::with_capacity(shards);
        for _ in 0..shards {
            root.push(Shard::new());
        }
        Self {
            shards: root,
            front_dir: FrontDirectory::Empty,
            micro4: Vec::new(),
            micro8: Vec::new(),
            micro16: Vec::new(),
            front_bits: DEFAULT_FRONT_BITS,
            profile: LookupProfile::Balanced,
            sealed: false,
            shard_mask: shards - 1,
            shard_shift,
        }
    }

    pub fn with_capacity(shards: usize, entries_per_shard: usize) -> Self {
        let mut tree = Self::new(shards);
        for shard in &mut tree.shards {
            shard.slab.reserve(entries_per_shard);
        }
        tree
    }

    pub fn set_lookup_profile(&mut self, profile: LookupProfile) {
        self.profile = profile;
        self.front_bits = match profile {
            LookupProfile::Speed | LookupProfile::Balanced => 20,
            LookupProfile::Memory => 16,
        };
        self.sealed = false;
    }

    pub fn insert(&mut self, key: u64, value: V) -> Option<V> {
        let scattered = deterministic_permutation_scatter(key);
        let shard_id = self.shard_id(scattered);
        let previous = self.shards[shard_id].insert(key, scattered, value);
        self.sealed = false;
        previous
    }

    #[inline(always)]
    pub fn get(&self, key: u64) -> Option<&V> {
        let scattered = deterministic_permutation_scatter(key);
        self.get_scattered(key, scattered)
    }

    #[inline(always)]
    pub fn get_scattered(&self, key: u64, scattered: u64) -> Option<&V> {
        if self.sealed {
            let front = self.front_dir.get(front_prefix(scattered, self.front_bits));
            match front & FRONT_TAG_MASK {
                FRONT_SINGLE => {
                    let (packed_shard, leaf_id) = unpack_front_single(front);
                    let leaf = &self.shards[packed_shard].slab.leaves[leaf_id];
                    return (leaf.key == key).then_some(&leaf.value);
                }
                FRONT_MICRO4 => {
                    let bucket = &self.micro4[unpack_front_micro4(front)];
                    let mask = bucket.match_mask(key);
                    if mask != 0 {
                        let slot = mask.trailing_zeros() as usize;
                        let leaf = &self.shards[bucket.shard_id as usize].slab.leaves
                            [bucket.leaf_ids[slot] as usize];
                        return Some(&leaf.value);
                    }
                    return None;
                }
                FRONT_MICRO8 => {
                    let bucket = &self.micro8[unpack_front_micro8(front)];
                    let mask = bucket.match_mask(key);
                    if mask != 0 {
                        let slot = mask.trailing_zeros() as usize;
                        let leaf = &self.shards[bucket.shard_id as usize].slab.leaves
                            [bucket.leaf_ids[slot] as usize];
                        return Some(&leaf.value);
                    }
                    return None;
                }
                FRONT_MICRO16 => {
                    let bucket = &self.micro16[unpack_front_micro16(front)];
                    let mask = bucket.match_mask(key);
                    if mask != 0 {
                        let slot = mask.trailing_zeros() as usize;
                        let leaf = &self.shards[bucket.shard_id as usize].slab.leaves
                            [bucket.leaf_ids[slot] as usize];
                        return Some(&leaf.value);
                    }
                    return None;
                }
                _ => {}
            }
        }

        let shard_id = self.shard_id(scattered);
        self.shards[shard_id].get(key, scattered)
    }

    pub fn contains_key(&self, key: u64) -> bool {
        self.get(key).is_some()
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    pub fn allocated_nodes(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| shard.slab.entries.len())
            .sum()
    }

    pub fn approx_memory_bytes(&self) -> usize {
        size_of::<Self>()
            + self.shards.capacity() * size_of::<Shard<V>>()
            + self.front_dir.memory_bytes()
            + self.micro4.capacity() * size_of::<MicroBucket4>()
            + self.micro8.capacity() * size_of::<MicroBucket8>()
            + self.micro16.capacity() * size_of::<MicroBucket16>()
            + self
                .shards
                .iter()
                .map(|shard| shard.approx_memory_bytes())
                .sum::<usize>()
    }

    pub fn blueprint(&self) -> ArchitectureBlueprint {
        ArchitectureBlueprint {
            name: "D-HARHT/static-hash-partitioned SIMD-ART",
            shards: self.shards.len(),
            shard_bytes: size_of::<Shard<V>>(),
            node4_bytes: size_of::<Node4>(),
            node16_bytes: size_of::<Node16>(),
            node48_bytes: size_of::<Node48>(),
            node256_bytes: size_of::<Node256>(),
            deterministic_permutation: "splitmix64 finalizer, fixed seed-free permutation",
            simd_tier: simd_tier(),
            allocator: "sealed per-shard typed slabs; u32 NodeId children; u32 tagged entries; deterministic profiled front directory with MicroBucket4/8/16",
        }
    }

    pub fn blueprint_cjc(&self) -> String {
        let bp = self.blueprint();
        format!(
            "{{\"name\":\"{}\",\"shards\":{},\"shard_bytes\":{},\"node4_bytes\":{},\"node16_bytes\":{},\"node48_bytes\":{},\"node256_bytes\":{},\"deterministic_permutation\":\"{}\",\"simd_tier\":\"{}\",\"allocator\":\"{}\"}}",
            bp.name,
            bp.shards,
            bp.shard_bytes,
            bp.node4_bytes,
            bp.node16_bytes,
            bp.node48_bytes,
            bp.node256_bytes,
            bp.deterministic_permutation,
            bp.simd_tier,
            bp.allocator
        )
    }

    fn shard_id(&self, scattered: u64) -> usize {
        if self.shards.len() == 1 {
            0
        } else {
            ((scattered >> self.shard_shift) as usize) & self.shard_mask
        }
    }

    pub fn optimize_lookup_index(&mut self) {
        self.seal_for_lookup();
    }

    pub fn seal_for_lookup(&mut self) {
        self.compact_leaf_storage();
        self.micro4.clear();
        self.micro8.clear();
        self.micro16.clear();

        let mut buckets: std::collections::BTreeMap<usize, Vec<u64>> =
            std::collections::BTreeMap::new();
        for shard_id in 0..self.shards.len() {
            for (leaf_id, leaf) in self.shards[shard_id].slab.leaves.iter().enumerate() {
                let prefix =
                    front_prefix(deterministic_permutation_scatter(leaf.key), self.front_bits);
                buckets
                    .entry(prefix)
                    .or_default()
                    .push(pack_front_leaf(shard_id, leaf_id));
            }
        }

        let mut entries = Vec::with_capacity(buckets.len());
        for (prefix, packed_leaves) in buckets {
            let entry = match packed_leaves.len() {
                0 => FRONT_EMPTY,
                1 => {
                    let (shard_id, leaf_id) = unpack_front_leaf(packed_leaves[0]);
                    pack_front_single(shard_id, leaf_id)
                }
                2..=4 => {
                    let bucket_id = self.micro4.len();
                    let mut bucket = MicroBucket4 {
                        shard_id: 0,
                        count: packed_leaves.len() as u8,
                        keys: [0; 4],
                        leaf_ids: [EMPTY; 4],
                    };
                    for (slot, packed_leaf) in packed_leaves.iter().enumerate() {
                        let (shard_id, leaf_id) = unpack_front_leaf(*packed_leaf);
                        bucket.shard_id = shard_id as u16;
                        bucket.keys[slot] = self.shards[shard_id].slab.leaves[leaf_id].key;
                        bucket.leaf_ids[slot] = leaf_id as u32;
                    }
                    self.micro4.push(bucket);
                    pack_front_micro4(bucket_id)
                }
                5..=8 => {
                    let bucket_id = self.micro8.len();
                    let mut bucket = MicroBucket8 {
                        shard_id: 0,
                        count: packed_leaves.len() as u8,
                        keys: [0; 8],
                        leaf_ids: [EMPTY; 8],
                    };
                    for (slot, packed_leaf) in packed_leaves.iter().enumerate() {
                        let (shard_id, leaf_id) = unpack_front_leaf(*packed_leaf);
                        bucket.shard_id = shard_id as u16;
                        bucket.keys[slot] = self.shards[shard_id].slab.leaves[leaf_id].key;
                        bucket.leaf_ids[slot] = leaf_id as u32;
                    }
                    self.micro8.push(bucket);
                    pack_front_micro8(bucket_id)
                }
                9..=16 => {
                    let bucket_id = self.micro16.len();
                    let mut bucket = MicroBucket16 {
                        shard_id: 0,
                        count: packed_leaves.len() as u8,
                        keys: [0; 16],
                        leaf_ids: [EMPTY; 16],
                    };
                    for (slot, packed_leaf) in packed_leaves.iter().enumerate() {
                        let (shard_id, leaf_id) = unpack_front_leaf(*packed_leaf);
                        bucket.shard_id = shard_id as u16;
                        bucket.keys[slot] = self.shards[shard_id].slab.leaves[leaf_id].key;
                        bucket.leaf_ids[slot] = leaf_id as u32;
                    }
                    self.micro16.push(bucket);
                    pack_front_micro16(bucket_id)
                }
                _ => FRONT_EMPTY,
            };
            if entry != FRONT_EMPTY {
                entries.push((prefix, entry));
            }
        }
        self.front_dir = self.build_front_directory(entries);
        self.sealed = true;
    }

    fn build_front_directory(&self, entries: Vec<(usize, u64)>) -> FrontDirectory {
        let directory_len = 1_usize << self.front_bits;
        match self.profile {
            LookupProfile::Speed => {
                let mut dense = vec![FRONT_EMPTY; directory_len];
                for (prefix, entry) in entries {
                    dense[prefix] = entry;
                }
                FrontDirectory::Dense(dense)
            }
            LookupProfile::Balanced | LookupProfile::Memory => {
                let page_count = directory_len / SPARSE_PAGE_SIZE;
                let mut page_table = vec![NO_PAGE; page_count];
                let mut pages: Vec<Box<[u64; SPARSE_PAGE_SIZE]>> = Vec::new();
                for (prefix, entry) in entries {
                    let page = prefix >> SPARSE_PAGE_BITS;
                    if page_table[page] == NO_PAGE {
                        page_table[page] = pages.len() as u32;
                        pages.push(Box::new([FRONT_EMPTY; SPARSE_PAGE_SIZE]));
                    }
                    let page_id = page_table[page] as usize;
                    pages[page_id][prefix & (SPARSE_PAGE_SIZE - 1)] = entry;
                }
                FrontDirectory::Sparse { page_table, pages }
            }
        }
    }

    fn compact_leaf_storage(&mut self) {
        for shard in &mut self.shards {
            let mut old = std::mem::take(&mut shard.slab.leaves)
                .into_iter()
                .enumerate()
                .collect::<Vec<_>>();
            old.sort_by_key(|(_, leaf)| (deterministic_permutation_scatter(leaf.key), leaf.key));

            let mut remap = vec![0_usize; old.len()];
            let mut compacted = Vec::with_capacity(old.len());
            for (new_id, (old_id, leaf)) in old.into_iter().enumerate() {
                remap[old_id] = new_id;
                compacted.push(leaf);
            }
            shard.slab.leaves = compacted;

            for entry in &mut shard.slab.entries {
                if entry.kind() == NodeKind::Leaf {
                    *entry = TaggedNode::leaf(remap[entry.index()]);
                }
            }
            if let Some(root) = shard.root {
                if shard.slab.entries[id_index(root)].kind() == NodeKind::Leaf {
                    shard.second_jump.fill(EMPTY);
                    shard.second_leaf.fill(EMPTY);
                    let leaf_id = shard.slab.entries[id_index(root)].index();
                    let key = shard.slab.leaves[leaf_id].key;
                    shard.second_leaf
                        [radix_byte(deterministic_permutation_scatter(key), 1) as usize] =
                        leaf_id as u32;
                } else {
                    shard.rebuild_second_jump(root);
                }
            }
        }
    }
}

impl<V> Default for DHarht<V> {
    fn default() -> Self {
        Self::new(256)
    }
}

impl<V> Shard<V> {
    pub fn new() -> Self {
        Self {
            slab: Slab::new(),
            root: None,
            second_jump: Box::new([EMPTY; 256]),
            second_leaf: Box::new([EMPTY; 256]),
        }
    }

    pub fn insert(&mut self, key: u64, scattered: u64, value: V) -> Option<V> {
        let previous = match self.root {
            Some(root) => self.slab.insert_at(root, 1, key, scattered, value),
            None => {
                self.root = Some(self.slab.alloc_leaf(key, value));
                None
            }
        };
        if let Some(root) = self.root {
            if self.slab.entries[id_index(root)].kind() == NodeKind::Leaf {
                self.second_jump.fill(EMPTY);
                self.second_leaf.fill(EMPTY);
                let leaf_id = self.slab.entries[id_index(root)].index();
                self.second_leaf[radix_byte(scattered, 1) as usize] = leaf_id as u32;
            } else {
                self.rebuild_second_jump(root);
            }
        }
        previous
    }

    pub fn get(&self, key: u64, scattered: u64) -> Option<&V> {
        let root = self.root?;
        let second = radix_byte(scattered, 1);
        let direct_leaf = self.second_leaf[second as usize];
        if direct_leaf != EMPTY {
            let leaf = &self.slab.leaves[direct_leaf as usize];
            return (leaf.key == key).then_some(&leaf.value);
        }
        let cached = self.second_jump[second as usize];
        let (mut node_id, mut depth) = if cached == EMPTY {
            (root, 1)
        } else {
            (cached, 2)
        };
        loop {
            let entry = self.slab.entries[id_index(node_id)];
            match entry.kind() {
                NodeKind::Leaf => {
                    let leaf_id = entry.index();
                    let leaf = &self.slab.leaves[leaf_id];
                    return (leaf.key == key).then_some(&leaf.value);
                }
                NodeKind::Node4 => {
                    let node_id4 = entry.index();
                    let node = &self.slab.node4[node_id4];
                    let slot = find_key4(&node.keys, node.count, radix_byte(scattered, depth))?;
                    node_id = node.children[slot];
                }
                NodeKind::Node16 => {
                    let node_id16 = entry.index();
                    let node = &self.slab.node16[node_id16];
                    let slot = find_key16(&node.keys, node.count, radix_byte(scattered, depth))?;
                    node_id = node.children[slot];
                }
                NodeKind::Node32 => {
                    let node_id32 = entry.index();
                    let node = &self.slab.node32[node_id32];
                    let radix = radix_byte(scattered, depth);
                    let slot = node.keys[..node.count as usize]
                        .iter()
                        .position(|&key| key == radix)?;
                    node_id = node.children[slot];
                }
                NodeKind::Node48 => {
                    let node_id48 = entry.index();
                    let node = &self.slab.node48[node_id48];
                    let slot = node.child_index[radix_byte(scattered, depth) as usize];
                    if slot == NODE48_EMPTY {
                        return None;
                    }
                    node_id = node.children[slot as usize];
                }
                NodeKind::Node256 => {
                    let node_id256 = entry.index();
                    let node = &self.slab.node256[node_id256];
                    let child = node.children[radix_byte(scattered, depth) as usize];
                    if child == EMPTY {
                        return None;
                    }
                    node_id = child;
                }
            }
            depth += 1;
        }
    }

    pub fn approx_memory_bytes(&self) -> usize {
        size_of::<Self>()
            + size_of::<[NodeId; 256]>()
            + self.slab.approx_memory_bytes()
            + size_of::<[u32; 256]>()
    }

    fn rebuild_second_jump(&mut self, root: NodeId) {
        self.second_jump.fill(EMPTY);
        self.second_leaf.fill(EMPTY);
        for radix in 0..=u8::MAX {
            if let Some(child) = self.slab.find_child(root, radix) {
                let entry = self.slab.entries[id_index(child)];
                if entry.kind() == NodeKind::Leaf {
                    self.second_leaf[radix as usize] = entry.index() as u32;
                } else {
                    self.second_jump[radix as usize] = child;
                }
            }
        }
    }
}

impl<V> Default for Shard<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> Slab<V> {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            leaves: Vec::new(),
            node4: Vec::new(),
            node16: Vec::new(),
            node32: Vec::new(),
            node48: Vec::new(),
            node256: Vec::new(),
        }
    }

    fn reserve(&mut self, entries_per_shard: usize) {
        self.entries.reserve(entries_per_shard * 2);
        self.leaves.reserve(entries_per_shard);
        self.node4.reserve(entries_per_shard / 2 + 1);
    }

    fn approx_memory_bytes(&self) -> usize {
        size_of::<Self>()
            + self.entries.capacity() * size_of::<TaggedNode>()
            + self.leaves.capacity() * size_of::<LeafNode<V>>()
            + self.node4.capacity() * size_of::<Node4>()
            + self.node16.capacity() * size_of::<Node16>()
            + self.node32.capacity() * size_of::<Node32>()
            + self.node48.capacity() * size_of::<Node48>()
            + self.node256.capacity() * size_of::<Node256>()
    }

    fn alloc_entry(&mut self, entry: TaggedNode) -> NodeId {
        let id = self.entries.len();
        self.entries.push(entry);
        make_id(id)
    }

    fn alloc_leaf(&mut self, key: u64, value: V) -> NodeId {
        let leaf_id = self.leaves.len();
        self.leaves.push(LeafNode { key, value });
        self.alloc_entry(TaggedNode::leaf(leaf_id))
    }

    fn alloc_node4(&mut self) -> NodeId {
        let node_id = self.node4.len();
        self.node4.push(Node4 {
            count: 0,
            keys: [0; 4],
            children: [EMPTY; 4],
        });
        self.alloc_entry(TaggedNode::node4(node_id))
    }

    fn insert_at(
        &mut self,
        node_id: NodeId,
        depth: usize,
        key: u64,
        scattered: u64,
        value: V,
    ) -> Option<V> {
        if self.entries[id_index(node_id)].kind() == NodeKind::Leaf {
            let leaf_id = self.entries[id_index(node_id)].index();
            let leaf = &mut self.leaves[leaf_id];
            if leaf.key == key {
                return Some(std::mem::replace(&mut leaf.value, value));
            }

            let old_leaf = self.alloc_entry(TaggedNode::leaf(leaf_id));
            let new_leaf = self.alloc_leaf(key, value);
            let old_scattered = deterministic_permutation_scatter(self.leaves[leaf_id].key);
            self.join_leaves_in_place(node_id, depth, old_scattered, old_leaf, scattered, new_leaf);
            return None;
        }

        let radix = radix_byte(scattered, depth);
        if let Some(child) = self.find_child(node_id, radix) {
            return self.insert_at(child, depth + 1, key, scattered, value);
        }

        let leaf = self.alloc_leaf(key, value);
        self.add_child(node_id, radix, leaf);
        None
    }

    fn join_leaves_in_place(
        &mut self,
        target: NodeId,
        depth: usize,
        left_scattered: u64,
        left: NodeId,
        right_scattered: u64,
        right: NodeId,
    ) {
        let left_radix = radix_byte(left_scattered, depth);
        let right_radix = radix_byte(right_scattered, depth);
        let node4_id = self.node4.len();
        self.node4.push(Node4 {
            count: 0,
            keys: [0; 4],
            children: [EMPTY; 4],
        });
        self.entries[id_index(target)] = TaggedNode::node4(node4_id);

        if left_radix == right_radix && depth < 7 {
            let child = self.alloc_node4();
            self.add_child(target, left_radix, child);
            self.join_leaves_in_place(
                child,
                depth + 1,
                left_scattered,
                left,
                right_scattered,
                right,
            );
        } else {
            self.add_child(target, left_radix, left);
            self.add_child(target, right_radix, right);
        }
    }

    fn find_child(&self, node_id: NodeId, radix: u8) -> Option<NodeId> {
        let entry = self.entries[id_index(node_id)];
        match entry.kind() {
            NodeKind::Leaf => None,
            NodeKind::Node4 => {
                let id = entry.index();
                let node = &self.node4[id];
                find_key4(&node.keys, node.count, radix).map(|slot| node.children[slot])
            }
            NodeKind::Node16 => {
                let id = entry.index();
                let node = &self.node16[id];
                find_key16(&node.keys, node.count, radix).map(|slot| node.children[slot])
            }
            NodeKind::Node32 => {
                let id = entry.index();
                let node = &self.node32[id];
                node.keys[..node.count as usize]
                    .iter()
                    .position(|&key| key == radix)
                    .map(|slot| node.children[slot])
            }
            NodeKind::Node48 => {
                let id = entry.index();
                let node = &self.node48[id];
                let slot = node.child_index[radix as usize];
                if slot == NODE48_EMPTY {
                    None
                } else {
                    Some(node.children[slot as usize])
                }
            }
            NodeKind::Node256 => {
                let id = entry.index();
                let child = self.node256[id].children[radix as usize];
                if child == EMPTY {
                    None
                } else {
                    Some(child)
                }
            }
        }
    }

    fn add_child(&mut self, node_id: NodeId, radix: u8, child: NodeId) {
        let entry = self.entries[id_index(node_id)];
        match entry.kind() {
            NodeKind::Node4 if self.node4[entry.index()].count < 4 => {
                let id = entry.index();
                let node = &mut self.node4[id];
                let slot = node.count as usize;
                node.keys[slot] = radix;
                node.children[slot] = child;
                node.count += 1;
            }
            NodeKind::Node4 => {
                self.grow_node4_to_node16(node_id);
                self.add_child(node_id, radix, child);
            }
            NodeKind::Node16 if self.node16[entry.index()].count < 16 => {
                let id = entry.index();
                let node = &mut self.node16[id];
                let slot = node.count as usize;
                node.keys[slot] = radix;
                node.children[slot] = child;
                node.count += 1;
            }
            NodeKind::Node16 => {
                self.grow_node16_to_node32(node_id);
                self.add_child(node_id, radix, child);
            }
            NodeKind::Node32 if self.node32[entry.index()].count < 32 => {
                let id = entry.index();
                let node = &mut self.node32[id];
                let slot = node.count as usize;
                node.keys[slot] = radix;
                node.children[slot] = child;
                node.count += 1;
            }
            NodeKind::Node32 => {
                self.grow_node32_to_node48(node_id);
                self.add_child(node_id, radix, child);
            }
            NodeKind::Node48 if self.node48[entry.index()].count < 48 => {
                let id = entry.index();
                let node = &mut self.node48[id];
                let slot = node.count as usize;
                node.child_index[radix as usize] = slot as u8;
                node.children[slot] = child;
                node.count += 1;
            }
            NodeKind::Node48 => {
                self.grow_node48_to_node256(node_id);
                self.add_child(node_id, radix, child);
            }
            NodeKind::Node256 => {
                let id = entry.index();
                let node = &mut self.node256[id];
                if node.children[radix as usize] == EMPTY {
                    node.count += 1;
                }
                node.children[radix as usize] = child;
            }
            NodeKind::Leaf => unreachable!("children cannot be added to leaves"),
        }
    }

    fn grow_node4_to_node16(&mut self, entry_id: NodeId) {
        debug_assert_eq!(self.entries[id_index(entry_id)].kind(), NodeKind::Node4);
        let old_id = self.entries[id_index(entry_id)].index();
        let old = &self.node4[old_id];
        let mut next = Node16 {
            count: old.count,
            keys: [0; 16],
            children: [EMPTY; 16],
        };
        let count = old.count as usize;
        next.keys[..count].copy_from_slice(&old.keys[..count]);
        next.children[..count].copy_from_slice(&old.children[..count]);
        let new_id = self.node16.len();
        self.node16.push(next);
        self.entries[id_index(entry_id)] = TaggedNode::node16(new_id);
    }

    fn grow_node16_to_node32(&mut self, entry_id: NodeId) {
        debug_assert_eq!(self.entries[id_index(entry_id)].kind(), NodeKind::Node16);
        let old_id = self.entries[id_index(entry_id)].index();
        let old = &self.node16[old_id];
        let mut next = Node32 {
            count: old.count,
            keys: [0; 32],
            children: [EMPTY; 32],
        };
        let count = old.count as usize;
        next.keys[..count].copy_from_slice(&old.keys[..count]);
        next.children[..count].copy_from_slice(&old.children[..count]);
        let new_id = self.node32.len();
        self.node32.push(next);
        self.entries[id_index(entry_id)] = TaggedNode::node32(new_id);
    }

    fn grow_node32_to_node48(&mut self, entry_id: NodeId) {
        debug_assert_eq!(self.entries[id_index(entry_id)].kind(), NodeKind::Node32);
        let old_id = self.entries[id_index(entry_id)].index();
        let old = &self.node32[old_id];
        let mut next = Node48 {
            count: old.count,
            child_index: [NODE48_EMPTY; 256],
            children: [EMPTY; 48],
        };
        for slot in 0..old.count as usize {
            next.child_index[old.keys[slot] as usize] = slot as u8;
            next.children[slot] = old.children[slot];
        }
        let new_id = self.node48.len();
        self.node48.push(next);
        self.entries[id_index(entry_id)] = TaggedNode::node48(new_id);
    }

    fn grow_node48_to_node256(&mut self, entry_id: NodeId) {
        debug_assert_eq!(self.entries[id_index(entry_id)].kind(), NodeKind::Node48);
        let old_id = self.entries[id_index(entry_id)].index();
        let old = &self.node48[old_id];
        let mut next = Node256 {
            count: old.count as u16,
            children: [EMPTY; 256],
        };
        for radix in 0..=u8::MAX {
            let slot = old.child_index[radix as usize];
            if slot != NODE48_EMPTY {
                next.children[radix as usize] = old.children[slot as usize];
            }
        }
        let new_id = self.node256.len();
        self.node256.push(next);
        self.entries[id_index(entry_id)] = TaggedNode::node256(new_id);
    }
}

pub fn deterministic_permutation_scatter(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn radix_byte(scattered: u64, depth: usize) -> u8 {
    if depth >= 8 {
        0
    } else {
        (scattered >> ((7 - depth) * 8)) as u8
    }
}

fn find_key4(keys: &[u8; 4], count: u8, needle: u8) -> Option<usize> {
    let live = if count >= 4 {
        0b1111
    } else {
        (1_u32 << count) - 1
    };
    let mask = (((keys[0] == needle) as u32)
        | (((keys[1] == needle) as u32) << 1)
        | (((keys[2] == needle) as u32) << 2)
        | (((keys[3] == needle) as u32) << 3))
        & live;
    (mask != 0).then(|| mask.trailing_zeros() as usize)
}

fn find_key16(keys: &[u8; 16], count: u8, needle: u8) -> Option<usize> {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    return unsafe { find_key16_sse2(keys, count, needle) };
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    find_key_scalar(&keys[..count as usize], needle)
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn find_key_scalar(keys: &[u8], needle: u8) -> Option<usize> {
    keys.iter().position(|&key| key == needle)
}

#[cfg(target_arch = "x86")]
use std::arch::x86;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64 as x86;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
unsafe fn find_key16_sse2(keys: &[u8; 16], count: u8, needle: u8) -> Option<usize> {
    let key_vec = x86::_mm_loadu_si128(keys.as_ptr() as *const x86::__m128i);
    let needle_vec = x86::_mm_set1_epi8(needle as i8);
    let cmp = x86::_mm_cmpeq_epi8(key_vec, needle_vec);
    let mask = x86::_mm_movemask_epi8(cmp) as u32;
    let live_mask = if count >= 16 {
        u16::MAX as u32
    } else {
        (1_u32 << count) - 1
    };
    let hits = mask & live_mask;
    (hits != 0).then(|| hits.trailing_zeros() as usize)
}

fn simd_tier() -> &'static str {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("avx512f") {
            "AVX-512 detected; Node4/Node16 use AVX2/SSE2-compatible 16-byte matching"
        } else if std::is_x86_feature_detected!("avx2") {
            "AVX2"
        } else if std::is_x86_feature_detected!("sse2") {
            "SSE2"
        } else {
            "scalar"
        }
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        "scalar"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_and_update_work() {
        let mut tree = DHarht::new(16);
        assert_eq!(tree.insert(7, "seven"), None);
        assert_eq!(tree.insert(9, "nine"), None);
        assert_eq!(tree.get(7), Some(&"seven"));
        assert_eq!(tree.get(9), Some(&"nine"));
        assert_eq!(tree.get(10), None);
        assert_eq!(tree.insert(7, "SEVEN"), Some("seven"));
        assert_eq!(tree.get(7), Some(&"SEVEN"));
    }

    #[test]
    fn deterministic_shape_is_identical_for_same_insert_order() {
        let keys: Vec<u64> = (0..512).map(|i| i * 17 + 3).collect();
        let mut first = DHarht::new(64);
        let mut second = DHarht::new(64);
        for key in keys {
            first.insert(key, key);
            second.insert(key, key);
        }
        first.seal_for_lookup();
        second.seal_for_lookup();
        assert_eq!(first.allocated_nodes(), second.allocated_nodes());
        assert_eq!(first.approx_memory_bytes(), second.approx_memory_bytes());
        assert_eq!(first.blueprint_cjc(), second.blueprint_cjc());
    }

    #[test]
    fn sealed_lookup_preserves_values_after_leaf_compaction() {
        let mut tree = DHarht::new(256);
        let keys: Vec<u64> = (0..2048)
            .map(|i| deterministic_permutation_scatter(i * 31))
            .collect();
        for (idx, &key) in keys.iter().enumerate() {
            tree.insert(key, idx as u64);
        }
        tree.seal_for_lookup();
        for (idx, &key) in keys.iter().enumerate() {
            assert_eq!(tree.get(key), Some(&(idx as u64)));
        }
        assert_eq!(tree.get(u64::MAX - 1), None);
    }

    #[test]
    fn memory_profile_roundtrips_under_seal() {
        // The profile the experiment recommends: sparse 16-bit front directory.
        let mut tree = DHarht::new(256);
        tree.set_lookup_profile(LookupProfile::Memory);
        let keys: Vec<u64> = (0..4096).map(|i| deterministic_permutation_scatter(i * 7 + 1)).collect();
        for (idx, &key) in keys.iter().enumerate() {
            tree.insert(key, idx as u64);
        }
        tree.seal_for_lookup();
        for (idx, &key) in keys.iter().enumerate() {
            assert_eq!(tree.get(key), Some(&(idx as u64)));
        }
    }

    #[test]
    fn memory_profile_is_deterministic_across_two_builds() {
        // Determinism: same keys, same insert order → identical sealed footprint.
        let keys: Vec<u64> = (0..1024).map(|i| i * 23 + 5).collect();
        let build = || {
            let mut t = DHarht::new(64);
            t.set_lookup_profile(LookupProfile::Memory);
            for &k in &keys {
                t.insert(k, k);
            }
            t.seal_for_lookup();
            (t.allocated_nodes(), t.approx_memory_bytes())
        };
        assert_eq!(build(), build());
    }
}
