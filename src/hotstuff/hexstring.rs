use ed25519_dalek::{Signature, VerifyingKey};
use hex;
use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serializer};

pub fn serialize_signature<S: Serializer>(
    sig: &Signature,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let hex_str = hex::encode(sig.to_bytes());
    serializer.serialize_str(&hex_str)
}

pub fn serialize_verifying_key<S: Serializer>(
    key: &VerifyingKey,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let hex_str = hex::encode(key.as_bytes());
    serializer.serialize_str(&hex_str)
}

pub fn deserialize_signature<'de, D>(deserializer: D) -> Result<Signature, D::Error>
where
    D: Deserializer<'de>,
{
    let hex_str = String::deserialize(deserializer)?;
    let bytes = hex::decode(hex_str).map_err(D::Error::custom)?;
    let array: [u8; 64] = bytes
        .try_into()
        .map_err(|_| D::Error::custom("Invalid sig length"))?;
    Ok(Signature::from_bytes(&array))
}

pub fn deserialize_verifying_key<'de, D>(deserializer: D) -> Result<VerifyingKey, D::Error>
where
    D: Deserializer<'de>,
{
    let hex_str = String::deserialize(deserializer)?;
    let bytes = hex::decode(hex_str).map_err(D::Error::custom)?;
    let array: [u8; 32] = bytes
        .try_into()
        .map_err(|_| D::Error::custom("Invalid key length"))?;
    VerifyingKey::from_bytes(&array).map_err(D::Error::custom)
}
