//! storage helpers
use super::vp::ADDRESS;
use crate::types::storage::{Key, KeySeg};

const QUEUE_STORAGE_KEY: &str = "queue";

/// Get the key corresponding to @EthBridge/queue
pub fn queue_key() -> Key {
    Key::from(ADDRESS.to_db_key())
        .push(&QUEUE_STORAGE_KEY.to_owned())
        .expect("Cannot obtain a storage key")
}

// TODO: This module should live with the EthSentinel VP rather than
// the EthBridge VP, as it is the EthSentinel VP which guards it
/// Keys to do with the /eth_msgs storage subspace
pub mod eth_msgs {
    use crate::types::hash::Hash;
    use crate::types::storage::{DbKeySeg, Key};

    const TOP_LEVEL_KEY: &str = "eth_msgs";

    /// Get the key corresponding to the /eth_msgs storage subspace
    pub fn top_level_key() -> Key {
        Key::from(DbKeySeg::StringSeg(TOP_LEVEL_KEY.to_owned()))
    }

    const BODY_KEY: &str = "body";
    const SEEN_KEY: &str = "seen";
    const SEEN_BY_KEY: &str = "seen_by";
    const VOTING_POWER_KEY: &str = "voting_power";

    /// Handle for the storage space for a specific [`EthMsg`]
    pub struct EthMsgKeys {
        /// The prefix under which the keys for the EthMsg are stored
        pub prefix: Key,
    }

    impl EthMsgKeys {
        /// Create a new [`EthMsgKeys`] based on the hash
        pub fn new(msg_hash: Hash) -> Self {
            let hex = format!("{}", msg_hash);
            let prefix = top_level_key().push(&hex).expect(
                "should always be able to construct prefix, given hex-encoded \
                 hash",
            );
            Self { prefix }
        }

        /// Get the `body` key for the given EthMsg
        pub fn body(&self) -> Key {
            self.prefix.push(&BODY_KEY.to_owned()).unwrap()
        }

        /// Get the `seen` key for the given EthMsg
        pub fn seen(&self) -> Key {
            self.prefix.push(&SEEN_KEY.to_owned()).unwrap()
        }

        /// Get the `seen_by` key for the given EthMsg
        pub fn seen_by(&self) -> Key {
            self.prefix.push(&SEEN_BY_KEY.to_owned()).unwrap()
        }

        /// Get the `voting_power` key for the given EthMsg
        pub fn voting_power(&self) -> Key {
            self.prefix.push(&VOTING_POWER_KEY.to_owned()).unwrap()
        }
    }

    #[cfg(test)]
    mod test {
        use super::*;

        fn arbitrary_hash_with_hex() -> (Hash, String) {
            (Hash::sha256(b"arbitrary"), "87288D68ED71BF8FA35E531A1E56F3B3705FA0EEA54A2AA689B41694A8F83F5B".to_owned())
        }

        #[test]
        fn test_top_level_key() {
            assert!(
                matches!(&top_level_key().segments[..], [DbKeySeg::StringSeg(s)] if s == TOP_LEVEL_KEY)
            )
        }

        #[test]
        fn test_eth_msgs_keys_all_keys() {
            let (msg_hash, hex) = arbitrary_hash_with_hex();
            let keys = EthMsgKeys::new(msg_hash);
            let prefix = vec![
                DbKeySeg::StringSeg(TOP_LEVEL_KEY.to_owned()),
                DbKeySeg::StringSeg(hex),
            ];
            let body_key = keys.body();
            assert_eq!(body_key.segments[..2], prefix[..]);
            assert_eq!(
                body_key.segments[2],
                DbKeySeg::StringSeg(BODY_KEY.to_owned())
            );

            let seen_key = keys.seen();
            assert_eq!(seen_key.segments[..2], prefix[..]);
            assert_eq!(
                seen_key.segments[2],
                DbKeySeg::StringSeg(SEEN_KEY.to_owned())
            );

            let seen_by_key = keys.seen_by();
            assert_eq!(seen_by_key.segments[..2], prefix[..]);
            assert_eq!(
                seen_by_key.segments[2],
                DbKeySeg::StringSeg(SEEN_BY_KEY.to_owned())
            );

            let voting_power_key = keys.voting_power();
            assert_eq!(voting_power_key.segments[..2], prefix[..]);
            assert_eq!(
                voting_power_key.segments[2],
                DbKeySeg::StringSeg(VOTING_POWER_KEY.to_owned())
            );
        }
    }
}
