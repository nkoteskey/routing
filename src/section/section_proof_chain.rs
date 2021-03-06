// Copyright 2020 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use std::{collections::HashSet, iter};

/// Chain of section BLS keys where every key is proven (signed) by the previous key, except the
/// first one.
#[derive(Debug, Eq, PartialEq, Clone, Hash, Serialize, Deserialize)]
pub struct SectionProofChain {
    head: bls::PublicKey,
    tail: Vec<Block>,
}

impl SectionProofChain {
    /// Creates new chain consisting of only one block.
    pub fn new(first: bls::PublicKey) -> Self {
        Self {
            head: first,
            tail: Vec::new(),
        }
    }

    /// Pushes a new key into the chain but only if the signature is valid.
    pub(crate) fn push(&mut self, key: bls::PublicKey, signature: bls::Signature) {
        if self.last_key().verify(&signature, &key.to_bytes()[..]) {
            self.tail.push(Block { key, signature })
        } else {
            log_or_panic!(
                log::Level::Error,
                "invalid SectionProofChain block signature"
            )
        }
    }

    /// Pushed a new key into the chain without validating the signature. For testing only.
    #[cfg(any(test, feature = "mock_base"))]
    #[allow(unused)]
    pub fn push_without_validation(&mut self, key: bls::PublicKey, signature: bls::Signature) {
        self.tail.push(Block { key, signature })
    }

    pub(crate) fn first_key(&self) -> &bls::PublicKey {
        &self.head
    }

    pub(crate) fn last_key(&self) -> &bls::PublicKey {
        self.tail
            .last()
            .map(|block| &block.key)
            .unwrap_or(&self.head)
    }

    pub(crate) fn keys(&self) -> impl DoubleEndedIterator<Item = &bls::PublicKey> {
        iter::once(&self.head).chain(self.tail.iter().map(|block| &block.key))
    }

    /// Returns whether this chain contains the given key.
    #[cfg(all(test, feature = "mock"))]
    #[cfg_attr(feature = "mock_base", allow(clippy::trivially_copy_pass_by_ref))]
    pub(crate) fn has_key(&self, key: &bls::PublicKey) -> bool {
        self.keys().any(|existing_key| existing_key == key)
    }

    /// Returns the index of the key in the chain or `None` if not present in the chain.
    #[cfg_attr(feature = "mock_base", allow(clippy::trivially_copy_pass_by_ref))]
    pub(crate) fn index_of(&self, key: &bls::PublicKey) -> Option<u64> {
        self.keys()
            .position(|existing_key| existing_key == key)
            .map(|index| index as u64)
    }

    /// Returns a slice of this chain starting at the given index.
    pub(crate) fn slice_from(&self, first_index: u64) -> Self {
        if first_index == 0 || self.tail.is_empty() {
            return self.clone();
        }

        let head_index = std::cmp::min(first_index as usize, self.tail.len()) - 1;
        let head = self.tail[head_index].key;
        let tail = self.tail[head_index + 1..].to_vec();

        Self { head, tail }
    }

    /// Number of blocks in the chain (including the first block)
    pub(crate) fn len(&self) -> usize {
        1 + self.tail.len()
    }

    /// Index of the last key in the chain.
    pub(crate) fn last_key_index(&self) -> u64 {
        self.tail.len() as u64
    }

    /// Check that all the blocks in the chain except the first one have valid signatures.
    /// The first one cannot be verified and requires matching against already trusted keys. Thus
    /// this function alone cannot be used to determine whether this chain is trusted. Use
    /// `check_trust` for that.
    pub(crate) fn self_verify(&self) -> bool {
        let mut current_key = &self.head;
        for block in &self.tail {
            if !block.verify(current_key) {
                return false;
            }

            current_key = &block.key;
        }
        true
    }

    /// Verify this proof chain against the given trusted keys.
    pub(crate) fn check_trust<'a, I>(&self, trusted_keys: I) -> TrustStatus
    where
        I: IntoIterator<Item = &'a bls::PublicKey>,
    {
        if let Some((index, mut trusted_key)) = self.latest_trusted_key(trusted_keys) {
            for block in &self.tail[index..] {
                if !block.verify(trusted_key) {
                    return TrustStatus::Invalid;
                }

                trusted_key = &block.key;
            }

            TrustStatus::Trusted
        } else if self.self_verify() {
            TrustStatus::Unknown
        } else {
            TrustStatus::Invalid
        }
    }

    // Returns the latest key in this chain that is among the trusted keys, together with its index.
    fn latest_trusted_key<'a, 'b, I>(
        &'a self,
        trusted_keys: I,
    ) -> Option<(usize, &'a bls::PublicKey)>
    where
        I: IntoIterator<Item = &'b bls::PublicKey>,
    {
        let trusted_keys: HashSet<_> = trusted_keys.into_iter().collect();
        let last_index = self.len() - 1;

        self.keys()
            .rev()
            .enumerate()
            .map(|(rev_index, key)| (last_index - rev_index, key))
            .find(|(_, key)| trusted_keys.contains(key))
    }
}

// Result of a message trust check.
#[derive(Debug, Eq, PartialEq)]
pub enum TrustStatus {
    // Proof chain is trusted.
    Trusted,
    // Proof chain is untrusted because one or more blocks in the chain have invalid signatures.
    Invalid,
    // Proof chain is self-validated but its trust cannot be determined because none of the keys
    // in the chain is among the trusted keys.
    Unknown,
}

// Block of the section proof chain. Contains the section BLS public key and is signed by the
// previous block. Note that the first key in the chain is not signed and so is not stored in
// `Block`.
#[derive(Debug, Eq, PartialEq, Clone, Hash, Serialize, Deserialize)]
struct Block {
    key: bls::PublicKey,
    signature: bls::Signature,
}

impl Block {
    #[cfg_attr(feature = "mock_base", allow(clippy::trivially_copy_pass_by_ref))]
    fn verify(&self, public_key: &bls::PublicKey) -> bool {
        public_key.verify(&self.signature, &self.key.to_bytes()[..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        rng::{self, MainRng},
        section::gen_secret_key,
    };

    #[test]
    fn check_trust_trusted() {
        let mut rng = rng::new();
        let chain = gen_chain(&mut rng, 4);

        // If any key in the chain is already trusted, the whole chain is trusted.
        for key in chain.keys() {
            assert_eq!(chain.check_trust(iter::once(key)), TrustStatus::Trusted)
        }
    }

    #[test]
    fn check_trust_invalid() {
        let mut rng = rng::new();
        let mut chain = gen_chain(&mut rng, 2);

        // Add a block with invalid signature to the chain.
        let (_, invalid_secret_key) = gen_keys(&mut rng);
        let (key, signature, secret_key) = gen_block(&mut rng, &invalid_secret_key);
        chain.push_without_validation(key, signature);

        // Add another block with valid signature by the previous block.
        let (key, signature, _) = gen_block(&mut rng, &secret_key);
        chain.push(key, signature);

        // If we only trust the keys up to, but excluding the invalid block, the trust check fails
        // because the rest of the chain contains invalid block.
        for key in chain.keys().take(2) {
            assert_eq!(chain.check_trust(iter::once(key)), TrustStatus::Invalid)
        }

        // But if any key at or after the invalid block is trusted, the rest of the chain is
        // trusted as well.
        for key in chain.keys().skip(2) {
            assert_eq!(chain.check_trust(iter::once(key)), TrustStatus::Trusted)
        }
    }

    #[test]
    fn check_trust_unknown() {
        let mut rng = rng::new();
        let chain = gen_chain(&mut rng, 2);

        // None of the keys in the chain is trusted - the chain might be valid, but its trust status
        // cannot be determined.
        let (trusted_key, _) = gen_keys(&mut rng);

        assert_eq!(
            chain.check_trust(iter::once(&trusted_key)),
            TrustStatus::Unknown
        )
    }

    fn gen_keys(rng: &mut MainRng) -> (bls::PublicKey, bls::SecretKey) {
        let secret_key = gen_secret_key(rng);
        (secret_key.public_key(), secret_key)
    }

    fn gen_block(
        rng: &mut MainRng,
        prev_secret_key: &bls::SecretKey,
    ) -> (bls::PublicKey, bls::Signature, bls::SecretKey) {
        let (public_key, secret_key) = gen_keys(rng);
        let signature = prev_secret_key.sign(&bincode::serialize(&public_key).unwrap());

        (public_key, signature, secret_key)
    }

    fn gen_chain(rng: &mut MainRng, len: usize) -> SectionProofChain {
        let (key, mut current_secret_key) = gen_keys(rng);
        let mut chain = SectionProofChain::new(key);

        for _ in 1..len {
            let (new_public_key, new_signature, new_secret_key) =
                gen_block(rng, &current_secret_key);
            chain.push(new_public_key, new_signature);
            current_secret_key = new_secret_key;
        }

        chain
    }
}
