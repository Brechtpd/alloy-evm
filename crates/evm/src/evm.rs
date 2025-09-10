//! Abstraction over EVM.

use crate::{tracing::TxTracer, EvmEnv, EvmError, IntoTxEnv};
use alloy_primitives::{Address, Bytes};
use core::{error::Error, fmt::Debug, hash::Hash};
use revm::{
    context::{result::ExecutionResult, BlockEnv}, 
    context_interface::{
        result::{HaltReasonTr, ResultAndState},
        ContextTr,
    }, 
    database_interface::{MultiChainDatabase, MultiChainDatabaseCommit}, 
    inspector::{JournalExt, NoOpInspector}, 
    primitives::{ChainAddress, HashMap}, 
    Inspector
};

/// Helper trait to bound [`MultiChainDatabase::Error`] with common requirements.
pub trait MultiDatabase: MultiChainDatabase<Error: Error + Send + Sync + 'static> {}
impl<T> MultiDatabase for T where T: MultiChainDatabase<Error: Error + Send + Sync + 'static> {}

/// An instance of an ethereum virtual machine.
///
/// An EVM is commonly initialized with the corresponding block context and state and it's only
/// purpose is to execute transactions.
///
/// Executing a transaction will return the outcome of the transaction.
pub trait Evm {
    /// Database type held by the EVM.
    type DB;
    /// The transaction object that the EVM will execute.
    ///
    /// This type represents the transaction environment that the EVM operates on internally.
    /// Typically this is [`revm::context::TxEnv`], which contains all necessary transaction
    /// data like sender, gas limits, value, and calldata.
    ///
    /// The EVM accepts flexible transaction inputs through the [`IntoTxEnv`] trait. This means
    /// that while the EVM internally works with `Self::Tx` (usually `TxEnv`), users can pass
    /// various transaction formats to [`Evm::transact`], including:
    /// - Direct [`TxEnv`](revm::context::TxEnv) instances
    /// - [`Recovered<T>`](alloy_consensus::transaction::Recovered) where `T` implements
    ///   [`crate::FromRecoveredTx`]
    /// - [`WithEncoded<Recovered<T>>`](alloy_eips::eip2718::WithEncoded) where `T` implements
    ///   [`crate::FromTxWithEncoded`]
    ///
    /// This design allows the EVM to accept recovered consensus transactions seamlessly.
    type Tx: IntoTxEnv<Self::Tx>;
    /// Error type returned by EVM. Contains either errors related to invalid transactions or
    /// internal irrecoverable execution errors.
    type Error: EvmError;
    /// Halt reason. Enum over all possible reasons for halting the execution. When execution halts,
    /// it means that transaction is valid, however, it's execution was interrupted (e.g because of
    /// running out of gas or overflowing stack).
    type HaltReason: HaltReasonTr + Send + Sync + 'static;
    /// Identifier of the EVM specification. EVM is expected to use this identifier to determine
    /// which features are enabled.
    type Spec: Debug + Copy + Hash + Eq + Send + Sync + Default + 'static;
    /// Precompiles used by the EVM.
    type Precompiles;
    /// Evm inspector.
    type Inspector;

    /// Reference to all blocks as a HashMap.
    fn blocks(&self) -> &HashMap<u64, BlockEnv>;

    /// Reference to the current chain's [`BlockEnv`].
    fn block(&self) -> &BlockEnv {
        let chain_id = self.chain_id();
        self.blocks()
            .get(&chain_id)
            .or_else(|| self.blocks().get(&0)) // fallback to chain 0
            .expect("No block environment found for chain or fallback chain 0")
    }

    /// Returns the chain ID of the environment.
    fn chain_id(&self) -> u64;

    /// Executes a transaction and returns the outcome.
    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error>;

    /// Same as [`Evm::transact_raw`], but takes any type implementing [`IntoTxEnv`].
    ///
    /// This is the primary method for executing transactions. It accepts flexible input types
    /// that can be converted to the EVM's transaction environment, including:
    /// - [`TxEnv`](revm::context::TxEnv) - Direct transaction environment
    /// - [`Recovered<T>`](alloy_consensus::transaction::Recovered) - Consensus transaction with
    ///   recovered sender
    /// - [`WithEncoded<Recovered<T>>`](alloy_eips::eip2718::WithEncoded) - Transaction with sender
    ///   and encoded bytes
    ///
    /// The conversion happens automatically through the [`IntoTxEnv`] trait.
    fn transact(
        &mut self,
        tx: impl IntoTxEnv<Self::Tx>,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        self.transact_raw(tx.into_tx_env())
    }

    /// Executes a system call.
    ///
    /// Note: this will only keep the target `contract` in the state. This is done because revm is
    /// loading [`BlockEnv::beneficiary`] into state by default, and we need to avoid it by also
    /// covering edge cases when beneficiary is set to the system contract address.
    fn transact_system_call(
        &mut self,
        caller: ChainAddress,
        contract: ChainAddress,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error>;

    /// Returns a mutable reference to the underlying database.
    fn db_mut(&mut self) -> &mut Self::DB;

    /// Executes a transaction and commits the state changes to the underlying database.
    fn transact_commit(
        &mut self,
        tx: impl IntoTxEnv<Self::Tx>,
    ) -> Result<ExecutionResult<Self::HaltReason>, Self::Error>
    where
        Self::DB: MultiChainDatabaseCommit,
    {
        let ResultAndState { result, state } = self.transact(tx)?;
        self.db_mut().commit_multi(state);

        Ok(result)
    }

    /// Consumes the EVM and returns the inner [`EvmEnv`].
    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>)
    where
        Self: Sized;

    /// Consumes the EVM and returns the inner database.
    fn into_db(self) -> Self::DB
    where
        Self: Sized,
    {
        self.finish().0
    }

    /// Consumes the EVM and returns the inner [`EvmEnv`].
    fn into_env(self) -> EvmEnv<Self::Spec>
    where
        Self: Sized,
    {
        self.finish().1
    }

    /// Determines whether additional transactions should be inspected or not.
    fn should_continue(&mut self, result: &ExecutionResult<Self::HaltReason>) -> bool {
        result.is_success()
    }

    /// Returns mutable reference to the precompiles.
    fn precompiles_mut(&mut self) -> &mut Self::Precompiles;

    /// Returns mutable reference to the inspector.
    fn inspector_mut(&mut self) -> &mut Self::Inspector;
}

/// Consumes the tracer and builds its final state.
///
/// This trait can be implemented by tracers that need to consume their internal state to produce
/// a final output. This is typically used at the end of tracing operations to generate a
/// comprehensive trace result.
pub trait IntoTracer {
    /// Final output type of the tracer.
    type Output;

    /// Consumes the tracer and returns its final state.
    fn finish(self) -> Self::Output;
}

/// Factory for creating EVM instances.
pub trait EvmFactory<DB = (), R = ()> {
    /// The EVM type produced by this factory.
    type Evm<I>: Evm
    where
        I: Inspector<ContextTr<DB = DB>, JournalState: JournalExt>;

    /// Additional data required by the `BlockExecutor`.
    type Context<'a>;

    /// Supported hardforks.
    type Hardforks;

    /// Associated transaction type.
    type Transaction;

    /// Create EVM instance.
    fn create_evm<I>(&self, db: DB, env: EvmEnv<Self::Hardforks>, inspector: I) -> Self::Evm<I>
    where
        I: Inspector<ContextTr<DB = DB>, JournalState: JournalExt>;

    /// Create EVM instance with TX tracer.
    fn create_evm_with_tracer<T>(&self, db: DB, env: EvmEnv<Self::Hardforks>, tracer: T) -> TxTracer<T>
    where
        T: Inspector<ContextTr<DB = DB>, JournalState: JournalExt>,
    {
        TxTracer::new(self.create_evm(db, env, tracer))
    }

    /// Create EVM instance with standard environment types.
    fn create_evm_env(&self, db: DB, env: EvmEnv<Self::Hardforks>) -> Self::Evm<NoOpInspector> {
        self.create_evm(db, env, NoOpInspector {})
    }
}