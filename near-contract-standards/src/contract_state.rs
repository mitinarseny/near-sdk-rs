use std::collections::BTreeMap;

use near_sdk::{near, serde::Serialize, serde_json, ContractCode};

#[near(serializers=[json])]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractState<T> {
    pub code: ContractCode,
    pub state: ExtraState<T>,
}

#[near(serializers=[json])]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraState<T> {
    #[serde(flatten)]
    pub value: T,
    /// Extra information that can be used by extended implementations such as
    /// [mintless tokens](https://github.com/ton-blockchain/mintless-jetton-contract).
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl<T> ExtraState<T> {
    pub const fn new(value: T) -> Self {
        Self { value, extra: BTreeMap::new() }
    }

    pub fn with<V>(mut self, key: impl Into<String>, value: V) -> Self
    where
        V: Serialize,
    {
        self.extra
            .insert(key.into(), serde_json::to_value(value).unwrap_or_else(|_| unreachable!()));
        self
    }
}

impl<T> From<T> for ExtraState<T> {
    fn from(value: T) -> Self {
        Self { value, extra: BTreeMap::new() }
    }
}
