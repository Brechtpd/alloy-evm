//! Ethereum EVM implementation.

use crate::{env::EvmEnv, evm::EvmFactory, precompiles::PrecompilesMap, Evm, MultiDatabase};
use alloy_primitives::Bytes;
use core::{
    fmt::Debug,
    ops::{Deref, DerefMut},
};
use revm::{
    primitives::{MultiChainTxKind as TxKind, HashMap, ChainAddress, hardfork::SpecId},
    context::{BlockEnv, CfgEnv, Context, ContextTr, TxEnv, LocalContext},
    context_interface::result::{EVMError, HaltReason, ResultAndState},
    database::{EmptyDB, State},
    handler::{instructions::EthInstructions, EthFrame, EthPrecompiles, PrecompileProvider},
    inspector::NoOpInspector,
    interpreter::{interpreter::EthInterpreter, InterpreterResult},
    precompile::{PrecompileSpecId, Precompiles},
    AutoSetupBuilder, MainnetEvm as RevmEvm, ExecuteEvm, Inspector, SystemCallEvm,
};

mod block;
pub use block::*;

pub mod dao_fork;
pub mod eip6110;
pub mod receipt_builder;
pub mod spec;

/// The Ethereum EVM context type.
pub type EthEvmContext<DB> = Context<BlockEnv, TxEnv, CfgEnv, DB>;

/// Helper builder to construct `EthEvm` instances in a unified way.
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

impl<DB: MultiDatabase, I> core::fmt::Debug for EthEvmBuilder<DB, I> 
where 
    DB: core::fmt::Debug,
    I: core::fmt::Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EthEvmBuilder")
            .field("db", &self.db)
            .field("block_env", &self.block_env)
            .field("inspector", &self.inspector)
            .field("inspect", &self.inspect)
            .field("precompiles", &self.precompiles.is_some())
            .finish()
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

    /// Builds the `EthEvm` instance without inspector.
    pub fn build_no_inspector(self) -> EthEvm<DB, NoOpInspector, PrecompilesMap> {
        let xchain = !self.block_env.is_empty() && self.block_env.len() > 1;
        let precompiles = match self.precompiles {
            Some(p) => p,
            None => PrecompilesMap::from_static(Precompiles::new(
                PrecompileSpecId::from_spec_id(self.cfg_env.spec),
                xchain,
            )),
        };

        // Create a new Context with the database and spec
        let spec = self.cfg_env.spec;
        let ctx = Context::<BlockEnv, TxEnv, CfgEnv, DB>::new(self.db, spec)
            .with_cfg(self.cfg_env)
            .with_blocks(self.block_env)
            .with_tx(TxEnv::default())
            .with_local(LocalContext::default());
        
        // Build the EVM with NoOpInspector
        let inner = ctx.build_mainnet_with_inspector_auto(NoOpInspector {})
            .with_precompiles(precompiles);

        EthEvm { inner, inspect: false }
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

        // Create a new Context with the database and spec
        let spec = self.cfg_env.spec;
        let ctx = Context::<BlockEnv, TxEnv, CfgEnv, DB>::new(self.db, spec)
            .with_cfg(self.cfg_env)
            .with_blocks(self.block_env)
            .with_tx(TxEnv::default())
            .with_local(LocalContext::default());
        
        // Build the EVM with inspector and precompiles
        let inner = ctx.build_mainnet_with_inspector_auto(self.inspector)
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
pub struct EthEvm<DB: MultiDatabase, I, PRECOMPILE = PrecompilesMap> {
    inner: revm::context::Evm<
        EthEvmContext<DB>,
        I,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILE,
        EthFrame<EthInterpreter>,
    >,
    inspect: bool,
}


impl<DB: MultiDatabase, I, PRECOMPILE> EthEvm<DB, I, PRECOMPILE> {
    /// Creates a new Ethereum EVM instance.
    ///
    /// The `inspect` argument determines whether the configured [`Inspector`] of the given
    /// [`RevmEvm`] should be invoked on [`Evm::transact`].
    pub const fn new(
        evm: revm::context::Evm<
            EthEvmContext<DB>,
            I,
            EthInstructions<EthInterpreter, EthEvmContext<DB>>,
            PRECOMPILE,
            EthFrame<EthInterpreter>,
        >,
        inspect: bool,
    ) -> Self {
        Self { inner: evm, inspect }
    }

    /// Consumes self and return the inner EVM instance.
    pub fn into_inner(
        self,
    ) -> revm::context::Evm<
        EthEvmContext<DB>,
        I,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILE,
        EthFrame<EthInterpreter>,
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
        // For now, always use transact without inspector
        // The inspect_tx would require I: Inspector<EthEvmContext<DB>>
        self.inner.transact(tx)
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
        self.inner.ctx.db_mut()
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        // Destructure the EVM to get the context
        let revm::context::Evm { ctx, .. } = self.inner;
        let Context { block, cfg, journaled_state, .. } = ctx;
        
        // Get the database from the journaled state  
        let db = journaled_state.database;
        
        // Create the environment
        let env = EvmEnv { block_env: block, cfg_env: cfg };
        (db, env)
    }

    fn precompiles_mut(&mut self) -> &mut Self::Precompiles {
        &mut self.inner.precompiles
    }

    fn inspector_mut(&mut self) -> &mut Self::Inspector {
        &mut self.inner.inspector
    }
}

/// Factory for creating and configuring EVM instances.
#[derive(Debug, Clone, Copy, Default)]
pub struct EthEvmFactory;

// Implementation specifically for &mut State<DB> which is what BlockExecutor needs
impl<'a, DB> EvmFactory<&'a mut State<DB>> for EthEvmFactory
where
    DB: MultiDatabase + 'a,
{
    type Evm<I> = EthEvm<&'a mut State<DB>, NoOpInspector>;  // Always NoOpInspector
    type Context<'b> = EthEvmContext<&'a mut State<DB>>;
    type Hardforks = SpecId;
    type Transaction = TxEnv;

    fn create_evm<I>(
        &self,
        db: &'a mut State<DB>,
        env: EvmEnv<Self::Hardforks>,
        _inspector: I,
    ) -> Self::Evm<I> {
        // Always return NoOpInspector version, ignoring the provided inspector
        // This is a limitation of the current type system
        EthEvmBuilder::new(db, env).build_no_inspector()
    }
}

// Default implementation for () to satisfy the default type parameter
impl EvmFactory<()> for EthEvmFactory {
    type Evm<I> = EthEvm<EmptyDB, NoOpInspector>;
    type Context<'a> = EthEvmContext<EmptyDB>;
    type Hardforks = SpecId;
    type Transaction = TxEnv;

    fn create_evm<I>(
        &self,
        _db: (),
        env: EvmEnv<Self::Hardforks>,
        _inspector: I,
    ) -> Self::Evm<I> {
        EthEvmBuilder::new(EmptyDB::default(), env).build_no_inspector()
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