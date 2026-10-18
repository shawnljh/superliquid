use std::collections::HashSet;

use ed25519::Signature;
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    hotstuff::message::{HashableMessage, HotStuffMessage, UnsignedVote},
    types::{consensus::ViewNumber, transaction::Sha256Hash},
};

use super::{block::BlockHash, hexstring};

pub type PartialSigHash = Sha256Hash;
pub type QuorumCertificateHash = Sha256Hash;

#[derive(Serialize)]
struct HashablePartialSig<'a> {
    signer_id_bytes: &'a [u8; 32],
    signature_bytes: &'a [u8],
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PartialSig {
    #[serde(
        serialize_with = "hexstring::serialize_verifying_key",
        deserialize_with = "hexstring::deserialize_verifying_key"
    )]
    pub signer_id: VerifyingKey,

    #[serde(
        serialize_with = "hexstring::serialize_signature",
        deserialize_with = "hexstring::deserialize_signature"
    )]
    pub signature: Signature,
}

impl PartialSig {
    pub fn new(signer_id: VerifyingKey, signature: Signature) -> Self {
        Self {
            signer_id,
            signature,
        }
    }

    pub fn hash(&self) -> PartialSigHash {
        let signer_bytes: &[u8; 32] = &self.signer_id.to_bytes();
        let sig_bytes: &[u8; 64] = &self.signature.to_bytes();

        let hashable = HashablePartialSig {
            signer_id_bytes: signer_bytes,
            signature_bytes: sig_bytes,
        };

        let encoded = bincode::serialize(&hashable).expect("bincode failed");
        Sha256::digest(&encoded).into()
    }
}

#[derive(Serialize)]
struct HashableQC {
    view_number: ViewNumber,
    block_hash: BlockHash,
    message_hash: Sha256Hash,
    sig_hashes: Vec<Sha256Hash>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct QuorumCertificate {
    pub(crate) view_number: ViewNumber,
    pub(crate) block_hash: BlockHash,
    pub(crate) message_hash: Sha256Hash,
    pub(crate) partial_sigs: Vec<PartialSig>, // Signatures forming the QC
}

impl QuorumCertificate {
    const GENESIS_BLOCK_HASH: [u8; 32] = [
        144, 17, 49, 216, 56, 177, 122, 172, 15, 120, 133, 184, 30, 3, 203, 220, 159, 81, 87, 160,
        3, 67, 211, 10, 178, 32, 131, 104, 94, 209, 65, 106,
    ];

    pub(crate) fn create_genesis_qc() -> QuorumCertificate {
        QuorumCertificate {
            view_number: 0,
            block_hash: Self::GENESIS_BLOCK_HASH,
            message_hash: [0u8; 32],
            partial_sigs: vec![],
        }
    }

    pub fn from_signatures(
        view_number: ViewNumber,
        block_hash: BlockHash,
        message_hash: [u8; 32],
        partial_sigs: Vec<&PartialSig>,
    ) -> Self {
        assert!(
            !partial_sigs.is_empty(),
            "from_signatures requires at least one partial signature"
        );
        let partial_sigs: Vec<PartialSig> = partial_sigs.into_iter().cloned().collect();

        QuorumCertificate {
            view_number,
            block_hash,
            message_hash,
            partial_sigs,
        }
    }

    // Verifies that the QC is valid
    // - Passes if
    //   Is Genesis QC
    //   OR (
    //     expected message_hash (block_hash + view_number) matches the hash that the signers signed
    //     AND
    //     valid signaures >= qourum threshold (n - f)
    //   )
    pub fn verify(&self, validator_set: &HashSet<VerifyingKey>, quorum_size: usize) -> bool {
        if self.view_number == 0 && self.block_hash == Self::GENESIS_BLOCK_HASH {
            // Genesis QC
            return true;
        }

        let vote = UnsignedVote::new(self.view_number, self.block_hash);
        let expected_message_hash = vote.hash();

        if expected_message_hash != self.message_hash {
            return false;
        }

        let mut unique_signers = HashSet::new();
        let mut valid_sig_count = 0;

        for sig in &self.partial_sigs {
            if valid_sig_count >= quorum_size {
                return true;
            }
            let pk = &sig.signer_id;
            if !validator_set.contains(pk) {
                continue;
            }

            if unique_signers.contains(pk) {
                continue;
            }

            if pk
                .verify_strict(&expected_message_hash, &sig.signature)
                .is_ok()
            {
                valid_sig_count += 1;
                unique_signers.insert(pk);
            }
        }

        valid_sig_count >= quorum_size
    }

    pub fn hash(&self) -> QuorumCertificateHash {
        let sig_hashes = self.partial_sigs.iter().map(|sig| sig.hash()).collect();

        let hashable = HashableQC {
            view_number: self.view_number,
            block_hash: self.block_hash,
            message_hash: self.message_hash,
            sig_hashes,
        };

        let encoded = bincode::serialize(&hashable).expect("bincode failed");
        Sha256::digest(&encoded).into()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ed25519::signature::SignerMut;
    use ed25519_dalek::{SigningKey, VerifyingKey};

    use crate::{
        hotstuff::{block::BlockHash, crypto::PartialSig, message::UnsignedVote},
        types::{consensus::ViewNumber, transaction::Sha256Hash},
    };

    use super::QuorumCertificate;

    impl PartialSig {
        pub fn mock(seed: u8) -> Self {
            let mut signing_key = SigningKey::from_bytes(&[seed; 32]);
            let verifying_key = signing_key.verifying_key();
            let signature = signing_key.sign(&[0u8; 32]);

            PartialSig::new(verifying_key, signature)
        }
    }

    impl QuorumCertificate {
        pub fn mock(view_number: ViewNumber) -> Self {
            QuorumCertificate {
                view_number,
                block_hash: [0u8; 32],
                message_hash: [0u8; 32],
                partial_sigs: vec![],
            }
        }
    }

    // ---- helpers ----

    const VIEW: ViewNumber = 5;
    const BLOCK: BlockHash = [3u8; 32];

    /// Deterministic signing keys, seeded 1..=n.
    fn keys(n: u8) -> Vec<SigningKey> {
        (1..=n)
            .map(|seed| SigningKey::from_bytes(&[seed; 32]))
            .collect()
    }

    fn validator_set(keys: &[SigningKey]) -> HashSet<VerifyingKey> {
        keys.iter().map(|k| k.verifying_key()).collect()
    }

    fn vote_hash(view: ViewNumber, block_hash: BlockHash) -> Sha256Hash {
        UnsignedVote::new(view, block_hash).hash()
    }

    fn sign(key: &mut SigningKey, msg: &Sha256Hash) -> PartialSig {
        PartialSig::new(key.verifying_key(), key.sign(msg))
    }

    /// A correctly formed QC for (view, block_hash), signed by every key in `signers`.
    fn signed_qc(
        signers: &mut [SigningKey],
        view: ViewNumber,
        block_hash: BlockHash,
    ) -> QuorumCertificate {
        let message_hash = vote_hash(view, block_hash);
        let partial_sigs = signers.iter_mut().map(|k| sign(k, &message_hash)).collect();

        QuorumCertificate {
            view_number: view,
            block_hash,
            message_hash,
            partial_sigs,
        }
    }

    // ---- genesis ----

    #[test]
    fn test_create_genesis_qc_defaults() {
        let qc = QuorumCertificate::create_genesis_qc();
        assert_eq!(qc.view_number, 0);
        assert_eq!(
            qc.block_hash,
            [
                144, 17, 49, 216, 56, 177, 122, 172, 15, 120, 133, 184, 30, 3, 203, 220, 159, 81,
                87, 160, 3, 67, 211, 10, 178, 32, 131, 104, 94, 209, 65, 106,
            ]
        );
        assert_eq!(qc.message_hash, [0u8; 32]);
        assert!(qc.partial_sigs.is_empty());
    }

    #[test]
    fn verify_accepts_genesis_qc() {
        let qc = QuorumCertificate::create_genesis_qc();
        assert!(qc.verify(&HashSet::new(), 3));
    }

    // ---- valid QCs ----

    #[test]
    fn verify_accepts_exact_quorum() {
        let mut keys = keys(4);
        let validators = validator_set(&keys);
        let qc = signed_qc(&mut keys[..3], VIEW, BLOCK);

        assert!(qc.verify(&validators, 3));
        assert!(!qc.verify(&validators, 4)); // not enough
    }

    #[test]
    fn verify_accepts_more_than_quorum() {
        let mut keys = keys(4);
        let validators = validator_set(&keys);
        let qc = signed_qc(&mut keys, VIEW, BLOCK);

        assert!(qc.verify(&validators, 3));
    }

    // ---- forged fields ----

    #[test]
    fn verify_rejects_forged_view_number() {
        // Regression: a real QC with its view_number changed must not verify.
        let mut keys = keys(4);
        let validators = validator_set(&keys);
        let mut qc = signed_qc(&mut keys[..3], VIEW, BLOCK);
        qc.view_number = 1_000_000;

        assert!(!qc.verify(&validators, 3));
    }

    #[test]
    fn verify_rejects_forged_block_hash() {
        let mut keys = keys(4);
        let validators = validator_set(&keys);
        let mut qc = signed_qc(&mut keys[..3], VIEW, BLOCK);
        qc.block_hash = [9u8; 32];

        assert!(!qc.verify(&validators, 3));
    }

    #[test]
    fn verify_rejects_message_hash_for_different_vote() {
        // Signatures and message_hash agree with each other, but not with (view, block).
        let mut keys = keys(4);
        let validators = validator_set(&keys);
        let other_hash = vote_hash(VIEW + 1, BLOCK);
        let partial_sigs = keys[..3].iter_mut().map(|k| sign(k, &other_hash)).collect();

        let qc = QuorumCertificate {
            view_number: VIEW,
            block_hash: BLOCK,
            message_hash: other_hash,
            partial_sigs,
        };

        assert!(!qc.verify(&validators, 3));
    }

    #[test]
    fn verify_rejects_signatures_over_wrong_message() {
        let mut keys = keys(4);
        let validators = validator_set(&keys);
        let wrong = Sha256Hash::from([9u8; 32]);
        let partial_sigs = keys[..3].iter_mut().map(|k| sign(k, &wrong)).collect();

        let qc = QuorumCertificate {
            view_number: VIEW,
            block_hash: BLOCK,
            message_hash: vote_hash(VIEW, BLOCK),
            partial_sigs,
        };

        assert!(!qc.verify(&validators, 1));
    }

    // ---- counting rules ----

    #[test]
    fn verify_ignores_signers_outside_validator_set() {
        let mut keys = keys(5);
        let validators = validator_set(&keys[..3]); // seeds 1, 2, 3
        let qc = signed_qc(&mut keys[1..], VIEW, BLOCK); // seeds 2..=5, only 2 and 3 count

        assert!(qc.verify(&validators, 2));
        assert!(!qc.verify(&validators, 3));
    }

    #[test]
    fn verify_counts_duplicate_signer_once() {
        let mut keys = keys(4);
        let validators = validator_set(&keys);
        let hash = vote_hash(VIEW, BLOCK);
        let sig = sign(&mut keys[0], &hash);

        let qc = QuorumCertificate {
            view_number: VIEW,
            block_hash: BLOCK,
            message_hash: hash,
            partial_sigs: vec![sig.clone(), sig.clone(), sig],
        };

        assert!(qc.verify(&validators, 1));
        assert!(!qc.verify(&validators, 2));
    }

    #[test]
    fn verify_invalid_signature_does_not_block_signers_valid_one() {
        // Regression: a relayer inserts a garbage signature for an honest signer
        // ahead of their real one. The real one must still count.
        let mut keys = keys(4);
        let validators = validator_set(&keys);
        let mut qc = signed_qc(&mut keys[..3], VIEW, BLOCK);
        let garbage = sign(&mut keys[0], &Sha256Hash::from([9u8; 32]));
        qc.partial_sigs.insert(0, garbage);

        assert!(qc.verify(&validators, 3));
    }

    // ---- serialization ----

    #[test]
    fn test_signature_serialization_round_trip() {
        let mut signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let verifying_key = signing_key.verifying_key();
        let message = b"hello consensus";
        let signature = signing_key.sign(message);

        let partialsig = PartialSig::new(verifying_key, signature);

        let json = serde_json::to_string(&partialsig).expect("Failed to serialize signature");
        let deserialized: PartialSig =
            serde_json::from_str(&json).expect("Failed to deserialize signature");

        assert_eq!(
            partialsig.signature.to_bytes(),
            deserialized.signature.to_bytes()
        );
        assert_eq!(
            partialsig.signer_id.to_bytes(),
            deserialized.signer_id.to_bytes()
        );
    }
}
