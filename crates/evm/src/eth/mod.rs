//! Ethereum EVM implementation.

use crate::{env::EvmEnv, evm::EvmFactory, precompiles::PrecompilesMap, Evm, MultiDatabase};
use alloc::vec::Vec;
use alloy_primitives::{Address, Bytes, U256};
use core::{
    fmt::Debug,
    ops::{Deref, DerefMut},
};
use revm::{
    primitives::{MultiChainTxKind as TxKind, HashMap, ChainAddress, hardfork::SpecId},
    context::{BlockEnv, CfgEnv, Evm as RevmEvm, TxEnv},
    context_interface::result::{EVMError, HaltReason, ResultAndState},
    handler::{instructions::EthInstructions, EthFrame, EthPrecompiles, PrecompileProvider},
    inspector::NoOpInspector,
    interpreter::{interpreter::EthInterpreter, InterpreterResult},
    precompile::{PrecompileSpecId, Precompiles},
    Context, ExecuteEvm, InspectEvm, Inspector, MainBuilder, MainContext, SystemCallEvm,
};

mod block;
pub use block::*;

pub mod dao_fork;
pub mod eip6110;
pub mod receipt_builder;
pub mod spec;

/// The Ethereum EVM context type.
pub type EthEvmContext<DB> = Context<HashMap<u64, BlockEnv>, TxEnv, CfgEnv, DB>;

/// Helper builder to construct `EthEvm` instances in a unified way.
#[derive(Debug)]
pub struct EthEvmBuilder<DB: MultiDatabase, I = NoOpInspector> {
    db: DB,
    block_env: HashMap<u64, BlockEnv>,
    cfg_env: CfgEnv,
    inspector: I,
    inspect: bool,
    precompiles: Option<PrecompilesMap>,
}

impl<DB: MultiDatabase> EthEvmBuilder<DB, NoOpInspector> {
    /// Creates a builder from the provided `EvmEnv` and database.
    pub fn new(db: DB, env: EvmEnv) -> Self {
        Self {
            db,
            block_env: env.block_env,
            cfg_env: env.cfg_env,
            inspector: NoOpInspector {},
            inspect: false,
            precompiles: None,
        }
    }
}

impl<DB: MultiDatabase, I> EthEvmBuilder<DB, I> {
    /// Sets a custom inspector
    pub fn inspector<J>(self, inspector: J) -> EthEvmBuilder<DB, J> {
        EthEvmBuilder {
            db: self.db,
            block_env: self.block_env,
            cfg_env: self.cfg_env,
            inspector,
            inspect: self.inspect,
            precompiles: self.precompiles,
        }
    }

    /// Sets a custom inspector and enables invoking it during transaction execution.
    pub fn activate_inspector<J>(self, inspector: J) -> EthEvmBuilder<DB, J> {
        self.inspector(inspector).inspect()
    }

    /// Sets whether to invoke the inspector during transaction execution.
    pub fn set_inspect(mut self, inspect: bool) -> Self {
        self.inspect = inspect;
        self
    }

    /// Enables invoking the inspector during transaction execution.
    pub fn inspect(self) -> Self {
        self.set_inspect(true)
    }

    /// Overrides the precompiles map. If not provided, it will be derived from the `SpecId` in
    /// `CfgEnv`.
    pub fn precompiles(mut self, precompiles: PrecompilesMap) -> Self {
        self.precompiles = Some(precompiles);
        self
    }

    /// Builds the `EthEvm` instance.
    pub fn build(self) -> EthEvm<DB, I, PrecompilesMap>
    where
        I: Inspector<EthEvmContext<DB>>,
    {
        let xchain = !self.block_env.is_empty() && self.block_env.len() > 1;
        let precompiles = match self.precompiles {
            Some(p) => p,
            None => PrecompilesMap::from_static(Precompiles::new(
                PrecompileSpecId::from_spec_id(self.cfg_env.spec),
                xchain,
            )),
        };

        let inner = Context::mainnet()
            .with_blocks(self.block_env)
            .with_cfg(self.cfg_env)
            .with_db(self.db)
            .build_mainnet_with_inspector(self.inspector)
            .with_precompiles(precompiles);

        EthEvm { inner, inspect: self.inspect }
    }
}

/// Ethereum EVM implementation.
///
/// This is a wrapper type around the `revm` ethereum evm with optional [`Inspector`] (tracing)
/// support. [`Inspector`] support is configurable at runtime because it's part of the underlying
/// [`RevmEvm`] type.
#[expect(missing_debug_implementations)]
pub struct EthEvm<DB: MultiDatabase, I, PRECOMPILE = EthPrecompiles> {
    inner: RevmEvm<
        EthEvmContext<DB>,
        I,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILE,
        EthFrame,
    >,
    inspect: bool,
}

impl<DB: MultiDatabase, I, PRECOMPILE> EthEvm<DB, I, PRECOMPILE> {
    /// Creates a new Ethereum EVM instance.
    ///
    /// The `inspect` argument determines whether the configured [`Inspector`] of the given
    /// [`RevmEvm`] should be invoked on [`Evm::transact`].
    pub const fn new(
        evm: RevmEvm<
            EthEvmContext<DB>,
            I,
            EthInstructions<EthInterpreter, EthEvmContext<DB>>,
            PRECOMPILE,
            EthFrame,
        >,
        inspect: bool,
    ) -> Self {
        Self { inner: evm, inspect }
    }

    /// Consumes self and return the inner EVM instance.
    pub fn into_inner(
        self,
    ) -> RevmEvm<
        EthEvmContext<DB>,
        I,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILE,
        EthFrame,
    > {
        self.inner
    }

    /// Provides a reference to the EVM context.
    pub const fn ctx(&self) -> &EthEvmContext<DB> {
        &self.inner.ctx
    }

    /// Provides a mutable reference to the EVM context.
    pub fn ctx_mut(&mut self) -> &mut EthEvmContext<DB> {
        &mut self.inner.ctx
    }
}

impl<DB: MultiDatabase, I, PRECOMPILE> Deref for EthEvm<DB, I, PRECOMPILE> {
    type Target = EthEvmContext<DB>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.ctx()
    }
}

impl<DB: MultiDatabase, I, PRECOMPILE> DerefMut for EthEvm<DB, I, PRECOMPILE> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx_mut()
    }
}

impl<DB, I, PRECOMPILE> Evm for EthEvm<DB, I, PRECOMPILE>
where
    DB: MultiDatabase,
    I: Inspector<EthEvmContext<DB>>,
    PRECOMPILE: PrecompileProvider<EthEvmContext<DB>, Output = InterpreterResult>,
{
    type DB = DB;
    type Tx = TxEnv;
    type Error = EVMError<DB::Error>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type Precompiles = PRECOMPILE;
    type Inspector = I;

    fn blocks(&self) -> &HashMap<u64, BlockEnv> {
        &self.inner.ctx.block
    }

    fn chain_id(&self) -> u64 {
        self.inner.ctx.cfg.chain_id
    }

    fn transact(
        &mut self,
        tx: impl crate::IntoTxEnv<Self::Tx>,
    ) -> Result<ResultAndState, Self::Error> {
        let mut tx_env = tx.into_tx_env();

        // For legacy transactions without a chain_id, use the default from config
        if tx_env.chain_id.is_none() {
            let default_chain_id = if let Some(parent_chain_id) = self.cfg.parent_chain_id {
                parent_chain_id
            } else {
                self.cfg.chain_id
            };
            tx_env.chain_id = Some(default_chain_id);

            // Also update the caller and call addresses if they use chain_id 0 (which would be from legacy tx defaulting)
            if tx_env.caller.0 == 0 {
                tx_env.caller = ChainAddress::new(default_chain_id, tx_env.caller.1);
            }
            if let TxKind::Call(ref mut addr) = tx_env.kind {
                if addr.0 == 0 {
                    *addr = ChainAddress::new(default_chain_id, addr.1);
                }
            }
        }

        // Set chain_ids from available blocks
        tx_env.chain_ids = Some(self.blocks().keys().cloned().collect());
        self.transact_raw(tx_env)
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        if self.inspect {
            self.inner.inspect_tx(tx)
        } else {
            self.inner.transact(tx)
        }
    }

    fn transact_system_call(
        &mut self,
        caller: ChainAddress,
        contract: ChainAddress,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        // Use the new system_call_with_caller from revm v86
        self.inner.system_call_with_caller(caller.1, contract.1, data)
    }

    fn db_mut(&mut self) -> &mut Self::DB {
        &mut self.inner.ctx.db
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let (mut context, _instructions, _precompiles, _inspector, _frame) = self.inner.into_parts();
        let db = core::mem::take(&mut context.db);
        let env = EvmEnv { block_env: context.block, cfg_env: context.cfg };
        (db, env)
    }

    fn precompiles_mut(&mut self) -> &mut Self::Precompiles {
        self.inner.precompiles_mut()
    }

    fn inspector_mut(&mut self) -> &mut Self::Inspector {
        self.inner.inspector_mut()
    }
}

/// Factory for creating and configuring EVM instances.
#[derive(Debug, Clone, Copy, Default)]
pub struct EthEvmFactory;

impl<DB, R> EvmFactory<DB, R> for EthEvmFactory
where
    DB: MultiDatabase,
    R: crate::receipt_builder::ReceiptBuilder,
{
    type Evm<I: Inspector<EthEvmContext<DB>>> = EthEvm<DB, I>;
    type Context<'a> = EthBlockExecutionCtx<'a>;
    type Hardforks = SpecId;
    type Transaction = <Self::Evm<NoOpInspector> as Evm>::Tx;

    fn create_evm<I: Inspector<EthEvmContext<DB>>>(
        &self,
        db: DB,
        env: EvmEnv<Self::Hardforks>,
        inspector: I,
    ) -> Self::Evm<I> {
        EthEvmBuilder::new(db, env).activate_inspector(inspector).build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use alloy_primitives::address;
    use revm::database::EmptyDB;

    #[test]
    fn test_build() {
        let env = EvmEnv::default();
        let evm = EthEvmBuilder::new(EmptyDB::default(), env).build();
        assert_eq!(evm.chain_id(), 1);
    }

    #[test]
    fn test_evm_transact_system_call() {
        let env = EvmEnv::default();
        let mut evm = EthEvmBuilder::new(EmptyDB::default(), env).build();
        let caller = ChainAddress::new(1, address!("0000000000000000000000000000000000000000"));
        let contract = ChainAddress::new(1, address!("4788629ABc6cFCA10F9f969efdEAa1cF70c23555"));
        let data = Bytes::default();
        let r = evm.transact_system_call(caller, contract, data);
        assert!(r.is_ok());
    }
}