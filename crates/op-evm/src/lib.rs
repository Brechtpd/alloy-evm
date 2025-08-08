#![doc = include_str!("../README.md")]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/alloy-rs/core/main/assets/alloy.jpg",
    html_favicon_url = "https://raw.githubusercontent.com/alloy-rs/core/main/assets/favicon.ico"
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod block;
pub use block::{OpBlockExecutionCtx, OpBlockExecutor, OpBlockExecutorFactory};

// Stub implementations since Op code won't be used but needs to compile
// The real implementation would require MultiChainBlockEnv to implement Block trait

use alloy_evm::{Evm, EvmEnv, EvmFactory, MultiDatabase};
use alloy_primitives::{Address, Bytes};
use op_revm::{OpHaltReason, OpSpecId, OpTransactionError};
use revm::{
    context::{BlockEnv, TxEnv},
    context_interface::result::{EVMError, ResultAndState},
    inspector::NoOpInspector,
    Inspector,
};

/// Stub OP EVM implementation
#[derive(Debug)]
pub struct OpEvm<DB, I> {
    _db: core::marker::PhantomData<DB>,
    _inspector: core::marker::PhantomData<I>,
}

impl<DB: MultiDatabase, I> Evm for OpEvm<DB, I> {
    type DB = DB;
    type Tx = TxEnv;
    type Error = EVMError<DB::Error, OpTransactionError>;
    type HaltReason = OpHaltReason;
    type Spec = OpSpecId;
    type Precompiles = ();
    type Inspector = I;

    fn block(&self) -> &BlockEnv {
        unimplemented!("OpEvm stub - not for production use")
    }

    fn chain_id(&self) -> u64 {
        1 // Default to mainnet
    }

    fn transact_raw(&mut self, _tx: Self::Tx) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        unimplemented!("OpEvm stub - not for production use")
    }

    fn transact_system_call(
        &mut self,
        _caller: Address,
        _contract: Address,
        _data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        unimplemented!("OpEvm stub - not for production use")
    }

    fn db_mut(&mut self) -> &mut DB {
        unimplemented!("OpEvm stub - not for production use")
    }

    fn finish(self) -> (DB, EvmEnv<Self::Spec>) {
        unimplemented!("OpEvm stub - not for production use")
    }

    fn set_inspector_enabled(&mut self, _enabled: bool) {}

    fn precompiles(&self) -> &Self::Precompiles {
        &()
    }

    fn precompiles_mut(&mut self) -> &mut Self::Precompiles {
        unimplemented!("OpEvm stub - not for production use")
    }

    fn inspector(&self) -> &I {
        unimplemented!("OpEvm stub - not for production use")
    }

    fn inspector_mut(&mut self) -> &mut I {
        unimplemented!("OpEvm stub - not for production use")
    }
}

/// Stub factory for OpEvm
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct OpEvmFactory;

impl EvmFactory for OpEvmFactory {
    type Evm<DB: MultiDatabase, I: Inspector<Self::Context<DB>>> = OpEvm<DB, I>;
    // Use a simple context type that satisfies bounds
    type Context<DB: MultiDatabase> = revm::Context<BlockEnv, TxEnv, revm::context::CfgEnv<OpSpecId>, DB>;
    type Tx = TxEnv;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError, OpTransactionError>;
    type HaltReason = OpHaltReason;
    type Spec = OpSpecId;
    type Precompiles = ();

    fn create_evm<DB: MultiDatabase>(
        &self,
        _db: DB,
        _input: EvmEnv<OpSpecId>,
    ) -> Self::Evm<DB, NoOpInspector> {
        OpEvm {
            _db: core::marker::PhantomData,
            _inspector: core::marker::PhantomData,
        }
    }

    fn create_evm_with_inspector<DB: MultiDatabase, I: Inspector<Self::Context<DB>>>(
        &self,
        _db: DB,
        _input: EvmEnv<OpSpecId>,
        _inspector: I,
    ) -> Self::Evm<DB, I> {
        OpEvm {
            _db: core::marker::PhantomData,
            _inspector: core::marker::PhantomData,
        }
    }
}