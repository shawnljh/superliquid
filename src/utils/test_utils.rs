#[cfg(test)]
pub mod test_helpers {

    use crate::types::transaction::{
        PublicKeyString, SignedTransaction, TransactionStatus, TransferTransaction,
        UnsignedTransaction,
    };
    use hex::FromHex;

    use ed25519_dalek::SigningKey;

    pub fn get_alice_pk_str() -> PublicKeyString {
        let sk_hex = "06e016c7278de39eb9e4e3d2088316bf8d4a2b4e73cdf5e651f1c89c7d206bf5";
        let sk_bytes = <[u8; 32]>::from_hex(sk_hex).expect("Invalid hex");
        let sk = SigningKey::from_bytes(&sk_bytes);
        PublicKeyString::from_public_key(&sk.verifying_key())
    }

    pub fn get_alice_sk() -> SigningKey {
        let sk_hex = "000016c7278de39eb9e4e3d2088316bf8d4a2b4e73cdf5e651f1c89c7d206bf5";
        let sk_bytes = <[u8; 32]>::from_hex(sk_hex).expect("Invalid hex");
        SigningKey::from_bytes(&sk_bytes)
    }

    pub fn get_bob_pk_str() -> PublicKeyString {
        let sk_hex = "00001bc6b900f8c76e97c6537370ea5d09538505df1a5859361972f32c8c1760";

        let sk_bytes = <[u8; 32]>::from_hex(sk_hex).expect("Invalid hex");
        let sk = SigningKey::from_bytes(&sk_bytes);
        PublicKeyString::from_public_key(&sk.verifying_key())
    }

    pub fn get_bob_sk() -> SigningKey {
        let sk_hex = "00002bc6b900f8c76e97c6537370ea5d09538505df1a5859361972f32c8c1760";

        let sk_bytes = <[u8; 32]>::from_hex(sk_hex).expect("Invalid hex");
        SigningKey::from_bytes(&sk_bytes)
    }

    pub fn get_carol_pk_str() -> PublicKeyString {
        let sk_hex = "00003bc6b900f8c76e97c6537370ea5d09538505df1a5859361972f32c8c1760";

        let sk_bytes = <[u8; 32]>::from_hex(sk_hex).expect("Invalid hex");
        let sk = SigningKey::from_bytes(&sk_bytes);
        PublicKeyString::from_public_key(&sk.verifying_key())
    }
    pub fn get_carol_sk() -> SigningKey {
        let sk_hex = "00003bc6b900f8c76e97c6537370ea5d09538505df1a5859361972f32c8c1760";

        let sk_bytes = <[u8; 32]>::from_hex(sk_hex).expect("Invalid hex");
        SigningKey::from_bytes(&sk_bytes)
    }

    pub fn make_alice_transaction() -> SignedTransaction {
        let unsigned_txn = UnsignedTransaction::Transfer(TransferTransaction {
            from: get_alice_pk_str().to_bytes(),
            to: get_bob_pk_str().to_bytes(),
            amount: 42,
            asset_id: 0,
            nonce: 0,
            status: TransactionStatus::Pending,
        });
        unsigned_txn.sign(&mut get_alice_sk())
    }
}
