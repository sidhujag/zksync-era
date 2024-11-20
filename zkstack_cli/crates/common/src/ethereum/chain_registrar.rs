use std::sync::Arc;

use ethers::{
    abi::Address,
    contract::abigen,
    middleware::Middleware,
    prelude::{Http, Provider},
    types::{BlockNumber, H256, U64},
};

abigen!(
    ChainRegistar,
    r"[
        event NewChainDeployed(uint256 indexed chainId, address author, address diamondProxy, address chainAdmin)
        event NewChainRegistrationProposal(uint256 indexed chainId, address author, bytes32 key)
        event SharedBridgeRegistered(uint256 indexed chainId, address l2Address)
    ]"
);

#[derive(Clone, Copy)]
struct ChainRegistrationResultBuilder {
    pub diamond_proxy: Option<Address>,
    pub chain_admin: Option<Address>,
    pub l2_shared_bridge: Option<Address>,
}

impl ChainRegistrationResultBuilder {
    fn new() -> Self {
        Self {
            diamond_proxy: None,
            chain_admin: None,
            l2_shared_bridge: None,
        }
    }
    fn with_diamond_proxy(mut self, address: Address) -> Self {
        self.diamond_proxy = Some(address);
        self
    }
    fn with_chain_admin(mut self, address: Address) -> Self {
        self.chain_admin = Some(address);
        self
    }

    fn with_l2_shared_bridge(mut self, address: Address) -> Self {
        self.l2_shared_bridge = Some(address);
        self
    }

    fn build(self) -> ChainRegistrationResult {
        ChainRegistrationResult {
            diamond_proxy: self.diamond_proxy.unwrap(),
            chain_admin: self.chain_admin.unwrap(),
            l2_shared_bridge: self.l2_shared_bridge.unwrap(),
        }
    }
}

pub struct ChainRegistrationResult {
    pub diamond_proxy: Address,
    pub chain_admin: Address,
    pub l2_shared_bridge: Address,
}

pub async fn load_contracts_for_chain(
    chain_registrar: Address,
    l1_rpc: String,
    l2_chain_id: u64,
) -> anyhow::Result<ChainRegistrationResult> {
    let client = Arc::new(Provider::<Http>::try_from(l1_rpc)?);
    let block = client.get_block_number().await?;
    let contract = ChainRegistar::new(chain_registrar, client);
    let events = contract
        .events()
        .from_block(block.saturating_sub(U64::from(100000)))
        .to_block(BlockNumber::Latest)
        .topic1(H256::from_low_u64_be(l2_chain_id));
    let result = events.query().await?;
    let mut builder = ChainRegistrationResultBuilder::new();
    for event in &result {
        match event {
            ChainRegistarEvents::NewChainDeployedFilter(event) => {
                builder = builder
                    .with_diamond_proxy(event.diamond_proxy)
                    .with_chain_admin(event.chain_admin);
            }
            ChainRegistarEvents::SharedBridgeRegisteredFilter(event) => {
                builder = builder.with_l2_shared_bridge(event.l_2_address);
            }
            _ => {}
        }
    }
    Ok(builder.build())
}