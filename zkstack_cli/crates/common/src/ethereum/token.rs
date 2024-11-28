use std::{sync::Arc, time::Duration};

use ethers::{
    abi::Address,
    contract::abigen,
    middleware::MiddlewareBuilder,
    prelude::{Http, Provider},
};
use types::TokenInfo;

use crate::{ethereum::create_ethers_client, logger, wallets::Wallet};
abigen!(
    TokenContract,
    r"[
    function name() external view returns (string)
    function symbol() external view returns (string)
    function decimals() external view returns (uint8)
    function mint(address to, uint256 amount)
    ]"
);

pub async fn get_token_info(token_address: Address, rpc_url: String) -> anyhow::Result<TokenInfo> {
    let provider = Provider::<Http>::try_from(rpc_url)?;
    let contract = TokenContract::new(token_address, Arc::new(provider));

    let name = contract.name().call().await?;
    let symbol = contract.symbol().call().await?;
    let decimals = contract.decimals().call().await?;

    Ok(TokenInfo {
        name,
        symbol,
        decimals,
    })
}

pub async fn mint_token(
    main_wallet: Wallet,
    token_address: Address,
    addresses: Vec<Address>,
    l1_rpc: String,
    chain_id: u64,
    amount: u128,
) -> anyhow::Result<()> {
    let client = Arc::new(
        create_ethers_client(main_wallet.private_key.unwrap(), l1_rpc, Some(chain_id))?
            .nonce_manager(main_wallet.address),
    );

    let contract = TokenContract::new(token_address, client);

    let mut pending_calls = vec![];
    for address in addresses {
        pending_calls.push(contract.mint(address, amount.into()));
    }

    let mut pending_txs = vec![];
    for call in &pending_calls {
        let call = call.send().await;
        match call {
            // It's safe to set such low number of confirmations and low interval for localhost
            Ok(call) => pending_txs.push(call.confirmations(3).interval(Duration::from_millis(30))),
            Err(e) => logger::error(format!("Minting is not successful {e}")),
        }
    }

    futures::future::join_all(pending_txs).await;

    Ok(())
}