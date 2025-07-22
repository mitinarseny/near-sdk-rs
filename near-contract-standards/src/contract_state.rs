use near_sdk::{near, ContractCode};

#[near(serializers=[borsh, json])]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractState<T> {
    pub code: ContractCode,
    pub state: T,
}
