use std::{fmt, ops::Deref};

use ed25519::{Signature, signature::SignerMut};
use ed25519_dalek::{SigningKey, VerifyingKey};
use hex::{FromHex, encode as hex_encode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    hotstuff::utils,
    state::{
        asset::AssetId,
        order::{OrderDirection, OrderId, OrderType},
        spot_clearinghouse::MarketId,
        state::{ExecError, Nonce},
    },
};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum TransactionStatus {
    Executed,
    Rejected(String),
    Error(ExecError),
    Pending,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum UnsignedTransaction {
    Transfer(TransferTransaction),
    Order(OrderTransaction),
    CancelOrder(CancelOrderTransaction),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TransferTransaction {
    pub from: PublicKeyHash,
    pub to: PublicKeyHash,
    pub amount: u128,
    pub asset_id: AssetId,
    pub status: TransactionStatus,

    pub nonce: Nonce,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OrderTransaction {
    pub from: PublicKeyHash,
    pub market_id: MarketId,
    pub direction: OrderDirection,
    pub order_type: OrderType,
    pub status: TransactionStatus,

    pub nonce: Nonce,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CancelOrderTransaction {
    pub from: PublicKeyHash,
    pub market_id: MarketId,
    pub order_id: OrderId,
    pub status: TransactionStatus,

    pub nonce: Nonce,
}

pub type Sha256Hash = [u8; 32];
pub type PublicKeyHash = Sha256Hash;

impl UnsignedTransaction {
    pub fn hash(&self) -> Sha256Hash {
        // probably want to implement my own encoding and hashing
        let encoded = bincode::serialize(&self).unwrap();
        Sha256::digest(&encoded).into()
    }

    pub fn sign(self, signing_key: &mut SigningKey) -> SignedTransaction {
        let transaction_hash = self.hash();
        let signature = signing_key.sign(&transaction_hash);
        let signature = SignatureString(utils::sig_to_string(&signature));

        SignedTransaction {
            tx: self,
            signature,
            hash: transaction_hash,
        }
    }
}

impl PartialEq for UnsignedTransaction {
    fn eq(&self, other: &Self) -> bool {
        self.hash() == other.hash()
    }
}

impl Eq for UnsignedTransaction {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SignedTransaction {
    pub tx: UnsignedTransaction,
    pub signature: SignatureString,
    pub hash: Sha256Hash,
}

impl SignedTransaction {
    pub fn verify_sender(&self) -> bool {
        match &self.tx {
            UnsignedTransaction::Transfer(transaction) => {
                let public_key = PublicKeyString::from_bytes(transaction.from).as_public_key();
                let tx_hash = self.hash;
                let signature = utils::string_to_sig(self.signature.as_str())
                    .expect("Conversion from string to signature failed");
                public_key.verify_strict(&tx_hash, &signature).is_ok()
            }

            UnsignedTransaction::Order(transaction) => {
                let public_key = PublicKeyString::from_bytes(transaction.from).as_public_key();
                let tx_hash = self.hash();
                let signature = utils::string_to_sig(self.signature.as_str())
                    .expect("Conversion from string to signature failed");
                public_key.verify_strict(&tx_hash, &signature).is_ok()
            }

            UnsignedTransaction::CancelOrder(transaction) => {
                let public_key = PublicKeyString::from_bytes(transaction.from).as_public_key();
                let tx_hash = self.hash();
                let signature = utils::string_to_sig(self.signature.as_str())
                    .expect("Conversion from string to signature failed");
                public_key.verify_strict(&tx_hash, &signature).is_ok()
            }
        }
    }

    pub fn get_from_account(&self) -> PublicKeyHash {
        match &self.tx {
            UnsignedTransaction::Transfer(transaction) => transaction.from,
            UnsignedTransaction::Order(transaction) => transaction.from,
            UnsignedTransaction::CancelOrder(transaction) => transaction.from,
        }
    }

    pub fn get_nonce(&self) -> Nonce {
        match &self.tx {
            UnsignedTransaction::Transfer(transaction) => transaction.nonce,
            UnsignedTransaction::Order(transaction) => transaction.nonce,
            UnsignedTransaction::CancelOrder(transaction) => transaction.nonce,
        }
    }

    pub fn get_status(&self) -> TransactionStatus {
        match &self.tx {
            UnsignedTransaction::Transfer(transaction) => transaction.status.clone(),
            UnsignedTransaction::Order(transaction) => transaction.status.clone(),
            UnsignedTransaction::CancelOrder(transaction) => transaction.status.clone(),
        }
    }
}

impl Deref for SignedTransaction {
    type Target = UnsignedTransaction;

    fn deref(&self) -> &Self::Target {
        &self.tx
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SignatureString(String);

impl SignatureString {
    pub fn new(s: String) -> Result<Self, &'static str> {
        let bytes = <[u8; 64]>::from_hex(&s).map_err(|_| "Invalid hex")?;
        Signature::from_bytes(&bytes);
        Ok(SignatureString(s))
    }

    pub fn as_signature(&self) -> Signature {
        let bytes = <[u8; 64]>::from_hex(&self.0).unwrap();
        Signature::from_bytes(&bytes)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for SignatureString {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for SignatureString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SignatureString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct PublicKeyString(pub String);

impl PublicKeyString {
    pub fn from_public_key(pk: &VerifyingKey) -> Self {
        let pk_string = hex_encode(pk.to_bytes());
        PublicKeyString(pk_string)
    }

    pub fn from_string(s: String) -> Result<Self, &'static str> {
        let bytes = <[u8; 32]>::from_hex(&s).map_err(|_| "Invalid hex")?;
        VerifyingKey::from_bytes(&bytes).expect("Invalid hex");
        Ok(PublicKeyString(s))
    }

    pub fn from_bytes(bytes: Sha256Hash) -> Self {
        let pk_string = hex_encode(bytes);
        PublicKeyString(pk_string)
    }

    pub fn as_public_key(&self) -> VerifyingKey {
        let bytes = <[u8; 32]>::from_hex(&self.0).unwrap();
        VerifyingKey::from_bytes(&bytes).expect("Expect to be public key")
    }

    pub fn to_bytes(&self) -> Sha256Hash {
        <[u8; 32]>::from_hex(&self.0).unwrap()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for PublicKeyString {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for PublicKeyString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PublicKeyString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Default for PublicKeyString {
    fn default() -> Self {
        PublicKeyString(
            "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn generate_keypair() -> (SigningKey, VerifyingKey) {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        (signing_key, verifying_key)
    }

    #[test]
    fn test_transfer_transaction_hash_consistency() {
        let (_, vk) = generate_keypair();
        let tx1 = UnsignedTransaction::Transfer(TransferTransaction {
            from: vk.to_bytes(),
            to: PublicKeyString::default().to_bytes(),
            amount: 42,
            asset_id: 0,
            nonce: 0,
            status: TransactionStatus::Pending,
        });

        let tx2 = tx1.clone();
        assert_eq!(
            tx1.hash(),
            tx2.hash(),
            "Hashes should be identical for identical txs"
        );
    }

    #[test]
    fn test_sign_and_verify_transfer_transaction() {
        let (mut sk, vk) = generate_keypair();
        let unsigned = UnsignedTransaction::Transfer(TransferTransaction {
            from: vk.to_bytes(),
            to: PublicKeyString::default().to_bytes(),
            amount: 100,
            asset_id: 0,
            nonce: 0,
            status: TransactionStatus::Pending,
        });

        let signed = unsigned.sign(&mut sk);
        assert!(signed.verify_sender(), "Signature should verify");
    }

    #[test]
    fn test_signature_string_conversion() {
        let (sk, _vk) = generate_keypair();
        let dummy_data = [1u8; 32];
        let sig = sk.sign(&dummy_data);

        let sig_string = utils::sig_to_string(&sig);
        let reconstructed = SignatureString::new(sig_string.clone()).expect("Valid signature hex");

        assert_eq!(sig_string, reconstructed.as_str());
    }

    mod public_key_string_tests {
        use crate::types::transaction::{
            PublicKeyString, SignatureString, TransactionStatus, TransferTransaction,
            UnsignedTransaction, tests::generate_keypair,
        };

        #[test]
        fn test_public_key_string_roundtrip() {
            let (_, vk) = generate_keypair();
            let pk_str = PublicKeyString::from_public_key(&vk);
            let reconstructed_vk = pk_str.as_public_key();

            assert_eq!(vk.to_bytes(), reconstructed_vk.to_bytes());
        }

        #[test]
        fn test_from_bytes_and_to_bytes_roundtrip() {
            // Generate random public key bytes

            let (_, vk) = generate_keypair();
            let bytes: [u8; 32] = vk.to_bytes();

            let pks = PublicKeyString::from_bytes(bytes);
            let roundtrip = pks.to_bytes();

            assert_eq!(
                roundtrip, bytes,
                "to_bytes should return original public key bytes"
            );
        }

        #[test]
        #[should_panic(expected = "Invalid hex")]
        fn test_signature_string_invalid_hex_panics() {
            let invalid_hex = "ZZZ".repeat(22); // clearly invalid
            let _ = SignatureString::new(invalid_hex).unwrap();
        }

        #[test]
        fn test_to_bytes_invalid_hex_panics() {
            let pks = PublicKeyString(
                "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz".into(),
            );
            // unwrap should panic due to invalid hex
            let result = std::panic::catch_unwind(|| {
                let _ = pks.to_bytes();
            });
            assert!(result.is_err(), "Expected to panic on invalid hex");
        }

        #[test]
        #[should_panic(expected = "Invalid hex")]
        fn test_public_key_string_invalid_hex_panics() {
            let invalid_hex = "GGG".repeat(10);
            let _ = PublicKeyString::from_string(invalid_hex).unwrap();
        }

        #[test]
        fn test_from_bytes_creates_valid_string() {
            let (_, vk) = generate_keypair();
            let bytes = vk.to_bytes();

            let pks = PublicKeyString::from_bytes(bytes);
            let s = pks.as_str();

            assert_eq!(
                s.len(),
                64,
                "Hex-encoded string should be 64 characters for 32 bytes"
            );
            assert!(
                s.chars().all(|c| c.is_ascii_hexdigit()),
                "String should be valid hex"
            );
        }

        #[test]
        fn test_unsigned_transaction_equality() {
            let (_, vk) = generate_keypair();
            let tx1 = UnsignedTransaction::Transfer(TransferTransaction {
                from: vk.to_bytes(),
                to: PublicKeyString::default().to_bytes(),
                amount: 123,
                asset_id: 0,
                nonce: 0,
                status: TransactionStatus::Pending,
            });
            let tx2 = tx1.clone();

            assert_eq!(tx1, tx2, "Transactions should be equal based on hash");
        }

        #[test]
        fn test_unsigned_transaction_inequality() {
            let (_, vk) = generate_keypair();
            let tx1 = UnsignedTransaction::Transfer(TransferTransaction {
                from: vk.to_bytes(),
                to: PublicKeyString::default().to_bytes(),
                amount: 123,
                asset_id: 0,
                nonce: 0,
                status: TransactionStatus::Pending,
            });

            let tx2 = UnsignedTransaction::Transfer(TransferTransaction {
                from: vk.to_bytes(),
                to: PublicKeyString::default().to_bytes(),
                amount: 456,
                asset_id: 0,
                nonce: 0,
                status: TransactionStatus::Pending,
            });

            assert_ne!(
                tx1, tx2,
                "Transactions with different amounts should not be equal"
            );
        }
    }

    mod signed_transaction_tests {
        use super::*;

        #[test]
        fn test_signed_transaction_wrong_sender_fails() {
            let (mut sk1, _vk1) = generate_keypair();
            let (_sk2, vk2) = generate_keypair(); // wrong key

            let unsigned = UnsignedTransaction::Transfer(TransferTransaction {
                from: vk2.to_bytes(), // set from wrong key
                to: PublicKeyString::default().to_bytes(),
                amount: 100,
                asset_id: 0,
                nonce: 0,
                status: TransactionStatus::Pending,
            });

            let signed = unsigned.sign(&mut sk1);

            assert!(
                !signed.verify_sender(),
                "Signature should fail to verify with wrong sender"
            );
        }

        #[test]
        fn test_signed_transaction_signature_difference() {
            let (mut sk1, vk1) = generate_keypair();
            let (mut sk2, vk2) = generate_keypair();

            let unsigned1 = UnsignedTransaction::Transfer(TransferTransaction {
                from: vk1.to_bytes(),
                to: PublicKeyString::default().to_bytes(),
                amount: 50,
                asset_id: 0,
                nonce: 0,
                status: TransactionStatus::Pending,
            });

            let unsigned2 = UnsignedTransaction::Transfer(TransferTransaction {
                from: vk2.to_bytes(),
                to: PublicKeyString::default().to_bytes(),
                amount: 50,
                asset_id: 0,
                nonce: 0,
                status: TransactionStatus::Pending,
            });

            let signed1 = unsigned1.sign(&mut sk1);
            let signed2 = unsigned2.sign(&mut sk2);

            assert_ne!(
                signed1.signature, signed2.signature,
                "Signatures should differ for different keys"
            );
        }

        #[test]
        fn test_unsigned_transaction_hash_consistency_after_signing() {
            let (mut sk, vk) = generate_keypair();

            let unsigned = UnsignedTransaction::Transfer(TransferTransaction {
                from: vk.to_bytes(),
                to: PublicKeyString::default().to_bytes(),
                amount: 777,
                asset_id: 0,
                nonce: 0,
                status: TransactionStatus::Pending,
            });

            let hash_before = unsigned.hash();
            let signed = unsigned.sign(&mut sk);
            let hash_after = signed.tx.hash();

            assert_eq!(
                hash_before, hash_after,
                "Hash should remain consistent after signing"
            );
        }
    }
}
