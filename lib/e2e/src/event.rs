use std::fmt::Debug;

use alloy::{rpc::types::eth::TransactionReceipt, sol_types::SolEvent};

use crate::Receipt;

/// Extension trait for asserting an event gets emitted.
pub trait Ext<E> {
    /// Asserts the contract emitted the `expected` event.
    fn emits(&self, expected: E) -> bool;
}

impl<E> Ext<E> for TransactionReceipt
where
    E: SolEvent,
    E: PartialEq,
{
    fn emits(&self, expected: E) -> bool {
        // Extract all events that are the expected type.
        self.inner
            .logs()
            .iter()
            .filter_map(|log| log.log_decode().ok())
            .map(|log| log.inner.data)
            .any(|event| expected == event)
    }
}

impl<E: Debug> Ext<E> for Receipt
where
    E: SolEvent,
    E: PartialEq,
{
    fn emits(&self, expected: E) -> bool {
        self.inner.emits(expected)
    }
}
