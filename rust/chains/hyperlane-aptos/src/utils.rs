use crate::{AptosClient, TxSpecificData};
use anyhow::{Context, Result};
use aptos_sdk::{
    crypto::ed25519::{Ed25519PrivateKey, Ed25519PublicKey, Ed25519Signature},
    move_types::language_storage::TypeTag,
    move_types::{ident_str, language_storage::ModuleId},
    rest_client::aptos_api_types::{
        EntryFunctionId, MoveType, Transaction as AptosTransaction, TransactionInfo,
        VersionedEvent, ViewRequest,
    },
    transaction_builder::TransactionFactory,
    types::{
        account_address::AccountAddress,
        chain_id::ChainId,
        transaction::{
            authenticator::AuthenticationKey, EntryFunction, SignedTransaction, TransactionPayload,
        },
        AccountKey, LocalAccount,
    },
};
use hyperlane_core::{ChainCommunicationError, ChainResult, Indexed, LogMeta, H256, H512, U256};
use solana_sdk::signature::Keypair;
use std::{ops::RangeInclusive, str::FromStr};

/// limit of gas unit
const GAS_UNIT_LIMIT: u64 = 100000;
/// minimum price of gas unit of aptos chains
pub const GAS_UNIT_PRICE: u64 = 100;

/// Send Aptos Transaction
pub async fn send_aptos_transaction(
    aptos_client: &AptosClient,
    signer: &mut LocalAccount,
    payload: TransactionPayload,
) -> Result<AptosTransaction> {
    let state = aptos_client
        .get_ledger_information()
        .await
        .context("Failed in getting chain id")?
        .into_inner();

    let transaction_factory = TransactionFactory::new(ChainId::new(state.chain_id))
        .with_gas_unit_price(100)
        .with_max_gas_amount(GAS_UNIT_LIMIT);

    let signed_tx = signer.sign_with_transaction_builder(transaction_factory.payload(payload));

    let response = aptos_client
        .submit_and_wait(&signed_tx)
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?
        .into_inner();
    Ok(response)
}

/// Send Aptos Transaction
pub async fn simulate_aptos_transaction(
    aptos_client: &AptosClient,
    signer: &mut LocalAccount,
    payload: TransactionPayload,
) -> Result<TransactionInfo> {
    let state = aptos_client
        .get_ledger_information()
        .await
        .context("Failed in getting chain id")?
        .into_inner();

    let transaction_factory = TransactionFactory::new(ChainId::new(state.chain_id))
        .with_gas_unit_price(GAS_UNIT_PRICE)
        .with_max_gas_amount(GAS_UNIT_LIMIT);

    let raw_tx = transaction_factory
        .payload(payload)
        .sender(signer.address())
        .sequence_number(signer.sequence_number())
        .build();

    let signed_tx = SignedTransaction::new(
        raw_tx,
        signer.public_key().clone(),
        Ed25519Signature::try_from([0u8; 64].as_ref()).unwrap(),
    );

    let response_txns = aptos_client.simulate(&signed_tx).await?.into_inner();
    let response = response_txns[0].clone();

    Ok(response.info)
}

/// Make Aptos Transaction Payload
pub fn make_aptos_payload(
    package_address: AccountAddress,
    module_name: &'static str,
    function_name: &'static str,
    ty_args: Vec<TypeTag>,
    args: Vec<Vec<u8>>,
) -> TransactionPayload {
    TransactionPayload::EntryFunction(EntryFunction::new(
        ModuleId::new(package_address, ident_str!(module_name).to_owned()),
        ident_str!(function_name).to_owned(),
        ty_args,
        args,
    ))
}

/// Send View Request
pub async fn send_view_request(
    aptos_client: &AptosClient,
    package_address: String,
    module_name: String,
    function_name: String,
    type_arguments: Vec<MoveType>,
    arguments: Vec<serde_json::Value>,
) -> ChainResult<Vec<serde_json::Value>> {
    let view_response = aptos_client
        .view(
            &ViewRequest {
                function: EntryFunctionId::from_str(&format!(
                    "{package_address}::{module_name}::{function_name}"
                ))
                .unwrap(),
                type_arguments,
                arguments,
            },
            Option::None,
        )
        .await
        .map_err(ChainCommunicationError::from_other)?
        .into_inner();
    Ok(view_response)
}

/// Convert address string to H256
pub fn convert_hex_string_to_h256(addr: &str) -> Result<H256, String> {
    let formated_addr = format!("{:0>64}", addr.to_string().trim_start_matches("0x"));
    H256::from_str(&formated_addr).map_err(|e| e.to_string())
}

/// Convert payer(Keypair) into Aptos LocalAccount
pub async fn convert_keypair_to_aptos_account(
    aptos_client: &AptosClient,
    payer: &Keypair,
) -> LocalAccount {
    let signer_priv_key = Ed25519PrivateKey::try_from(payer.secret().to_bytes().as_ref()).unwrap();
    let signer_address =
        AuthenticationKey::ed25519(&Ed25519PublicKey::from(&signer_priv_key)).account_address();
    let signer_account = LocalAccount::new(
        signer_address,
        AccountKey::from_private_key(signer_priv_key),
        aptos_client
            .get_account(signer_address)
            .await
            .map_err(ChainCommunicationError::from_other)
            .unwrap()
            .into_inner()
            .sequence_number,
    );
    signer_account
}

/// Filter events based on range
pub async fn get_filtered_events<T, S>(
    aptos_client: &AptosClient,
    account_address: AccountAddress,
    struct_tag: &str,
    field_name: &str,
    range: RangeInclusive<u32>,
) -> ChainResult<Vec<(Indexed<T>, LogMeta)>>
where
    S: TryFrom<VersionedEvent> + TxSpecificData + TryInto<T> + Clone,
    ChainCommunicationError:
        From<<S as TryFrom<VersionedEvent>>::Error> + From<<S as TryInto<T>>::Error>,
{
    // fetch events from global storage
    let events: Vec<VersionedEvent> = aptos_client
        .get_account_events(account_address, struct_tag, field_name, None, Some(10000))
        .await
        .map_err(ChainCommunicationError::from_other)?
        .into_inner();

    // get start block and end block
    let blk_start_no: u32 = *range.start();
    let blk_end_no = *range.end();
    let start_block = aptos_client
        .get_block_by_height(blk_start_no as u64, false)
        .await
        .map_err(ChainCommunicationError::from_other)?
        .into_inner();
    let end_block = aptos_client
        .get_block_by_height(blk_end_no as u64, false)
        .await
        .map_err(ChainCommunicationError::from_other)?
        .into_inner();
    let start_tx_version = start_block.first_version;
    let end_tx_version = end_block.last_version;

    // filter events which are from `start_tx_version` to `end_tx_version`
    let filtered_events: Vec<VersionedEvent> = events
        .into_iter()
        .filter(|e| e.version.0 > start_tx_version.0 && e.version.0 <= end_tx_version.0)
        .collect();

    // prepare result
    let mut messages: Vec<(Indexed<T>, LogMeta)> =
        Vec::with_capacity((range.end() - range.start()) as usize);
    for filtered_event in filtered_events {
        let evt_data: S = filtered_event.clone().try_into()?;
        let block_height = evt_data.block_height().parse().unwrap();
        let block = aptos_client
            .get_block_by_height(block_height as u64, false)
            .await
            .map_err(ChainCommunicationError::from_other)?
            .into_inner();
        messages.push((
            Indexed::new(evt_data.clone().try_into()?),
            LogMeta {
                address: account_address.into_bytes().into(),
                block_number: block_height,
                block_hash: convert_hex_string_to_h256(&block.block_hash.to_string()).unwrap(),
                transaction_id: H512::from(
                    convert_hex_string_to_h256(&evt_data.transaction_hash()).unwrap(),
                ),
                transaction_index: *filtered_event.version.inner(),
                log_index: U256::from(*filtered_event.sequence_number.inner()),
            },
        ));
    }

    Ok(messages)
}
