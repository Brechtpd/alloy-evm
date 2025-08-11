//! Configuration types for EVM environment.

use revm::{
    context::{BlockEnv, CfgEnv},
    primitives::{hardfork::SpecId, HashMap},
};

/// Container type that holds both the configuration and block environment for EVM execution.
#[derive(Debug, Clone, Default)]
pub struct EvmEnv<Spec = SpecId> {
    /// The configuration environment with handler settings
    pub cfg_env: CfgEnv<Spec>,
    /// The block environment containing block-specific data
    pub block_env: HashMap<u64, BlockEnv>,
}

impl<Spec> EvmEnv<Spec> {
    /// Create a new `EvmEnv` from its components.
    ///
    /// # Arguments
    ///
    /// * `cfg_env_with_handler_cfg` - The configuration environment with handler settings
    /// * `block` - The block environment containing block-specific data
    pub const fn new(cfg_env: CfgEnv<Spec>, block_env: HashMap<u64, BlockEnv>) -> Self {
        Self { cfg_env, block_env }
    }

    /// Returns a reference to the block environment.
    pub const fn block_env(&self) -> &HashMap<u64, BlockEnv> {
        &self.block_env
    }

    /// Returns a reference to the configuration environment.
    pub const fn cfg_env(&self) -> &CfgEnv<Spec> {
        &self.cfg_env
    }

    /// Returns the chain ID of the environment.
    pub const fn chainid(&self) -> u64 {
        self.cfg_env.chain_id
    }

    /// Returns the spec id of the chain
    pub const fn spec_id(&self) -> &Spec {
        &self.cfg_env.spec
    }
}

impl<Spec> From<(CfgEnv<Spec>, HashMap<u64, BlockEnv>)> for EvmEnv<Spec> {
    fn from((cfg_env, block_env): (CfgEnv<Spec>, HashMap<u64, BlockEnv>)) -> Self {
        Self { cfg_env, block_env }
    }
}
