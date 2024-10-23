//! Serialization of node types in the database.

use std::{collections::HashMap, str};

use crate::{
    errors::{DeserializeError, DeserializeErrorKind, ErrorContext},
    types::{
        ChildRef, InternalNode, Key, LeafNode, Manifest, Node, Root, TreeTags, ValueHash,
        HASH_SIZE, KEY_SIZE,
    },
};

/// Estimate for the byte size of LEB128-encoded `u64` values. 3 bytes fits values
/// up to `2 ** (3 * 7) = 2_097_152` (exclusive).
const LEB128_SIZE_ESTIMATE: usize = 3;

impl LeafNode {
    pub(super) fn deserialize(bytes: &[u8]) -> Result<Self, DeserializeError> {
        if bytes.len() < KEY_SIZE + HASH_SIZE {
            return Err(DeserializeErrorKind::UnexpectedEof.into());
        }
        let full_key = Key::from_big_endian(&bytes[..KEY_SIZE]);
        let value_hash = ValueHash::from_slice(&bytes[KEY_SIZE..(KEY_SIZE + HASH_SIZE)]);

        let mut bytes = &bytes[(KEY_SIZE + HASH_SIZE)..];
        let leaf_index = leb128::read::unsigned(&mut bytes).map_err(|err| {
            DeserializeErrorKind::Leb128(err).with_context(ErrorContext::LeafIndex)
        })?;
        Ok(Self {
            full_key,
            value_hash,
            leaf_index,
        })
    }

    pub(super) fn serialize(&self, buffer: &mut Vec<u8>) {
        buffer.reserve(KEY_SIZE + HASH_SIZE + LEB128_SIZE_ESTIMATE);
        let mut key_bytes = [0_u8; KEY_SIZE];
        self.full_key.to_big_endian(&mut key_bytes);
        buffer.extend_from_slice(&key_bytes);
        buffer.extend_from_slice(self.value_hash.as_ref());
        leb128::write::unsigned(buffer, self.leaf_index).unwrap();
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(u32)]
enum ChildKind {
    None = 0,
    Internal = 1,
    Leaf = 2,
}

impl ChildKind {
    const MASK: u32 = 3;

    fn deserialize(bitmap_chunk: u32) -> Result<Self, DeserializeError> {
        match bitmap_chunk {
            0 => Ok(Self::None),
            1 => Ok(Self::Internal),
            2 => Ok(Self::Leaf),
            _ => Err(DeserializeErrorKind::InvalidChildKind.into()),
        }
    }
}

impl ChildRef {
    /// Estimated capacity to serialize a `ChildRef`.
    const ESTIMATED_CAPACITY: usize = LEB128_SIZE_ESTIMATE + HASH_SIZE;

    fn deserialize(buffer: &mut &[u8], is_leaf: bool) -> Result<Self, DeserializeError> {
        if buffer.len() < HASH_SIZE {
            let err = DeserializeErrorKind::UnexpectedEof;
            return Err(err.with_context(ErrorContext::ChildRefHash));
        }
        let (hash, rest) = buffer.split_at(HASH_SIZE);
        let hash = ValueHash::from_slice(hash);

        *buffer = rest;
        let version = leb128::read::unsigned(buffer)
            .map_err(|err| DeserializeErrorKind::Leb128(err).with_context(ErrorContext::Version))?;

        Ok(Self {
            hash,
            version,
            is_leaf,
        })
    }

    fn serialize(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(self.hash.as_bytes());
        leb128::write::unsigned(buffer, self.version).unwrap();
        // ^ `unwrap()` is safe; writing to a `Vec<u8>` always succeeds

        // `self.is_leaf` is not serialized here, but rather in `InternalNode::serialize()`
    }

    fn kind(&self) -> ChildKind {
        if self.is_leaf {
            ChildKind::Leaf
        } else {
            ChildKind::Internal
        }
    }
}

impl InternalNode {
    pub(super) fn deserialize(bytes: &[u8]) -> Result<Self, DeserializeError> {
        if bytes.len() < 4 {
            let err = DeserializeErrorKind::UnexpectedEof;
            return Err(err.with_context(ErrorContext::ChildrenMask));
        }
        let (bitmap, mut bytes) = bytes.split_at(4);
        let mut bitmap = u32::from_le_bytes([bitmap[0], bitmap[1], bitmap[2], bitmap[3]]);
        if bitmap == 0 {
            return Err(DeserializeErrorKind::EmptyInternalNode.into());
        }

        // This works because both non-empty `ChildKind`s have exactly one bit set
        // in their binary representation.
        let child_count = bitmap.count_ones();
        let mut this = Self::with_capacity(child_count as usize);
        for i in 0..Self::CHILD_COUNT {
            match ChildKind::deserialize(bitmap & ChildKind::MASK)? {
                ChildKind::None => { /* skip */ }
                ChildKind::Internal => {
                    let child_ref = ChildRef::deserialize(&mut bytes, false)?;
                    this.insert_child_ref(i, child_ref);
                }
                ChildKind::Leaf => {
                    let child_ref = ChildRef::deserialize(&mut bytes, true)?;
                    this.insert_child_ref(i, child_ref);
                }
            }
            bitmap >>= 2;
        }
        Ok(this)
    }

    pub(super) fn serialize(&self, buffer: &mut Vec<u8>) {
        // Creates a bitmap specifying children existence and type (internal node or leaf).
        // Each child occupies 2 bits in the bitmap (i.e., the entire bitmap is 32 bits),
        // with ordering from least significant bits to most significant ones.
        // `0b00` means no child, while bitmap chunks for existing children are determined by
        // `ChildKind`.
        let mut bitmap = 0_u32;
        let mut child_count = 0;
        for (i, child_ref) in self.children() {
            let offset = 2 * u32::from(i);
            bitmap |= (child_ref.kind() as u32) << offset;
            child_count += 1;
        }

        let additional_capacity = 4 + ChildRef::ESTIMATED_CAPACITY * child_count;
        buffer.reserve(additional_capacity);
        buffer.extend_from_slice(&bitmap.to_le_bytes());

        for child_ref in self.child_refs() {
            child_ref.serialize(buffer);
        }
    }
}

impl Root {
    pub(super) fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializeError> {
        let leaf_count = leb128::read::unsigned(&mut bytes).map_err(|err| {
            DeserializeErrorKind::Leb128(err).with_context(ErrorContext::LeafCount)
        })?;
        let node = match leaf_count {
            0 => return Ok(Self::Empty),
            1 => {
                // Try both the leaf and internal node serialization; in some cases, a single leaf
                // may still be persisted as an internal node. Since serialization of an internal node with a single child
                // is always shorter than that a leaf, the order (first leaf, then internal node) is chosen intentionally.
                LeafNode::deserialize(bytes)
                    .map(Node::Leaf)
                    .or_else(|_| InternalNode::deserialize(bytes).map(Node::Internal))?
            }
            _ => Node::Internal(InternalNode::deserialize(bytes)?),
        };
        Ok(Self::new(leaf_count, node))
    }

    pub(super) fn serialize(&self, buffer: &mut Vec<u8>) {
        match self {
            Self::Empty => {
                leb128::write::unsigned(buffer, 0 /* leaf_count */).unwrap();
            }
            Self::Filled { leaf_count, node } => {
                leb128::write::unsigned(buffer, (*leaf_count).into()).unwrap();
                node.serialize(buffer);
            }
        }
    }
}

impl Node {
    pub(super) fn serialize(&self, buffer: &mut Vec<u8>) {
        match self {
            Self::Internal(node) => node.serialize(buffer),
            Self::Leaf(leaf) => leaf.serialize(buffer),
        }
    }
}

impl TreeTags {
    /// Tags are serialized as a length-prefixed list of `(&str, &str)` tuples, where each
    /// `&str` is length-prefixed as well. All lengths are encoded using LEB128.
    /// Custom tag keys are prefixed with `custom.` to ensure they don't intersect with standard tags.
    fn deserialize(bytes: &mut &[u8]) -> Result<Self, DeserializeError> {
        let tag_count = leb128::read::unsigned(bytes).map_err(DeserializeErrorKind::Leb128)?;
        let mut architecture = None;
        let mut hasher = None;
        let mut depth = None;
        let mut is_recovering = false;
        let mut custom = HashMap::new();

        for _ in 0..tag_count {
            let key = Self::deserialize_str(bytes)?;
            let value = Self::deserialize_str(bytes)?;
            match key {
                "architecture" => architecture = Some(value.to_owned()),
                "hasher" => hasher = Some(value.to_owned()),
                "depth" => {
                    let parsed = value.parse::<usize>().map_err(|err| {
                        DeserializeErrorKind::MalformedTag {
                            name: "depth",
                            err: err.into(),
                        }
                    })?;
                    depth = Some(parsed);
                }
                "is_recovering" => {
                    let parsed = value.parse::<bool>().map_err(|err| {
                        DeserializeErrorKind::MalformedTag {
                            name: "is_recovering",
                            err: err.into(),
                        }
                    })?;
                    is_recovering = parsed;
                }
                key => {
                    if let Some(custom_key) = key.strip_prefix("custom.") {
                        custom.insert(custom_key.to_owned(), value.to_owned());
                    } else {
                        return Err(DeserializeErrorKind::UnknownTag(key.to_owned()).into());
                    }
                }
            }
        }
        Ok(Self {
            architecture: architecture.ok_or(DeserializeErrorKind::MissingTag("architecture"))?,
            hasher: hasher.ok_or(DeserializeErrorKind::MissingTag("hasher"))?,
            depth: depth.ok_or(DeserializeErrorKind::MissingTag("depth"))?,
            is_recovering,
            custom,
        })
    }

    fn deserialize_str<'a>(bytes: &mut &'a [u8]) -> Result<&'a str, DeserializeErrorKind> {
        let str_len = leb128::read::unsigned(bytes).map_err(DeserializeErrorKind::Leb128)?;
        let str_len = usize::try_from(str_len).map_err(|_| DeserializeErrorKind::UnexpectedEof)?;

        if bytes.len() < str_len {
            return Err(DeserializeErrorKind::UnexpectedEof);
        }
        let (s, rest) = bytes.split_at(str_len);
        *bytes = rest;
        str::from_utf8(s).map_err(DeserializeErrorKind::Utf8)
    }

    fn serialize_str(bytes: &mut Vec<u8>, s: &str) {
        leb128::write::unsigned(bytes, s.len() as u64).unwrap();
        bytes.extend_from_slice(s.as_bytes());
    }

    fn serialize(&self, buffer: &mut Vec<u8>) {
        let entry_count = 3 + u64::from(self.is_recovering) + self.custom.len() as u64;
        leb128::write::unsigned(buffer, entry_count).unwrap();

        Self::serialize_str(buffer, "architecture");
        Self::serialize_str(buffer, &self.architecture);
        Self::serialize_str(buffer, "depth");
        Self::serialize_str(buffer, &self.depth.to_string());
        Self::serialize_str(buffer, "hasher");
        Self::serialize_str(buffer, &self.hasher);
        if self.is_recovering {
            Self::serialize_str(buffer, "is_recovering");
            Self::serialize_str(buffer, "true");
        }

        for (custom_key, value) in &self.custom {
            Self::serialize_str(buffer, &format!("custom.{custom_key}"));
            Self::serialize_str(buffer, value);
        }
    }
}

impl Manifest {
    pub(super) fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializeError> {
        let version_count =
            leb128::read::unsigned(&mut bytes).map_err(DeserializeErrorKind::Leb128)?;
        let tags = if bytes.is_empty() {
            None
        } else {
            Some(TreeTags::deserialize(&mut bytes)?)
        };

        Ok(Self {
            version_count,
            tags,
        })
    }

    pub(super) fn serialize(&self, buffer: &mut Vec<u8>) {
        leb128::write::unsigned(buffer, self.version_count).unwrap();
        if let Some(tags) = &self.tags {
            tags.serialize(buffer);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{env, path::Path};

    use hex;
    use itertools::Itertools;
    use zksync_types::{writes::TreeWrite, AccountTreeId, StorageKey, H256};

    use super::*;
    use crate::{
        storage::{LoadAncestorsResult, SortedKeys, TreeUpdater},
        types::{Nibbles, NodeKey, TreeEntry},
        Database, PatchSet, RocksDBWrapper, TreeInstruction,
    };

    fn filter_write_instructions(instructions: &[TreeInstruction]) -> Vec<TreeEntry> {
        let kvs = instructions
            .iter()
            .filter_map(|instruction| match instruction {
                TreeInstruction::Write(entry) => Some(*entry),
                TreeInstruction::Read(_) => None,
            });
        kvs.collect()
    }

    #[test]
    fn serializing_manifest() {
        let manifest_buffer = hex::decode("ACC125030C61726368697465637475726506415231364D5405646570746803323536066861736865720A626C616B653273323536").unwrap();
        let manifest = Manifest::deserialize(&manifest_buffer).unwrap();
        println!("Manifest: {:?}", manifest);

        let version = 614572;
        let base_version = version - 1;
        println!(
            "Root key: {}",
            hex::encode(&NodeKey::empty(base_version).to_db_key())
        );

        let root_buffer = hex::decode("91E4ABB2035555555500CBEFC4D9ABEB93BCD9D8FF868BAFD5C060E3FCCD6ABDC436A97A4C45EE1773ABC1252C90CAD0114668F0D93904BCF8B022803B609B2B2C831D97C736205013DA85AEABC125B5A298502C74E165532D44AFE53FE7D7D3056DB8D2AAF8BEBC5A9F7DA6E7135FABC125D93CE06F81B93923347928BAC9A657B4AECE4EDF1647BD746ACF185A010CCADCABC12534E21B5AD837646336A70A67AEA6911B19A54712E6213918D3D06134C3B7E7F2ABC12544AA366B3ACB2586D990F6ECAD25A81AB58E7C032FF2879F165C7A77B543193CABC125BA1A8B1EDA7FE2EA2D359C3026A1C5013D966435A375FCCAF39ED7F9444802E0ABC1259CD897CDD87AE58439B4D54738B6C7F81A8A14E0EF493637392A6CFDE670735BABC125002413EE497C76ED15ED38966ACD689CB93485C9A31321DB139B74C6D6B3B915ABC125FDC5612BE1A065F3A30D8DA5E6E91BDF11AEC0F928DBB26415B665AE3023CC10ABC125847C41CD6EEFE50B5D025CBD26658672BB46C18785C700557DC09087241CF977ABC1254C8DCF046564B7D80E9641EC184356F93B6BF4DE7A2FE6C01E164D444E459971ABC12552D5928DB5024A8D6DF9126F3F7A37B7217BF3D114F7D0EBEDA974379C604A45ABC1257A5025BB56BA4E535AFF53B4CBB0F4A66804CA1DDB050E8656DB0E2D0DA56172ABC1250820B68B73F1969FE1F6BC7E1DE438EDE3624F0E479372400E72905BB085E07CABC12597C285D9F00CFE938396B51CD1124B42442BEC79532CBC0C9A2651E4404E1010ABC125").unwrap();
        let root = Root::deserialize(&root_buffer).unwrap();
        println!("Root: {:?}", root);

        let mut tree_updater = TreeUpdater::new(version, root);

        let tree_writes_buffer = hex::decode(include_str!("tree_writes.hex")).unwrap();
        let tree_writes: Vec<TreeWrite> = bincode::deserialize(&tree_writes_buffer).unwrap();
        println!("Processing tree writes with {} entries", tree_writes.len());

        // If tree writes are present in DB then simply use them.
        let writes = tree_writes.into_iter().map(|tree_write| {
            let storage_key =
                StorageKey::new(AccountTreeId::new(tree_write.address), tree_write.key);
            TreeInstruction::write(storage_key, tree_write.leaf_index, tree_write.value)
        });
        let reads = vec![];

        let storage_logs: Vec<TreeInstruction> = writes
            .chain(reads)
            .sorted_by_key(|tree_instruction| tree_instruction.key())
            .map(TreeInstruction::with_hashed_key)
            .collect();
        let entries = filter_write_instructions(&storage_logs);
        let entries = entries
            .iter()
            .take(env::var("ENTRIES").unwrap_or("1".to_string()).parse()?);
        let sorted_keys = SortedKeys::new(entries.map(|entry| entry.key));

        let db = RocksDBWrapper::new(&Path::new("/db/lightweight-new")).unwrap();
        tree_updater.load_ancestors(&sorted_keys, &db);

        // let manifest = Manifest::new(42, &());
        // let mut buffer = vec![];
        // manifest.serialize(&mut buffer);
        // assert_eq!(buffer[0], 42); // version count
        // assert_eq!(buffer[1], 3); // number of tags
        // assert_eq!(
        //     buffer[2..],
        //     *b"\x0Carchitecture\x06AR16MT\x05depth\x03256\x06hasher\x08no_op256"
        // );
        // // ^ length-prefixed tag names and values
        //
        // let manifest_copy = Manifest::deserialize(&buffer).unwrap();
        // assert_eq!(manifest_copy, manifest);
    }

    #[test]
    fn serializing_manifest_with_recovery_flag() {
        let mut manifest = Manifest::new(42, &());
        manifest.tags.as_mut().unwrap().is_recovering = true;
        let mut buffer = vec![];
        manifest.serialize(&mut buffer);
        assert_eq!(buffer[0], 42); // version count
        assert_eq!(buffer[1], 4); // number of tags
        assert_eq!(
            buffer[2..],
            *b"\x0Carchitecture\x06AR16MT\x05depth\x03256\x06hasher\x08no_op256\x0Dis_recovering\x04true"
        );
        // ^ length-prefixed tag names and values

        let manifest_copy = Manifest::deserialize(&buffer).unwrap();
        assert_eq!(manifest_copy, manifest);
    }

    #[test]
    fn serializing_manifest_with_custom_tags() {
        let mut manifest = Manifest::new(42, &());
        // Test a single custom tag first to not deal with non-determinism when enumerating tags.
        manifest.tags.as_mut().unwrap().custom =
            HashMap::from([("test".to_owned(), "1".to_owned())]);
        let mut buffer = vec![];
        manifest.serialize(&mut buffer);
        assert_eq!(buffer[0], 42); // version count
        assert_eq!(buffer[1], 4); // number of tags (3 standard + 1 custom)
        assert_eq!(
            buffer[2..],
            *b"\x0Carchitecture\x06AR16MT\x05depth\x03256\x06hasher\x08no_op256\x0Bcustom.test\x011"
        );

        let manifest_copy = Manifest::deserialize(&buffer).unwrap();
        assert_eq!(manifest_copy, manifest);

        // Test multiple tags.
        let tags = manifest.tags.as_mut().unwrap();
        tags.is_recovering = true;
        tags.custom = HashMap::from([
            ("test".to_owned(), "1".to_owned()),
            ("other.long.tag".to_owned(), "123456!!!".to_owned()),
        ]);
        let mut buffer = vec![];
        manifest.serialize(&mut buffer);
        assert_eq!(buffer[0], 42); // version count
        assert_eq!(buffer[1], 6); // number of tags (4 standard + 2 custom)

        let manifest_copy = Manifest::deserialize(&buffer).unwrap();
        assert_eq!(manifest_copy, manifest);
    }

    #[test]
    fn manifest_serialization_errors() {
        let manifest = Manifest::new(42, &());
        let mut buffer = vec![];
        manifest.serialize(&mut buffer);

        // Replace "architecture" -> "Architecture"
        let mut mangled_buffer = buffer.clone();
        mangled_buffer[3] = b'A';
        let err = Manifest::deserialize(&mangled_buffer).unwrap_err();
        let err = err.to_string();
        assert!(
            err.contains("unknown tag `Architecture` in tree manifest"),
            "{err}"
        );

        let mut mangled_buffer = buffer.clone();
        mangled_buffer.truncate(mangled_buffer.len() - 1);
        let err = Manifest::deserialize(&mangled_buffer).unwrap_err();
        let err = err.to_string();
        assert!(err.contains("unexpected end of input"), "{err}");

        // Remove the `hasher` tag.
        let mut mangled_buffer = buffer.clone();
        mangled_buffer[1] = 2; // decreased number of tags
        let err = Manifest::deserialize(&mangled_buffer).unwrap_err();
        let err = err.to_string();
        assert!(
            err.contains("missing required tag `hasher` in tree manifest"),
            "{err}"
        );
    }

    #[test]
    fn serializing_leaf_node() {
        let leaf = LeafNode::new(TreeEntry::new(513.into(), 42, H256([4; 32])));
        let mut buffer = vec![];
        leaf.serialize(&mut buffer);
        assert_eq!(buffer[..30], [0; 30]); // padding for the key
        assert_eq!(buffer[30..32], [2, 1]); // lower 2 bytes of the key
        assert_eq!(buffer[32..64], [4; 32]); // value hash
        assert_eq!(buffer[64], 42); // leaf index
        assert_eq!(buffer.len(), 65);

        let leaf_copy = LeafNode::deserialize(&buffer).unwrap();
        assert_eq!(leaf_copy, leaf);
    }

    fn create_internal_node() -> InternalNode {
        let mut node = InternalNode::default();
        node.insert_child_ref(1, ChildRef::internal(3));
        node.child_ref_mut(1).unwrap().hash = H256([1; 32]);
        node.insert_child_ref(0xb, ChildRef::leaf(2));
        node.child_ref_mut(0xb).unwrap().hash = H256([11; 32]);
        node
    }

    #[test]
    fn serializing_internal_node() {
        let node = create_internal_node();
        let mut buffer = vec![];
        node.serialize(&mut buffer);
        assert_eq!(buffer[..4], [4, 0, 128, 0]);
        // ^ bitmap (`4 == ChildKind::Internal << 2`, `128 == ChildKind::Leaf << 6`).
        assert_eq!(buffer[4..36], [1; 32]); // hash of the child at 1
        assert_eq!(buffer[36], 3); // version of the child at 1
        assert_eq!(buffer[37..69], [11; 32]); // hash of the child at b
        assert_eq!(buffer[69], 2); // version of the child at b
        assert_eq!(buffer.len(), 70);

        // Check that the child count estimate works correctly.
        let bitmap = u32::from_le_bytes([4, 0, 128, 0]);
        let child_count = bitmap.count_ones();
        assert_eq!(child_count, 2);

        let node_copy = InternalNode::deserialize(&buffer).unwrap();
        assert_eq!(node_copy, node);
    }

    #[test]
    fn serializing_empty_root() {
        let root = Root::Empty;
        let mut buffer = vec![];
        root.serialize(&mut buffer);
        assert_eq!(buffer, [0]);

        let root_copy = Root::deserialize(&buffer).unwrap();
        assert_eq!(root_copy, root);
    }

    #[test]
    fn serializing_root_with_leaf() {
        let leaf = LeafNode::new(TreeEntry::new(513.into(), 42, H256([4; 32])));
        let root = Root::new(1, leaf.into());
        let mut buffer = vec![];
        root.serialize(&mut buffer);
        assert_eq!(buffer[0], 1);

        let root_copy = Root::deserialize(&buffer).unwrap();
        assert_eq!(root_copy, root);
    }

    #[test]
    fn serializing_root_with_internal_node() {
        let node = create_internal_node();
        let root = Root::new(2, node.into());
        let mut buffer = vec![];
        root.serialize(&mut buffer);
        assert_eq!(buffer[0], 2);

        let root_copy = Root::deserialize(&buffer).unwrap();
        assert_eq!(root_copy, root);
    }
}
