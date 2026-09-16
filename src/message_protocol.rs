use serde::{Deserialize, Serialize};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::Mutex;

use crate::hotstuff::message::HotStuffMessage;
use crate::network;
use crate::node::state::PeerId;
use crate::state::asset::{Asset, AssetId};
use crate::state::spot_clearinghouse::MarketId;
use crate::state::spot_market::MarketInfo;
use crate::state::state::AccountInfoWithBalances;
use crate::types::message::Message;
use crate::types::transaction::{PublicKeyHash, Sha256Hash, SignedTransaction};
use std::io::{Error, ErrorKind, Result};
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum AppMessage {
    Query,
    SubmitTransaction(SignedTransaction),
    Response(Vec<SignedTransaction>),
    Drip(PublicKeyHash, AssetId),
    Ack,
    AccountQuery(PublicKeyHash),
    AccountQueryResponse(AccountInfoWithBalances),

    MarketInfoQuery(MarketId),
    MarketInfoQueryResponse(Option<MarketInfo>),

    MarketsQuery,
    MarketsQueryResponse(Vec<MarketInfo>),

    AssetQuery,
    AssetQueryResponse(Vec<Asset>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ControlMessage {
    Hello { peer_id: usize },
    End, // Terminate connection
}

pub async fn receive_message(reader: Arc<Mutex<OwnedReadHalf>>) -> Result<Option<Message>> {
    network::receive_json::<Message, OwnedReadHalf>(reader).await
}

pub async fn send_message(writer: Arc<Mutex<OwnedWriteHalf>>, message: &Message) -> Result<()> {
    let json = serde_json::to_vec(&message)?;
    let _ = network::send_data(writer, &json).await;
    Ok(())
}

pub async fn send_hotstuff_message(
    writer: Arc<Mutex<OwnedWriteHalf>>,
    message: &HotStuffMessage,
) -> Result<()> {
    let json = serde_json::to_vec(&message)?;
    let _ = network::send_data(writer, &json).await;
    Ok(())
}

// Used for replicas to identify inbound connections
pub async fn send_hello(
    writer: Arc<Mutex<OwnedWriteHalf>>,
    reader: Arc<Mutex<OwnedReadHalf>>,
    peer_id: PeerId,
) -> Result<()> {
    let msg = ControlMessage::Hello { peer_id };
    send_message(writer, &Message::Connection(msg)).await?;

    let msg = receive_message(reader).await?;
    match msg {
        None | Some(Message::Application(AppMessage::Ack)) => Ok(()),
        Some(other) => Err(Error::new(
            ErrorKind::InvalidData,
            format!("Expected Ack or None, got {:?}", other),
        )),
    }
}

pub async fn send_transaction(
    writer: Arc<Mutex<OwnedWriteHalf>>,
    tx: SignedTransaction,
) -> Result<()> {
    let msg = AppMessage::SubmitTransaction(tx);
    send_message(writer, &Message::Application(msg)).await
}

pub async fn send_drip(
    writer: Arc<Mutex<OwnedWriteHalf>>,
    pk_bytes: &Sha256Hash,
    asset_id: AssetId,
) -> Result<()> {
    let msg = AppMessage::Drip(*pk_bytes, asset_id);

    send_message(writer, &Message::Application(msg)).await
}

pub async fn send_account_query(
    account_public_key: PublicKeyHash,
    reader: Arc<Mutex<OwnedReadHalf>>,
    writer: Arc<Mutex<OwnedWriteHalf>>,
) -> Result<AccountInfoWithBalances> {
    let msg = AppMessage::AccountQuery(account_public_key);
    send_message(writer, &Message::Application(msg)).await?;

    match receive_message(reader).await? {
        Some(Message::Application(AppMessage::AccountQueryResponse(account_info))) => {
            Ok(account_info)
        }
        other => Err(Error::new(
            ErrorKind::InvalidData,
            format!("Expected Response, got {:?}", other),
        )),
    }
}

pub async fn send_assets_query(
    reader: Arc<Mutex<OwnedReadHalf>>,
    writer: Arc<Mutex<OwnedWriteHalf>>,
) -> Result<Vec<Asset>> {
    let msg = AppMessage::AssetQuery;
    send_message(writer, &Message::Application(msg)).await?;

    match receive_message(reader).await? {
        Some(Message::Application(AppMessage::AssetQueryResponse(assets))) => Ok(assets),
        other => Err(Error::new(
            ErrorKind::InvalidData,
            format!("Expected Response, got {:?}", other),
        )),
    }
}

pub async fn send_markets_query(
    reader: Arc<Mutex<OwnedReadHalf>>,
    writer: Arc<Mutex<OwnedWriteHalf>>,
) -> Result<Vec<MarketInfo>> {
    let msg = AppMessage::MarketsQuery;
    send_message(writer, &Message::Application(msg)).await?;

    match receive_message(reader).await? {
        Some(Message::Application(AppMessage::MarketsQueryResponse(markets_info))) => {
            Ok(markets_info)
        }
        other => Err(Error::new(
            ErrorKind::InvalidData,
            format!("Expected Response, got {:?}", other),
        )),
    }
}

pub async fn send_market_info_query(
    market_id: MarketId,
    reader: Arc<Mutex<OwnedReadHalf>>,
    writer: Arc<Mutex<OwnedWriteHalf>>,
) -> Result<Option<MarketInfo>> {
    let msg = AppMessage::MarketInfoQuery(market_id);
    send_message(writer, &Message::Application(msg)).await?;

    match receive_message(reader).await? {
        Some(Message::Application(AppMessage::MarketInfoQueryResponse(market_info))) => {
            Ok(market_info)
        }
        other => Err(Error::new(
            ErrorKind::InvalidData,
            format!("Expected Response, got {:?}", other),
        )),
    }
}

pub async fn send_query(
    reader: Arc<Mutex<OwnedReadHalf>>,
    writer: Arc<Mutex<OwnedWriteHalf>>,
) -> Result<Option<Vec<SignedTransaction>>> {
    let msg = AppMessage::Query;
    send_message(writer, &Message::Application(msg)).await?;

    match receive_message(reader).await? {
        Some(Message::Application(AppMessage::Response(txs))) => Ok(Some(txs)),
        other => Err(Error::new(
            ErrorKind::InvalidData,
            format!("Expected Response, got {:?}", other),
        )),
    }
}

pub async fn send_end(writer: Arc<Mutex<OwnedWriteHalf>>) -> Result<()> {
    let msg = ControlMessage::End;
    send_message(writer, &&Message::Connection(msg)).await
}

pub async fn send_ack(writer: Arc<Mutex<OwnedWriteHalf>>) -> Result<()> {
    let msg = AppMessage::Ack;
    send_message(writer, &Message::Application(msg)).await
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::test_utils::test_helpers::{get_alice_pk_str, make_alice_transaction};
    use tokio::net::{TcpListener, TcpStream};

    #[tokio::test]
    async fn test_send_and_receive_transaction() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?; // random port
        let addr = listener.local_addr()?;

        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (reader, _) = socket.into_split();
            let reader = Arc::new(Mutex::new(reader));

            let msg = receive_message(reader).await.unwrap();
            match msg {
                Some(Message::Application(AppMessage::SubmitTransaction(signed_tx))) => {
                    match &signed_tx.tx {
                        crate::types::transaction::UnsignedTransaction::Transfer(
                            transfer_transaction,
                        ) => assert_eq!(transfer_transaction.amount, 42),
                        crate::types::transaction::UnsignedTransaction::Order(
                            _order_transaction,
                        ) => panic!("Expected order"),
                        crate::types::transaction::UnsignedTransaction::CancelOrder(
                            _cancel_order_transaction,
                        ) => panic!("Expected order"),
                    }
                }
                _ => panic!("Expected Transaction"),
            }
        });

        let stream = TcpStream::connect(addr).await?;
        let tx = make_alice_transaction();
        let (_, writer) = stream.into_split();
        let writer = Arc::new(Mutex::new(writer));
        send_transaction(writer, tx).await
    }

    #[tokio::test]
    async fn test_send_and_receive_query_response() -> Result<()> {
        use std::sync::Arc;
        use tokio::net::{TcpListener, TcpStream};
        use tokio::sync::Mutex;

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        // Spawn the server-side task
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (reader, writer) = socket.into_split();
            let writer = Arc::new(Mutex::new(writer));

            let reader = Arc::new(Mutex::new(reader));
            let msg_opt = receive_message(reader).await.unwrap();
            let msg = msg_opt.expect("Expected message, got None");

            match msg {
                Message::Application(AppMessage::Query) => {
                    let txs = vec![make_alice_transaction()];
                    println!("Received Query, sending response...");
                    send_message(writer, &Message::Application(AppMessage::Response(txs)))
                        .await
                        .unwrap();
                    println!("Response sent.");
                }
                _ => panic!("Expected AppMessage::Query, got {:?}", msg),
            }
        });

        // Client-side test logic
        let stream = TcpStream::connect(addr).await?;

        let (reader, writer) = stream.into_split();
        let writer = Arc::new(Mutex::new(writer));

        let reader = Arc::new(Mutex::new(reader));
        let txs_opt = send_query(reader, writer).await?;
        let txs = txs_opt.expect("Expected Some(transaction), got None");

        assert_eq!(txs.len(), 1);
        let first_tx = &txs[0];
        match &first_tx.tx {
            crate::types::transaction::UnsignedTransaction::Transfer(transfer_transaction) => {
                assert_eq!(transfer_transaction.from, get_alice_pk_str().to_bytes());
            }
            crate::types::transaction::UnsignedTransaction::Order(_order_transaction) => {
                panic!("Expected transaction")
            }
            crate::types::transaction::UnsignedTransaction::CancelOrder(
                _cancel_order_transaction,
            ) => {
                panic!("Expected transaction")
            }
        }

        Ok(())
    }
}
