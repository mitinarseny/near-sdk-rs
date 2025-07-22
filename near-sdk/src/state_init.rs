use std::{
    borrow::Cow,
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};

use borsh::{io, BorshDeserialize, BorshSerialize};
use near_account_id::AccountId;
use near_sdk_macros::near;
use near_token::NearToken;

use crate::{env, CryptoHash, StorageUsage};

/// Initialization state for non-existing contract
#[near(inside_nearsdk, serializers=[borsh, json])]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateInit {
    /// Code to deploy
    pub code: ContractCode,
    /// Optional key/value pairs to populate to storage on first initialization
    // TODO: serde
    pub data: ContractStorage,
    // TODO: serde vec?
    // pub data: Option<Cow<'a, [u8]>>,
}

impl StateInit {
    /// Create new [`StateInit`] with given code and no data
    #[inline]
    pub fn code(code: impl Into<ContractCode>) -> Self {
        Self { code: code.into(), data: ContractStorage::new() }
    }

    /// Set given data
    #[inline]
    pub fn data(mut self, data: ContractStorage) -> Self {
        self.data = data;
        self
    }

    /// Derives [`AccountId`] deterministically, according to NEP-616.
    ///
    /// We reuse existing implicit eth addresses and add custom prefix to
    /// second prehash to ensure we avoid collisions between secp256k1
    /// public keys and [`StateInit`] borsh representation.
    /// So, the final schema looks like:  
    /// `"0x" .. hex(keccak256("state_init" .. keccak256(state_init))[12..32])`
    // TODO: or separate account-id scheme?
    // TODO: this can be cheaper for transfers, since transfers to implicit eth
    // cost more and there is no way to tell the difference based on account_id
    #[inline]
    pub fn derive_account_id(&self) -> AccountId {
        self.lazy_serialized().derive_account_id()
    }

    #[inline]
    pub const fn lazy(self) -> LazyStateInit {
        LazyStateInit(LazyStateInitInner::StateInit(self))
    }

    pub fn lazy_serialized(&self) -> LazyStateInit {
        LazyStateInit(LazyStateInitInner::Serialized(
            borsh::to_vec(self).unwrap_or_else(|_| unreachable!()),
        ))
    }

    // TODO
    // /// Set data to given `value` serialized to [`borsh`]
    // #[inline]
    // pub fn with_data_entry_borsh<V>(self, key: K, value: V) -> Self
    // where
    //     V: BorshSerialize,
    // {
    //     self.with_data_entry(
    //         value.map(|v| borsh::to_vec(&v).unwrap_or_else(|_| unreachable!())).map(Cow::Owned),
    //     )
    // }

    /// TODO: docs
    #[inline]
    pub fn storage_cost(&self) -> NearToken {
        self.storage_usage()
            .and_then(|s| env::storage_byte_cost().checked_mul(s.into()))
            .unwrap_or_else(|| env::panic_str("too big"))
    }

    /// See NEP-591: https://github.com/near/NEPs/blob/master/neps/nep-0591.md#costs
    pub(crate) fn storage_usage(&self) -> Option<StorageUsage> {
        self.code
            .storage_usage()
            .checked_add(self.data.storage_usage()?)?
            // `num_bytes_account` is required for every account on creation:
            // https://github.com/near/nearcore/blob/685f92e3b9efafc966c9dafcb7815f172d4bb557/runtime/runtime/src/actions.rs#L468
            .checked_add(env::storage_num_bytes_account())
    }
}

/// Code to deploy for non-existing contract
#[near(inside_nearsdk, serializers=[borsh, json])]
#[serde(tag = "location", content = "data", rename_all = "snake_case")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractCode {
    /// Reference global contract's code by its hash
    // TODO: serde serialization?
    CodeHash(CryptoHash),

    /// Reference global contract's code by its [`AccountId`]
    AccountId(AccountId),
}

impl ContractCode {
    pub(crate) fn storage_usage(&self) -> StorageUsage {
        // Global contract identifier, see NEP-591:
        // https://github.com/near/NEPs/blob/master/neps/nep-0591.md#costs
        // Here is nearcore implementation:
        // https://github.com/near/nearcore/blob/685f92e3b9efafc966c9dafcb7815f172d4bb557/core/primitives-core/src/account.rs#L123-L128
        match self {
            Self::CodeHash(hash) => hash.len() as StorageUsage,
            Self::AccountId(account_id) => account_id.len() as StorageUsage,
        }
    }
}

impl From<CryptoHash> for ContractCode {
    #[inline]
    fn from(hash: CryptoHash) -> Self {
        Self::CodeHash(hash)
    }
}

impl From<AccountId> for ContractCode {
    #[inline]
    fn from(account_id: AccountId) -> Self {
        Self::AccountId(account_id)
    }
}

#[near(inside_nearsdk, serializers=[borsh, json])]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[repr(transparent)]
// TODO: Cow<'a, [u8]>?
// TODO: serde
pub struct ContractStorage(pub BTreeMap<Vec<u8>, Vec<u8>>);

impl ContractStorage {
    #[inline]
    pub const fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn borsh<K, V>(mut self, key: &K, value: &V) -> Self
    where
        K: BorshSerialize,
        V: BorshSerialize,
    {
        self.0.insert(
            borsh::to_vec(key).unwrap_or_else(|_| unreachable!()),
            borsh::to_vec(value).unwrap_or_else(|_| unreachable!()),
        );
        self
    }

    pub(crate) fn storage_usage(&self) -> Option<StorageUsage> {
        let num_extra_bytes_record = env::storage_num_extra_bytes_record();
        self.iter().try_fold(0u64, |storage_usage, (key, value)| {
            // key.len() + value.len() + num_extra_bytes_record:
            // https://github.com/near/nearcore/blob/1c2903faeb47fdaf40d5d140cec78aa9bab018ae/runtime/near-vm-runner/src/logic/logic.rs#L3311-L3346
            storage_usage
                .checked_add(key.len().try_into().ok()?)?
                .checked_add(value.len().try_into().ok()?)?
                .checked_add(num_extra_bytes_record)
        })
    }
}

impl Deref for ContractStorage {
    type Target = BTreeMap<Vec<u8>, Vec<u8>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ContractStorage {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[near(inside_nearsdk, serializers=[borsh, json])]
#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(transparent)]
pub struct LazyStateInit(LazyStateInitInner);

impl From<StateInit> for LazyStateInit {
    fn from(state_init: StateInit) -> Self {
        state_init.lazy()
    }
}

#[near(inside_nearsdk, serializers=[json])]
#[derive(Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
enum LazyStateInitInner {
    StateInit(StateInit),
    // TODO: serde vec
    Serialized(Vec<u8>),
}

impl LazyStateInit {
    /// See [`StateInit::derive_account_id()`]
    #[inline]
    pub fn derive_account_id(&self) -> AccountId {
        let hash = env::keccak256_array(
            &[b"state_init".as_slice(), &env::keccak256_array(&self.serialize())].concat(),
        );

        format!("0x{}", hex::encode(&hash[12..32])).parse().unwrap_or_else(|_| unreachable!())
    }

    pub fn serialize(&self) -> Cow<'_, [u8]> {
        match &self.0 {
            LazyStateInitInner::StateInit(state_init) => {
                Cow::Owned(borsh::to_vec(state_init).unwrap_or_else(|_| unreachable!()))
            }
            LazyStateInitInner::Serialized(data) => Cow::Borrowed(data),
        }
    }

    #[inline]
    pub fn into_state_init(self) -> io::Result<StateInit> {
        match self.0 {
            LazyStateInitInner::StateInit(state_init) => Ok(state_init),
            LazyStateInitInner::Serialized(data) => borsh::from_slice(&data),
        }
    }
}

impl BorshSerialize for LazyStateInitInner {
    fn serialize<W: io::Write>(&self, writer: &mut W) -> io::Result<()> {
        match self {
            Self::StateInit(state_init) => BorshSerialize::serialize(state_init, writer),
            Self::Serialized(data) => writer.write_all(data),
        }
    }
}

impl BorshDeserialize for LazyStateInitInner {
    fn deserialize_reader<R: io::Read>(reader: &mut R) -> io::Result<Self> {
        BorshDeserialize::deserialize_reader(reader).map(Self::StateInit)
    }
}
