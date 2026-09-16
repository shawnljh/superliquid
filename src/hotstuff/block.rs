use std::{
    collections::HashMap,
    sync::{Arc, RwLock, RwLockReadGuard},
    vec,
};

use crate::types::{
    consensus::ViewNumber,
    transaction::{Sha256Hash, SignedTransaction},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::crypto::{self, QuorumCertificate};

pub type BlockHash = Sha256Hash;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Block {
    Genesis {
        transactions: Vec<SignedTransaction>,
        view_number: ViewNumber,
        justify: QuorumCertificate,
        merkle_root: Sha256Hash,
    },
    Normal {
        parent_id: BlockHash,
        transactions: Vec<SignedTransaction>,
        view_number: ViewNumber,
        justify: QuorumCertificate,
        merkle_root: Sha256Hash,
        // proposer: PublicKeyString,
        // block_hash: TODO
    },
}

#[derive(Serialize, Deserialize)]
struct HashableBlock {
    parent_id: BlockHash,
    merkle_root: Sha256Hash,
    view_number: ViewNumber,
}

impl Block {
    pub fn create_leaf(
        parent: &Block,
        transactions: Vec<SignedTransaction>,
        view_number: ViewNumber,
        justify: QuorumCertificate,
    ) -> Self {
        Self::Normal {
            parent_id: parent.hash(),
            view_number,
            justify,
            merkle_root: Self::hash_transactions(&transactions),
            transactions,
        }
    }

    pub fn extends_from(
        &self,
        locked_block_hash: BlockHash,
        block_store: &HashMap<BlockHash, Arc<RwLock<Block>>>,
    ) -> bool {
        let mut current = self;
        let mut parent: RwLockReadGuard<'_, Block>;

        // check 3 parents up
        for _ in 0..3 {
            match current {
                Block::Genesis { .. } => {
                    return false;
                }
                Block::Normal { parent_id, .. } => {
                    if *parent_id == locked_block_hash {
                        return true;
                    }
                    let temp = match block_store.get(parent_id) {
                        Some(node) => node.read().unwrap(),
                        None => {
                            return false;
                        } // missing parent, unsafe
                    };

                    parent = temp;
                    current = &parent;
                }
            }
        }
        false
    }

    pub fn compute_merkle_root(mut hashes: Vec<[u8; 32]>) -> Sha256Hash {
        if hashes.is_empty() {
            return [0u8; 32];
        }

        if hashes.len() == 1 {
            return hashes[0];
        }

        if hashes.len() % 2 == 1 {
            hashes.push(*hashes.last().unwrap());
        }

        let mut len = hashes.len();
        while len > 1 {
            for i in (0..len).step_by(2) {
                let mut buf = [0u8; 64];
                buf[..32].copy_from_slice(&hashes[i]);
                buf[32..].copy_from_slice(&hashes[i + 1]);
                hashes[i / 2] = Sha256::digest(&buf).into();
            }
            len = (len + 1) / 2;

            if len % 2 == 1 && len > 1 {
                hashes[len] = hashes[len - 1]; // safe only if hashes has enough capacity
                len += 1;
            }
        }

        hashes[0]
    }

    pub fn hash_transactions(transactions: &Vec<SignedTransaction>) -> Sha256Hash {
        if transactions.len() == 0 {
            return Sha256Hash::default();
        }

        let hashes = transactions.iter().map(|tx| tx.hash()).collect::<Vec<_>>();
        Self::compute_merkle_root(hashes)
    }

    pub fn hash_block_transaction(&self) -> Sha256Hash {
        Self::hash_transactions(self.transactions())
    }

    pub fn hash(&self) -> BlockHash {
        match self {
            Self::Genesis { .. } => Sha256::digest(b"GENESIS").into(),
            Self::Normal {
                parent_id,
                view_number,
                merkle_root,
                ..
            } => {
                let hashable = HashableBlock {
                    parent_id: *parent_id,
                    merkle_root: *merkle_root,
                    view_number: *view_number,
                };

                let encoded = bincode::serialize(&hashable).unwrap();
                Sha256::digest(&encoded).into()
            }
        }
    }

    pub fn transactions(&self) -> &Vec<SignedTransaction> {
        let (Block::Genesis { transactions, .. } | Block::Normal { transactions, .. }) = self;
        transactions
    }

    pub fn transactions_mut(&mut self) -> &mut Vec<SignedTransaction> {
        let (Block::Genesis { transactions, .. } | Block::Normal { transactions, .. }) = self;
        transactions
    }

    pub fn merkle_root(&self) -> Sha256Hash {
        let (Block::Genesis { merkle_root, .. } | Block::Normal { merkle_root, .. }) = self;
        *merkle_root
    }

    pub fn create_genesis_block() -> (Block, QuorumCertificate) {
        let qc = crypto::QuorumCertificate::create_genesis_qc();

        let genesis = Block::Genesis {
            view_number: 0,
            justify: qc.clone(),
            transactions: vec![],
            merkle_root: Sha256Hash::default(),
        };
        return (genesis, qc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    impl Block {
        pub fn mock(view_number: ViewNumber) -> Self {
            Block::Genesis {
                transactions: vec![],
                view_number,
                justify: QuorumCertificate::mock(view_number),
                merkle_root: Sha256Hash::default(),
            }
        }
    }

    fn hash(data: &[u8]) -> [u8; 32] {
        Sha256::digest(data).into()
    }

    #[test]
    fn test_empty_hash_list() {
        let hashes = vec![];
        let root = Block::compute_merkle_root(hashes);
        assert_eq!(root, [0u8; 32]);
    }

    #[test]
    fn test_single_hash() {
        let leaf = hash(b"leaf");
        let root = Block::compute_merkle_root(vec![leaf]);
        assert_eq!(root, leaf);
    }

    #[test]
    fn test_even_number_of_hashes() {
        let h1 = hash(b"a");
        let h2 = hash(b"b");
        let h3 = hash(b"c");
        let h4 = hash(b"d");

        let root = Block::compute_merkle_root(vec![h1, h2, h3, h4]);

        // Manually compute
        let l1: [u8; 32] = Sha256::digest(&[h1, h2].concat()).into();
        let l2: [u8; 32] = Sha256::digest(&[h3, h4].concat()).into();
        let expected_root: [u8; 32] = Sha256::digest(&[l1, l2].concat()).into();

        assert_eq!(root, expected_root);
    }

    #[test]
    fn test_odd_number_of_hashes() {
        let h1 = hash(b"x");
        let h2 = hash(b"y");
        let h3 = hash(b"z");

        let root = Block::compute_merkle_root(vec![h1, h2, h3]);

        // After duplication, tree will be built on [h1, h2, h3, h3]
        let l1: [u8; 32] = Sha256::digest(&[h1, h2].concat()).into();
        let l2: [u8; 32] = Sha256::digest(&[h3, h3].concat()).into();
        let expected_root: [u8; 32] = Sha256::digest(&[l1, l2].concat()).into();

        assert_eq!(root, expected_root);
    }

    #[test]
    fn test_10_hashes() {
        let h1 = hash(b"a");
        let h2 = hash(b"b");
        let h3 = hash(b"c");
        let h4 = hash(b"d");
        let h5 = hash(b"e");
        let h6 = hash(b"f");
        let h7 = hash(b"g");

        let root = Block::compute_merkle_root(vec![h1, h2, h3, h4, h5, h6, h7]);

        let l1: [u8; 32] = Sha256::digest(&[h1, h2].concat()).into();
        let l2: [u8; 32] = Sha256::digest(&[h3, h4].concat()).into();
        let l3: [u8; 32] = Sha256::digest(&[h5, h6].concat()).into();
        let l4: [u8; 32] = Sha256::digest(&[h7, h7].concat()).into();
        let l5: [u8; 32] = Sha256::digest(&[l1, l2].concat()).into();
        let l6: [u8; 32] = Sha256::digest(&[l3, l4].concat()).into();
        let expected_root: [u8; 32] = Sha256::digest(&[l5, l6].concat()).into();

        assert_eq!(root, expected_root);
    }

    #[test]
    fn test_merkle_root_is_not_commutative() {
        let h1 = hash(b"1");
        let h2 = hash(b"2");

        let root1 = Block::compute_merkle_root(vec![h1, h2]);
        let root2 = Block::compute_merkle_root(vec![h2, h1]);

        assert_ne!(root1, root2);
    }
}
