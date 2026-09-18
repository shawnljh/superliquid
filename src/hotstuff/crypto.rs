use std::collections::HashSet;

use ed25519::Signature;
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::types::{consensus::ViewNumber, transaction::Sha256Hash};

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

    pub fn verify(&self, validator_set: &HashSet<VerifyingKey>, quorum_size: usize) -> bool {
        let mut unique_signers = HashSet::new();

        let mut valid_sig_count = 0;

        if self.view_number == 0 && self.block_hash == Self::GENESIS_BLOCK_HASH {
            // Genesis QC
            return true;
        }

        for sig in &self.partial_sigs {
            let pk = &sig.signer_id;
            // count signatures only from known validators
            if !validator_set.contains(pk) {
                continue;
            }

            if !unique_signers.insert(pk) {
                continue;
            }

            // verify that signatures are valid
            if pk.verify_strict(&self.message_hash, &sig.signature).is_ok() {
                valid_sig_count += 1;
            } else {
                println!(
                    "Not valid {:?} {:?}. pk: {:?}",
                    &self.message_hash, &sig.signature, pk,
                )
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
    use ed25519_dalek::SigningKey;

    use crate::{
        hotstuff::crypto::PartialSig,
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
    fn test_verify_qc_with_valid_sigs() {
        let mut sk1 = SigningKey::generate(&mut rand::rngs::OsRng);
        let pk1 = sk1.verifying_key();
        let message_hash = Sha256Hash::from([2u8; 32]);
        let sig1 = sk1.sign(&message_hash);

        let mut sk2 = SigningKey::generate(&mut rand::rngs::OsRng);
        let pk2 = sk2.verifying_key();
        let sig2 = sk2.sign(&message_hash);

        let mut sk3 = SigningKey::generate(&mut rand::rngs::OsRng);
        let pk3 = sk3.verifying_key();
        let sig3 = sk3.sign(&message_hash);

        let sk4 = SigningKey::generate(&mut rand::rngs::OsRng);
        let pk4 = sk4.verifying_key();

        let partials = vec![
            PartialSig::new(pk1, sig1),
            PartialSig::new(pk2, sig2),
            PartialSig::new(pk3, sig3),
        ];

        let qc = QuorumCertificate {
            view_number: 5,
            block_hash: [3u8; 32],
            message_hash,
            partial_sigs: partials.clone(),
        };

        let mut validator_set = HashSet::new();
        validator_set.insert(pk1);
        validator_set.insert(pk2);
        validator_set.insert(pk3);
        validator_set.insert(pk4);

        assert!(qc.verify(&validator_set, 3));
        assert!(!qc.verify(&validator_set, 4)); // not enough
    }

    #[test]
    fn test_verify_qc_rejects_invalid_sigs() {
        let mut sk1 = SigningKey::generate(&mut rand::rngs::OsRng);
        let pk1 = sk1.verifying_key();

        let mut sk2 = SigningKey::generate(&mut rand::rngs::OsRng);
        let pk2 = sk2.verifying_key();

        let wrong_message = Sha256Hash::from([10u8; 32]);

        let sig1 = sk1.sign(&Sha256Hash::from([9u8; 32])); // incorrect message
        let sig2 = sk2.sign(&Sha256Hash::from([8u8; 32])); // incorrect message

        let partials = vec![PartialSig::new(pk1, sig1), PartialSig::new(pk2, sig2)];

        let qc = QuorumCertificate {
            view_number: 2,
            block_hash: [11u8; 32],
            message_hash: wrong_message,
            partial_sigs: partials,
        };

        let mut validator_set = HashSet::new();
        validator_set.insert(pk1);
        validator_set.insert(pk2);

        assert!(!qc.verify(&validator_set, 1));
        assert!(!qc.verify(&validator_set, 2));
    }

    #[test]
    fn test_signature_serialization_round_trip() {
        let mut signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let verifying_key = signing_key.verifying_key();
        let message = b"hello consensus";
        let signature = signing_key.sign(message);

        let partialsig = PartialSig::new(verifying_key, signature);

        // Serialize
        let json = serde_json::to_string(&partialsig).expect("Failed to serialize signature");

        // Deserialize
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
