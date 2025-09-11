//! Ethereum block executor.

use super::{
    dao_fork, eip6110,
    receipt_builder::{AlloyReceiptBuilder, ReceiptBuilder, ReceiptBuilderCtx},
    spec::{EthExecutorSpec, EthSpec},
    EthEvm, EthEvmFactory,
};
use crate::{
    block::{
        state_changes::{balance_increment_state, post_block_balance_increments},
        BlockExecutionError, BlockExecutionResult, BlockExecutor, BlockExecutorFactory,
        BlockExecutorFor, BlockValidationError, CommitChanges, ExecutableTx, OnStateHook,
        StateChangePostBlockSource, StateChangeSource, SystemCaller,
    },
    Evm, EvmFactory, FromRecoveredTx, FromTxWithEncoded, MultiDatabase,
};
use alloc::{borrow::Cow, boxed::Box, vec::Vec};
use alloy_consensus::{Header, Transaction, TxReceipt};
use alloy_eips::{eip4895::Withdrawals, eip7685::Requests, Encodable2718};
use alloy_hardforks::EthereumHardfork;
use alloy_primitives::{Log, B256};
use revm::{
    context::{result::ExecutionResult, TxEnv},
    context_interface::result::ResultAndState,
    database::State, database_interface::MultiChainDatabaseCommit,
    inspector::NoOpInspector,
    primitives::{ChainAddress, HashMap, StateChanges}, Inspector,
};

/// Context for Ethereum block execution.
#[derive(Debug, Clone)]
pub struct EthBlockExecutionCtx<'a> {
    /// Parent block hash.
    pub parent_hash: B256,
    /// Parent beacon block root.
    pub parent_beacon_block_root: Option<B256>,
    /// Block ommers
    pub ommers: &'a [Header],
    /// Block withdrawals.
    pub withdrawals: Option<Cow<'a, Withdrawals>>,
}

/// Block executor for Ethereum.
#[derive(Debug)]
pub struct EthBlockExecutor<'a, Evm, Spec, R: ReceiptBuilder> {
    /// Reference to the specification object.
    spec: Spec,

    /// Context for block execution.
    pub ctx: HashMap<u64, EthBlockExecutionCtx<'a>>,
    /// Inner EVM.
    evm: Evm,
    /// Utility to call system smart contracts.
    system_caller: SystemCaller<Spec>,
    /// Receipt builder.
    receipt_builder: R,

    /// Receipts of executed transactions.
    receipts: Vec<R::Receipt>,
    /// Total gas used by transactions in this block.
    gas_used: u64,
    /// State changes from executed transactions.
    state_changes: Vec<StateChanges>,
    /// Gas used per chain.
    gas_used_per_chain: HashMap<u64, u64>,
}

impl<'a, Evm, Spec, R> EthBlockExecutor<'a, Evm, Spec, R>
where
    Spec: Clone,
    R: ReceiptBuilder,
{
    /// Creates a new [`EthBlockExecutor`]
    pub fn new(evm: Evm, ctx: HashMap<u64, EthBlockExecutionCtx<'a>>, spec: Spec, receipt_builder: R) -> Self {
        Self {
            evm,
            ctx,
            receipts: Vec::new(),
            gas_used: 0,
            state_changes: Vec::new(),
            gas_used_per_chain: HashMap::new(),
            system_caller: SystemCaller::new(spec.clone()),
            spec,
            receipt_builder,
        }
    }
}

impl<'db, DB, E, Spec, R> BlockExecutor for EthBlockExecutor<'_, E, Spec, R>
where
    DB: MultiDatabase + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    >,
    Spec: EthExecutorSpec,
    R: ReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt<Log = Log>>,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = E;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        // Set state clear flag if the block is after the Spurious Dragon hardfork.
        let state_clear_flag =
            self.spec.is_spurious_dragon_active_at_block(self.evm.block().number.saturating_to());
        self.evm.db_mut().set_state_clear_flag(state_clear_flag);

        for (&chain_id, ctx) in self.ctx.iter() {
            self.system_caller.apply_blockhashes_contract_call(ctx.parent_hash, &mut self.evm, chain_id)?;
            self.system_caller
                .apply_beacon_root_contract_call(ctx.parent_beacon_block_root, &mut self.evm, chain_id)?;
        }

        Ok(())
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&ExecutionResult<<Self::Evm as Evm>::HaltReason>) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        // The sum of the transaction's gas limit, Tg, and the gas utilized in this block prior,
        // must be no greater than the block's gasLimit.
        let block_available_gas = self.evm.block().gas_limit - self.gas_used;

        let gas_limit = tx.tx().gas_limit();
        let tx_hash = tx.tx().trie_hash();
        
        if gas_limit > block_available_gas {
            return Err(BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit: gas_limit,
                block_available_gas,
            }
            .into());
        }

        // Execute transaction.
        let ResultAndState { result, state } = self
            .evm
            .transact(tx)
            .map_err(|err| BlockExecutionError::evm(err, tx_hash))?;

        if !f(&result).should_commit() {
            return Ok(None);
        }

        if !result.is_success() {
            println!("result: {:?}", result);
        }

        self.system_caller.on_state(StateChangeSource::Transaction(self.receipts.len()), &state);

        let gas_used = result.gas_used();

        // Track gas used per chain
        for (chain_id, chain_gas) in result.gas_used_per_chain() {
            *self.gas_used_per_chain.entry(chain_id).or_default() += chain_gas;
        }

        self.state_changes.push(result.state_changes());

        // append gas used
        self.gas_used += gas_used;

        // Push transaction changeset and calculate header bloom filter for receipt.
        self.receipts.push(self.receipt_builder.build_receipt(ReceiptBuilderCtx {
            tx: tx.tx(),
            evm: &self.evm,
            result,
            state: &state,
            cumulative_gas_used: self.gas_used,
        }));

        // Commit the state changes.
        self.evm.db_mut().commit_multi(state);

        Ok(Some(gas_used))
    }

    fn finish(
        mut self,
    ) -> Result<(Self::Evm, BlockExecutionResult<R::Receipt>), BlockExecutionError> {
        let requests = if self
            .spec
            .is_prague_active_at_timestamp(self.evm.block().timestamp.saturating_to())
        {
            // Collect all EIP-6110 deposits
            let deposit_requests =
                eip6110::parse_deposits_from_receipts(&self.spec, &self.receipts)?;

            let mut requests = Requests::default();

            if !deposit_requests.is_empty() {
                requests.push_request_with_type(eip6110::DEPOSIT_REQUEST_TYPE, deposit_requests);
            }

            requests.extend(self.system_caller.apply_post_execution_changes(&mut self.evm)?);
            requests
        } else {
            Requests::default()
        };

        let chain_id = self.evm.chain_id();
        let mut balance_increments = post_block_balance_increments(
            &self.spec,
            self.evm.block(),
            self.ctx.get(&chain_id).unwrap().ommers,
            self.ctx.get(&chain_id).unwrap().withdrawals.as_deref(),
        );

        // Irregular state change at Ethereum DAO hardfork
        if self
            .spec
            .ethereum_fork_activation(EthereumHardfork::Dao)
            .transitions_at_block(self.evm.block().number.saturating_to())
        {
            // drain balances from hardcoded addresses.
            let drained_balance: u128 = self
                .evm
                .db_mut()
                .drain_balances(dao_fork::DAO_HARDFORK_ACCOUNTS.iter().map(|addr| ChainAddress::new(chain_id, *addr)))
                .map_err(|_| BlockValidationError::IncrementBalanceFailed)?
                .into_iter()
                .sum();

            // return balance to DAO beneficiary.
            *balance_increments.entry(dao_fork::DAO_HARDFORK_BENEFICIARY).or_default() +=
                drained_balance;
        }
        // increment balances
        self.evm
            .db_mut()
            .increment_balances(balance_increments.iter().map(|(addr, balance)| (ChainAddress::new(chain_id, *addr), *balance)))
            .map_err(|_| BlockValidationError::IncrementBalanceFailed)?;

        // call state hook with changes due to balance increments.
        self.system_caller.try_on_state_with(|| {
            balance_increment_state(&balance_increments, self.evm.db_mut(), chain_id).map(|state| {
                (
                    StateChangeSource::PostBlock(StateChangePostBlockSource::BalanceIncrements),
                    Cow::Owned(state),
                )
            })
        })?;

        Ok((
            self.evm,
            BlockExecutionResult {
                receipts: self.receipts,
                requests,
                gas_used: self.gas_used,
                state_changes: self.state_changes,
                gas_used_per_chain: self.gas_used_per_chain,
            },
        ))
    }

    fn set_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
        self.system_caller.with_state_hook(hook);
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        &mut self.evm
    }

    fn evm(&self) -> &Self::Evm {
        &self.evm
    }
}

/// Ethereum block executor factory.
#[derive(Debug, Clone, Default, Copy)]
pub struct EthBlockExecutorFactory<R = AlloyReceiptBuilder, Spec = EthSpec> {
    /// Receipt builder.
    receipt_builder: R,
    /// Chain specification.
    spec: Spec,
    /// EVM factory.
    evm_factory: EthEvmFactory,
}

impl<R, Spec> EthBlockExecutorFactory<R, Spec> {
    /// Creates a new [`EthBlockExecutorFactory`] with the given spec and [`ReceiptBuilder`].
    pub const fn new(receipt_builder: R, spec: Spec) -> Self {
        Self { receipt_builder, spec, evm_factory: EthEvmFactory }
    }

    /// Exposes the receipt builder.
    pub const fn receipt_builder(&self) -> &R {
        &self.receipt_builder
    }

    /// Exposes the chain specification.
    pub const fn spec(&self) -> &Spec {
        &self.spec
    }

    /// Exposes the EVM factory.
    pub const fn evm_factory(&self) -> &EthEvmFactory {
        &self.evm_factory
    }
}

// Generic implementation
impl<R, Spec> BlockExecutorFactory for EthBlockExecutorFactory<R, Spec>
where
    R: ReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt<Log = Log>>,
    Spec: EthExecutorSpec,
    Self: 'static,
    // Add explicit bounds to help the compiler understand that TxEnv satisfies the requirements
    TxEnv: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
{
    type EvmFactory = EthEvmFactory;
    type ExecutionCtx<'a> = EthBlockExecutionCtx<'a>;
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Executor<'a, DB, I> = EthBlockExecutor<'a, EthEvm<&'a mut State<DB>, NoOpInspector>, &'a Spec, &'a R>
    where
        Self: 'a,
        DB: MultiDatabase + 'a;

    fn evm_factory(&self) -> &Self::EvmFactory {
        &self.evm_factory
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: <Self::EvmFactory as EvmFactory<&'a mut State<DB>>>::Evm<I>,
        ctx: HashMap<u64, Self::ExecutionCtx<'a>>,
    ) -> Self::Executor<'a, DB, I>
    where
        Self::EvmFactory: EvmFactory<&'a mut State<DB>>,
        DB: MultiDatabase + 'a,
    {
        // SAFETY: We know EthEvmFactory always returns EthEvm<&'a mut State<DB>, NoOpInspector>
        // This cast is necessary due to Rust's type system limitations with associated types
        let concrete_evm: EthEvm<&'a mut State<DB>, NoOpInspector> = unsafe {
            std::ptr::read(&evm as *const _ as *const _)
        };
        std::mem::forget(evm); // Prevent double drop
        EthBlockExecutor::new(concrete_evm, ctx, &self.spec, &self.receipt_builder)
    }
}
