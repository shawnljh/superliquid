use std::io::{Error, ErrorKind, Result};
use std::sync::Arc;

use tokio::{
    net::tcp::OwnedWriteHalf,
    sync::{Mutex, mpsc, oneshot},
};

use crate::{
    networking::message_protocol::{self, AppMessage, ControlMessage},
    node::{peer::broadcast::broadcast_transaction, state::Node},
    state::{
        asset::{Asset, AssetId},
        spot_clearinghouse::MarketId,
        spot_market::MarketInfo,
        state::AccountInfoWithBalances,
    },
    types::{
        message::{Message, ReplicaInBound, mpsc_error},
        transaction::{
            PublicKeyHash, SignedTransaction, TransactionStatus, TransferTransaction,
            UnsignedTransaction,
        },
    },
};

use super::listener::ClientSocket;

pub enum ClientQuery {
    Account(PublicKeyHash),
    Asset,
    MarketInfo(MarketId),
    Markets,
}

#[derive(Debug)]
pub enum ClientResponse {
    AccountQueryReponse(AccountInfoWithBalances),
    AssetQueryResponse(Vec<Asset>),
    MarketInfoQueryResponse(Option<MarketInfo>),
    MarketsQueryResponse(Vec<MarketInfo>),
}

pub struct QueryRequest {
    pub query: ClientQuery,
    pub response_channel: oneshot::Sender<ClientResponse>,
}

pub(super) async fn handle_client_connection(
    socket: ClientSocket,
    node: Arc<Node>,
    to_replica_tx: mpsc::Sender<ReplicaInBound>,
) -> Result<()> {
    loop {
        let message = message_protocol::receive_message(socket.reader.clone()).await?;
        match message {
            Some(Message::Application(AppMessage::SubmitTransaction(tx))) => {
                handle_transaction(&node, tx, to_replica_tx.clone()).await?;
            }
            Some(Message::Application(AppMessage::Query)) => {
                handle_query(socket.writer.clone(), &node).await?;
            }
            Some(Message::Application(AppMessage::Drip(pk, asset_id))) => {
                handle_drip(&node, pk, asset_id, to_replica_tx.clone()).await?
            }
            Some(Message::Application(AppMessage::AccountQuery(pk))) => {
                handle_account_query(socket.writer.clone(), pk, to_replica_tx.clone()).await?;
            }
            Some(Message::Application(AppMessage::AssetQuery)) => {
                handle_asset_query(socket.writer.clone(), to_replica_tx.clone()).await?;
            }
            Some(Message::Application(AppMessage::MarketsQuery)) => {
                handle_markets_query(socket.writer.clone(), to_replica_tx.clone()).await?;
            }
            Some(Message::Application(AppMessage::MarketInfoQuery(market_id))) => {
                handle_market_query(market_id, socket.writer.clone(), to_replica_tx.clone())
                    .await?;
            }
            Some(Message::Connection(ControlMessage::End)) => {
                return Ok(());
            }
            _ => {}
        }
    }
}

pub(super) async fn send_query_to_replica(
    query: ClientQuery,
    to_replica_tx: mpsc::Sender<ReplicaInBound>,
) -> Result<ClientResponse> {
    let (response_tx, response_rx) = oneshot::channel();

    // Request info from replica to oneshot channel
    to_replica_tx
        .send(ReplicaInBound::Query(QueryRequest {
            query,
            response_channel: response_tx,
        }))
        .await
        .map_err(|e| mpsc_error("Failed to send query request to replica", e))?;

    println!("Waiting for response");
    let response = response_rx
        .await
        .map_err(|e| mpsc_error("Failed to recieve response from replica", e))?;
    Ok(response)
}

pub(super) async fn handle_account_query(
    writer: Arc<Mutex<OwnedWriteHalf>>,
    pk_bytes: PublicKeyHash,
    to_replica_tx: mpsc::Sender<ReplicaInBound>,
) -> Result<()> {
    let query = ClientQuery::Account(pk_bytes);
    let response = send_query_to_replica(query, to_replica_tx).await?;

    match response {
        ClientResponse::AccountQueryReponse(account_info_with_balances) => {
            // send to client
            message_protocol::send_message(
                writer,
                &Message::Application(AppMessage::AccountQueryResponse(account_info_with_balances)),
            )
            .await?;
        }
        other => {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Expected AccountQueryResponse, got {:?}", other),
            ));
        }
    }
    Ok(())
}

pub(super) async fn handle_asset_query(
    writer: Arc<Mutex<OwnedWriteHalf>>,
    to_replica_tx: mpsc::Sender<ReplicaInBound>,
) -> Result<()> {
    let query = ClientQuery::Asset;
    let response = send_query_to_replica(query, to_replica_tx).await?;

    match response {
        ClientResponse::AssetQueryResponse(asset_info) => {
            // send to client
            message_protocol::send_message(
                writer,
                &Message::Application(AppMessage::AssetQueryResponse(asset_info)),
            )
            .await?;
        }
        other => {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Expected AssetQueryResponse, got {:?}", other),
            ));
        }
    }
    Ok(())
}

pub(super) async fn handle_market_query(
    market_id: MarketId,
    writer: Arc<Mutex<OwnedWriteHalf>>,
    to_replica_tx: mpsc::Sender<ReplicaInBound>,
) -> Result<()> {
    let query = ClientQuery::MarketInfo(market_id);
    let response = send_query_to_replica(query, to_replica_tx).await?;

    match response {
        ClientResponse::MarketInfoQueryResponse(market_info) => {
            // send to client
            message_protocol::send_message(
                writer,
                &Message::Application(AppMessage::MarketInfoQueryResponse(market_info)),
            )
            .await?;
        }
        other => {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Expected AssetQueryResponse, got {:?}", other),
            ));
        }
    }
    Ok(())
}

pub(super) async fn handle_markets_query(
    writer: Arc<Mutex<OwnedWriteHalf>>,
    to_replica_tx: mpsc::Sender<ReplicaInBound>,
) -> Result<()> {
    let query = ClientQuery::Markets;
    let response = send_query_to_replica(query, to_replica_tx).await?;

    match response {
        ClientResponse::MarketsQueryResponse(markets_info) => {
            // send to client
            message_protocol::send_message(
                writer,
                &Message::Application(AppMessage::MarketsQueryResponse(markets_info)),
            )
            .await?;
        }
        other => {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Expected AssetQueryResponse, got {:?}", other),
            ));
        }
    }
    Ok(())
}

pub(super) async fn handle_drip(
    node: &Arc<Node>,
    pk_bytes: PublicKeyHash,
    asset_id: AssetId,
    to_replica_tx: mpsc::Sender<ReplicaInBound>,
) -> Result<()> {
    let mut faucet_key = node.faucet_key.clone();
    let faucet_pk_bytes = faucet_key.verifying_key().to_bytes();

    let query = ClientQuery::Account(faucet_pk_bytes);
    let response = send_query_to_replica(query, to_replica_tx.clone()).await?;

    let account_info = match response {
        ClientResponse::AccountQueryReponse(account_info_with_balances) => {
            account_info_with_balances.account_info
        }
        other => {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Expected AccountQueryResponse, got {:?}", other),
            ));
        }
    };

    let drip_amount = if asset_id == 0 {
        1_000_000_000
    } else if asset_id == 1 {
        500_000_000
    } else {
        0
    };

    let drip_txn = UnsignedTransaction::Transfer(TransferTransaction {
        to: pk_bytes,
        from: faucet_pk_bytes,
        amount: drip_amount,
        asset_id,
        nonce: account_info.expected_nonce,
        status: TransactionStatus::Pending,
    });

    let drip_txn = drip_txn.sign(&mut faucet_key);
    handle_transaction(node, drip_txn, to_replica_tx).await
}

pub(crate) async fn handle_transaction(
    node: &Arc<Node>,
    signed_tx: SignedTransaction,
    to_replica_tx: mpsc::Sender<ReplicaInBound>,
) -> Result<()> {
    let logger = node.logger.clone();

    signed_tx.verify_sender();

    {
        let mut seen_transactions = node.seen_transactions.lock().await;
        if seen_transactions.insert(signed_tx.hash()) {
            {
                let mut transactions = node.transactions.lock().await;
                transactions.push(signed_tx.clone());
            }
        } else {
            return Ok(());
        }
    }
    logger.log("info", &format!("Received Transaction: {:?}", signed_tx));

    broadcast_transaction(node, signed_tx.clone()).await?;
    to_replica_tx
        .send(ReplicaInBound::Transaction(signed_tx))
        .await
        .map_err(|e| mpsc_error("Send to replica failed", e))?;

    Ok(())
}

pub(super) async fn handle_query(
    writer: Arc<Mutex<OwnedWriteHalf>>,
    node: &Arc<Node>,
) -> Result<()> {
    let logger = node.logger.clone();

    let peer_addr = { writer.lock().await.peer_addr() };

    logger.log("info", &format!("Received a query from {:?}", peer_addr));
    let txs = {
        let transactions = node.transactions.lock().await;
        transactions.clone()
    };
    message_protocol::send_message(writer, &Message::Application(AppMessage::Response(txs))).await
}
