use colored::Colorize;
use ed25519_dalek::{SigningKey, VerifyingKey};
use hex::FromHex;
use rand::rngs::OsRng;
use std::{
    io::{self, Write},
    sync::Arc,
};
use tokio::{net::tcp::OwnedWriteHalf, sync::Mutex};

use crate::{
    client::ClientConnection,
    message_protocol::{self},
    state::{
        asset::{Asset, AssetId},
        order::{LimitOrder, OrderDirection, OrderId, OrderType},
        spot_clearinghouse::{AccountBalance, MarketId},
        spot_market::{LevelInfo, MarketInfo},
    },
    types::transaction::{
        CancelOrderTransaction, OrderTransaction, PublicKeyString, TransactionStatus,
        TransferTransaction, UnsignedTransaction,
    },
};

pub struct ClientAccount {
    sk: SigningKey,
    pub(crate) _pk: VerifyingKey,
    pub(crate) pk_str: PublicKeyString,
}

impl ClientAccount {
    pub fn new(sk: SigningKey) -> Self {
        let pk = sk.verifying_key();
        let pk_str = PublicKeyString::from_public_key(&pk);
        let sk_hex: String = sk.to_bytes().iter().map(|b| format!("{:02x}", b)).collect();
        println!("{:?}", sk_hex);
        Self {
            sk,
            _pk: pk,
            pk_str,
        }
    }
}

const ANSI_ESC: &str = "\x1B[2J\x1B[1;1H";

fn prompt_confirmation() -> std::io::Result<bool> {
    print!("Confirm order? (y/n): ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn prompt_cancel_confirmation(order_id: u64) -> std::io::Result<bool> {
    print!("Cancel order with ID {}? (y/n): ", order_id);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn prompt_ack() -> std::io::Result<()> {
    println!("\nPress Enter to return...");
    io::stdout().flush()?; // ensure prompt prints before input

    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(())
}

fn format_asset_balance(amount: u128, decimals: u8) -> String {
    let float = amount as f64 / 10f64.powi(decimals as i32);
    format!("{:.2}", float)
}

/// Converts a float user input to discrete lots
fn amount_to_lots(amount: f64, lot_size: u32, decimals: u8) -> Option<u64> {
    if amount < 0.0 {
        return None;
    }

    let factor = 10u64.pow(decimals as u32);
    let raw_units = (amount * factor as f64).round() as u64;

    Some(raw_units / lot_size as u64)
}

fn lots_to_amount(lots: u64, lot_size: u32, decimals: u8) -> f64 {
    let raw_units = lots as u128 * lot_size as u128;
    raw_units as f64 / 10f64.powi(decimals as i32)
}

fn format_last_executed_price(
    last_executed_price: Option<u64>,
    tick_size: u32,
    tick_decimals: u8,
) -> String {
    match last_executed_price {
        Some(price) => format_price(price, tick_size, tick_decimals),
        None => "None".to_string(),
    }
}

fn parse_price_to_multiple(price: f64, tick_size: u32, tick_decimals: u8) -> Option<u64> {
    let factor = 10u64.pow(tick_decimals as u32) as f64;
    let raw_units = (price * factor).round() as u64;
    let tick_size = tick_size as u64;
    if !raw_units.is_multiple_of(tick_size) {
        None // not an exact multiple
    } else {
        Some(raw_units / tick_size)
    }
}

fn format_price(price_multiple: u64, tick_size: u32, tick_decimals: u8) -> String {
    let raw_price_units = price_multiple * tick_size as u64;
    let factor = 10u64.pow(tick_decimals as u32);
    let whole = raw_price_units / factor;
    let fractional = raw_price_units % factor;
    format!(
        "{}.{}",
        whole,
        format!("{:0>width$}", fractional, width = tick_decimals as usize)
    )
}

fn format_level(
    level: &Option<LevelInfo>,
    tick_size: u32,
    tick_decimals: u8,
    asset_info: &Asset,
) -> (String, String) {
    match level {
        Some(level_info) => (
            format_price(level_info.price, tick_size, tick_decimals),
            lots_to_amount(level_info.volume, asset_info.lot_size, asset_info.decimals).to_string(),
        ),
        None => ("None".to_string(), "None".to_string()),
    }
}

fn display_spot_balances(asset_infos: &Vec<Asset>, spot_balance: AccountBalance) {
    for token_balance in &spot_balance.asset_balances {
        let Some(asset) = asset_infos.get(token_balance.asset_id as usize) else {
            println!(
                "Could not find asset info for asset: {}",
                token_balance.asset_id
            );
            continue;
        };
        let total_balance = format_asset_balance(token_balance.total_balance, asset.decimals);
        let available_balance =
            format_asset_balance(token_balance.available_balance, asset.decimals);

        println!(
            "Asset: {}, Total: {}, Available: {}",
            asset.asset_name.blue(),
            total_balance.blue(),
            available_balance.blue(),
        );
    }
}

fn display_spot_markets(markets_info: &Vec<MarketInfo>) {
    println!("Spot Markets:");
    for market in markets_info {
        let market_name = market.market_name.to_string().blue();
        let market_id = format!("{}", market.market_id).blue();
        println!("{}, id: {}", market_name, market_id);
    }
}

fn display_spot_actions() {
    println!("Available actions:");
    println!("  limit buy <price> <base_amount>   - Place a limit buy order");
    println!("  limit sell <price> <base_amount>  - Place a limit sell order");
    println!("  market buy <quote_amount>          - Market buy (spend quote)");
    println!("  market sell <base_amount>        - Market sell (receive quote)");
    println!("  open                              - View your open orders");
    println!("  cancel <order-id>                 - Cancel order");
    println!("  re                                - Refresh");
    println!("  back                              - Return to previous menu");
}

fn display_market(market_info: &MarketInfo, base_asset_info: &Asset) {
    let market_name = format!("{}", market_info.market_name).blue();
    let tick_size = market_info.tick;
    let tick_decimals = market_info.tick_decimals;

    let price =
        format_last_executed_price(market_info.last_executed_price, tick_size, tick_decimals)
            .white();
    let ask_info = &market_info.best_asks_info;
    let bid_info = &market_info.best_bids_info;
    let tick_size = market_info.tick;
    let tick_decimals = market_info.tick_decimals;

    let (best_ask_price, best_ask_volume) =
        format_level(ask_info, tick_size, tick_decimals, base_asset_info);
    let (best_bid_price, best_bid_volume) =
        format_level(bid_info, tick_size, tick_decimals, base_asset_info);
    println!("Market: {}", market_name);
    println!("Price: {}", price);

    println!(
        "Best Bids: {}, Volume: {}",
        best_bid_price.green(),
        best_bid_volume.green()
    );
    println!(
        "Best Asks: {}, Volume: {}",
        best_ask_price.red(),
        best_ask_volume.red()
    );
    println!();
}

fn display_user_available_balance(
    spot_balance: AccountBalance,
    base_asset_info: &Asset,
    quote_asset_info: &Asset,
) {
    let base_available = spot_balance
        .find_asset_id(base_asset_info.asset_id)
        .map(|b| b.available_balance)
        .unwrap_or(0);

    let quote_available = spot_balance
        .find_asset_id(quote_asset_info.asset_id)
        .map(|b| b.available_balance)
        .unwrap_or(0);

    let base_amount = format_asset_balance(base_available, base_asset_info.decimals).blue();
    let quote_amount = format_asset_balance(quote_available, quote_asset_info.decimals).blue();
    println!(
        "Available: {} {} | {} {}",
        base_amount, base_asset_info.asset_name, quote_amount, quote_asset_info.asset_name
    );
    println!();
}

fn display_open_orders(
    open_orders: &Vec<LimitOrder>,
    tick_size: u32,
    tick_decimals: u8,
    base_asset_info: &Asset,
) {
    println!("Open Orders:");
    println!(
        "{:<10} {:<6} {:<15} {:<12} {:<12} {:<12}",
        "OrderID", "Side", "Price", "Size", "Filled", "Self Filled"
    );

    for order in open_orders {
        let side = match order.common.direction {
            OrderDirection::Buy => "Buy".green(),
            OrderDirection::Sell => "Sell".red(),
        };
        let lot_size = base_asset_info.lot_size;
        let decimals = base_asset_info.decimals;

        let price = format_price(order.price_multiple, tick_size, tick_decimals);
        let amount = lots_to_amount(order.base_lots, lot_size, decimals);
        let filled_amount = lots_to_amount(order.filled_base_lots, lot_size, decimals);
        let self_filled_amount = lots_to_amount(order.self_filled, lot_size, decimals);

        println!(
            "{:<10} {:<6} {:<15} {:<12} {:<12} {:<12}",
            order.common.id, side, price, amount, filled_amount, self_filled_amount,
        );
    }

    if open_orders.is_empty() {
        println!("No open orders.");
    }
}

fn handle_help() {
    println!("{}", "Commands:".green());
    println!(
        "{}   {}",
        "  create, c".blue(),
        "Creates a new public key secret key pair"
    );
    println!(
        "{}   {}",
        "  load <secret key>".blue(),
        "Loads an account from a secret key pair"
    );
    println!("{}   {}", "  drip".blue(), "Request balance from faucet");
    println!("{}", "  transfer <to> <amount>".blue());
    println!("{}", "  quit, q".blue());
}

fn handle_create() -> ClientAccount {
    let mut csprng = OsRng;

    let signing_key: SigningKey = SigningKey::generate(&mut csprng);

    let client_account = ClientAccount::new(signing_key);
    println!("Account created: {}", client_account.pk_str);
    client_account
}

async fn handle_drip(
    trimmed: &str,
    client: &Option<ClientAccount>,
    client_writer: Arc<Mutex<OwnedWriteHalf>>,
) -> std::io::Result<()> {
    let Some(client) = client else {
        println!("Please create or load an account before transferring.");
        return Ok(());
    };

    let parts: Vec<&str> = trimmed["drip ".len()..].split_whitespace().collect();
    if parts.len() != 1 {
        println!("Usage: drip <SUPE | USD>");
        return Ok(());
    }

    let asset_id = match parts[0].to_uppercase().as_str() {
        "SUPE" => 0,
        "USD" => 1,
        other => {
            println!("Unknown asset: {}", other);
            return Ok(());
        }
    };

    println!("Submitting transaction..");
    message_protocol::send_drip(client_writer, &client.pk_str.to_bytes(), asset_id).await?;
    println!("Transaction Submitted");

    Ok(())
}

fn handle_load(trimmed: &str) -> ClientAccount {
    let sk_hex = trimmed["load ".len()..].trim();
    let sk_bytes = <[u8; 32]>::from_hex(sk_hex).expect("Invalid hex");

    let signing_key = SigningKey::from_bytes(&sk_bytes);
    let client_account = ClientAccount::new(signing_key);

    println!("Account loaded: {}", client_account.pk_str);
    client_account
}

async fn handle_transfer(
    trimmed: &str,
    client: &mut Option<ClientAccount>,
    client_connection: &ClientConnection,
) -> std::io::Result<()> {
    let Some(client) = client else {
        println!("Please create or load an account before transferring.");
        return Ok(());
    };

    let parts: Vec<&str> = trimmed["transfer ".len()..].split_whitespace().collect();
    if parts.len() != 3 {
        println!("Usage: transfer <to> <asset_id> <amount>");
    }

    // Ensure that pk is valid hex
    let to_pk_bytes = <[u8; 32]>::from_hex(parts[0]).expect("Invalid hex");
    let to_pk = VerifyingKey::from_bytes(&to_pk_bytes).expect("Invalid public key bytes");
    let to_pk = PublicKeyString::from_public_key(&to_pk);

    if to_pk == client.pk_str {
        println!("Can't transfer to self");
        return Ok(());
    }
    let asset_id = parts[1]
        .parse::<AssetId>()
        .expect("To be non negative number");

    let amount = parts[2].parse::<u128>().expect("To be non negative number");

    let account_info_with_balances = message_protocol::send_account_query(
        client.pk_str.to_bytes(),
        client_connection.reader.clone(),
        client_connection.writer.clone(),
    )
    .await?;

    let account_info = account_info_with_balances.account_info;
    let token_balances = account_info_with_balances.spot_balances.asset_balances;

    let token_balance_opt = token_balances
        .iter()
        .find(|asset| asset.asset_id == asset_id);

    let Some(token_balance) = token_balance_opt else {
        println!("Insufficient balance {:?}", 0);
        return Ok(());
    };

    if amount > token_balance.available_balance {
        println!(
            "Insufficient balance: Available balance {:?}",
            token_balance.available_balance
        );
        return Ok(());
    }

    let txn = UnsignedTransaction::Transfer(TransferTransaction {
        from: client.pk_str.to_bytes(),
        to: to_pk.to_bytes(),
        amount,
        asset_id,
        nonce: account_info.expected_nonce,
        status: TransactionStatus::Pending,
    });

    let tx = txn.sign(&mut client.sk);

    message_protocol::send_transaction(client_connection.writer.clone(), tx).await?;
    return Ok(());
}

async fn handle_query(
    client: &Option<ClientAccount>,
    client_connection: &ClientConnection,
    asset_infos: &Vec<Asset>,
) -> std::io::Result<()> {
    let Some(client) = client else {
        println!("Please create or load an account before transferring.");
        return Ok(());
    };

    let account_info_with_balances = message_protocol::send_account_query(
        client.pk_str.to_bytes(),
        client_connection.reader.clone(),
        client_connection.writer.clone(),
    )
    .await?;

    let pk = format!("{}", client.pk_str).blue();
    println!("Account: {}", pk);
    display_spot_balances(asset_infos, account_info_with_balances.spot_balances);

    return Ok(());
}

async fn handle_open_orders(
    client: &ClientAccount,
    client_connection: &ClientConnection,
    tick_size: u32,
    tick_decimals: u8,
    base_asset_info: &Asset,
) -> std::io::Result<()> {
    let account_info = message_protocol::send_account_query(
        client.pk_str.to_bytes(),
        client_connection.reader.clone(),
        client_connection.writer.clone(),
    )
    .await?;

    display_open_orders(
        &account_info.account_info.open_orders,
        tick_size,
        tick_decimals,
        base_asset_info,
    );

    prompt_ack()?;

    Ok(())
}

async fn handle_limit_order(
    direction: OrderDirection,
    base_amount: u64,
    price: u64,
    client: &mut ClientAccount,
    client_connection: &ClientConnection,
    market_info: &MarketInfo,
) -> std::io::Result<()> {
    let account_info_with_balances = message_protocol::send_account_query(
        client.pk_str.to_bytes(),
        client_connection.reader.clone(),
        client_connection.writer.clone(),
    )
    .await?;

    let token_balances = account_info_with_balances.spot_balances.asset_balances;
    let account_info = account_info_with_balances.account_info;

    let base_asset = market_info.base_asset_id;
    let quote_asset = market_info.quote_asset_id;
    let market_id = market_info.market_id;

    match direction {
        OrderDirection::Buy => {
            let asset_id = quote_asset;
            let token_balance_opt = token_balances
                .iter()
                .find(|asset| asset.asset_id == asset_id);

            let Some(token_balance) = token_balance_opt else {
                println!("Insufficient balance {:?}", 0);
                return Ok(());
            };

            let quote_needed = price * base_amount;
            if quote_needed as u128 > token_balance.available_balance {
                println!(
                    "Insufficient balance: Available balance {:?}",
                    token_balance.available_balance
                );
                return Ok(());
            }
        }
        OrderDirection::Sell => {
            let asset_id = base_asset;
            let token_balance_opt = token_balances
                .iter()
                .find(|asset| asset.asset_id == asset_id);

            let Some(token_balance) = token_balance_opt else {
                println!("Insufficient balance {:?}", 0);
                return Ok(());
            };

            if base_amount as u128 > token_balance.available_balance {
                println!(
                    "Insufficient balance: Available balance {:?}",
                    token_balance.available_balance
                );
                return Ok(());
            }
        }
    }
    let order_type = OrderType::Limit(price, base_amount);
    let txn = UnsignedTransaction::Order(OrderTransaction {
        from: client.pk_str.to_bytes(),
        market_id,
        direction,
        order_type,
        status: TransactionStatus::Pending,
        nonce: account_info.expected_nonce,
    });

    let tx = txn.sign(&mut client.sk);

    println!("Submitting transaction... ");
    message_protocol::send_transaction(client_connection.writer.clone(), tx).await?;
    println!("Transaction submitted");
    prompt_ack()?;
    Ok(())
}

async fn handle_market_order(
    direction: OrderDirection,
    amount: u64,
    client: &mut ClientAccount,
    client_connection: &ClientConnection,
    market_info: &MarketInfo,
) -> std::io::Result<()> {
    let account_info_with_balances = message_protocol::send_account_query(
        client.pk_str.to_bytes(),
        client_connection.reader.clone(),
        client_connection.writer.clone(),
    )
    .await?;

    let token_balances = account_info_with_balances.spot_balances.asset_balances;
    let account_info = account_info_with_balances.account_info;

    let base_asset = market_info.base_asset_id;
    let quote_asset = market_info.quote_asset_id;
    let market_id = market_info.market_id;

    let asset_id = match direction {
        OrderDirection::Buy => quote_asset,
        OrderDirection::Sell => base_asset,
    };

    let token_balance_opt = token_balances
        .iter()
        .find(|asset| asset.asset_id == asset_id);

    let Some(token_balance) = token_balance_opt else {
        println!("Insufficient balance {:?}", 0);
        return Ok(());
    };

    if amount as u128 > token_balance.available_balance {
        println!(
            "Insufficient balance: Available balance {:?}",
            token_balance.available_balance
        );
        return Ok(());
    }

    let order_type = OrderType::Market(amount);
    let txn = UnsignedTransaction::Order(OrderTransaction {
        from: client.pk_str.to_bytes(),
        market_id,
        direction,
        order_type,
        status: TransactionStatus::Pending,
        nonce: account_info.expected_nonce,
    });

    let tx = txn.sign(&mut client.sk);

    println!("Submitting transaction... ");
    message_protocol::send_transaction(client_connection.writer.clone(), tx).await?;
    println!("Transaction submitted");
    prompt_ack()?;
    Ok(())
}

async fn handle_cancel_order(
    client: &mut ClientAccount,
    client_connection: &ClientConnection,
    order_id: OrderId,
    market_id: MarketId,
) -> std::io::Result<()> {
    let account_info_with_balances = message_protocol::send_account_query(
        client.pk_str.to_bytes(),
        client_connection.reader.clone(),
        client_connection.writer.clone(),
    )
    .await?;

    let account_info = account_info_with_balances.account_info;

    let txn = UnsignedTransaction::CancelOrder(CancelOrderTransaction {
        from: client.pk_str.to_bytes(),
        market_id,
        order_id,
        status: TransactionStatus::Pending,
        nonce: account_info.expected_nonce,
    });

    let tx = txn.sign(&mut client.sk);

    println!("Submitting transaction... ");
    message_protocol::send_transaction(client_connection.writer.clone(), tx).await?;
    println!("Transaction submitted");
    prompt_ack()?;
    Ok(())
}

async fn fetch_asset_infos(client_connection: &ClientConnection) -> std::io::Result<Vec<Asset>> {
    message_protocol::send_assets_query(
        client_connection.reader.clone(),
        client_connection.writer.clone(),
    )
    .await
}

async fn handle_market(
    client: &mut ClientAccount,
    connection: &ClientConnection,
    asset_infos: &Vec<Asset>,
    market_id: MarketId,
) -> std::io::Result<()> {
    loop {
        print!("{}", ANSI_ESC);
        let market_info = message_protocol::send_market_info_query(
            market_id,
            connection.reader.clone(),
            connection.writer.clone(),
        )
        .await?;

        let account_info_with_balances = message_protocol::send_account_query(
            client.pk_str.to_bytes(),
            connection.reader.clone(),
            connection.writer.clone(),
        )
        .await?;

        let Some(market_info) = market_info else {
            println!("Error fetching market data");
            return Ok(());
        };

        let base_asset = market_info.base_asset_id;
        let quote_asset = market_info.quote_asset_id;

        let Some(base_asset_info) = asset_infos.get(base_asset as usize) else {
            println!("Error fetching Asset data for {}", base_asset);
            return Ok(());
        };

        let Some(quote_asset_info) = asset_infos.get(quote_asset as usize) else {
            println!("Error fetching Asset data for {}", quote_asset);
            return Ok(());
        };

        let tick_size = &market_info.tick;
        let tick_decimals = &market_info.tick_decimals;
        let base_name = &market_info.base_name;
        let quote_name = &market_info.quote_name;

        display_market(&market_info, base_asset_info);
        display_user_available_balance(
            account_info_with_balances.spot_balances,
            base_asset_info,
            quote_asset_info,
        );

        display_spot_actions();
        print!("> ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let trimmed = input.trim();
        let parts: Vec<&str> = trimmed.split_whitespace().collect();

        match parts.as_slice() {
            ["back"] => {
                println!("Returning to previous menu.");
                return Ok(());
            }

            ["open"] => {
                handle_open_orders(
                    client,
                    connection,
                    *tick_size,
                    *tick_decimals,
                    base_asset_info,
                )
                .await?;
            }

            ["re"] => {
                continue;
            }

            ["limit", "buy", price, base_amount] => {
                let raw_amount = base_amount.parse::<f64>().unwrap_or(0.0);
                let raw_price = price.parse::<f64>().unwrap_or(0.0);

                let lot_size = base_asset_info.lot_size;
                let decimals = base_asset_info.decimals;

                let amount = amount_to_lots(raw_amount, lot_size, decimals);
                let Some(amount) = amount else {
                    println!("Invalid base amount");
                    continue;
                };

                if amount == 0 {
                    println!("Invalid base amount.");
                    continue;
                }

                let price = parse_price_to_multiple(raw_price, *tick_size, *tick_decimals);

                let Some(price) = price else {
                    println!("Invalid base amount");
                    continue;
                };

                if price == 0 {
                    println!("Invalid price.");
                    continue;
                }

                println!(
                    "Limit Buy {} {} at price: {}",
                    raw_amount, base_name, raw_price
                );

                if prompt_confirmation()? {
                    handle_limit_order(
                        OrderDirection::Buy,
                        amount,
                        price,
                        client,
                        connection,
                        &market_info,
                    )
                    .await?;
                } else {
                    println!("Order not submitted.");
                }
            }

            ["limit", "sell", price, base_amount] => {
                let raw_amount = base_amount.parse::<f64>().unwrap_or(0.0);
                let raw_price = price.parse::<f64>().unwrap_or(0.0);

                let lot_size = base_asset_info.lot_size;
                let decimals = base_asset_info.decimals;

                let amount = amount_to_lots(raw_amount, lot_size, decimals);
                let Some(amount) = amount else {
                    println!("Invalid base amount");
                    continue;
                };

                if amount == 0 {
                    println!("Invalid base amount.");
                    continue;
                }

                let price = parse_price_to_multiple(raw_price, *tick_size, *tick_decimals);

                let Some(price) = price else {
                    println!("Invalid base amount");
                    continue;
                };

                if price == 0 {
                    println!("Invalid price.");
                    continue;
                }

                println!(
                    "Limit Sell {} {} at price: {}",
                    raw_amount, base_name, raw_price
                );

                if prompt_confirmation()? {
                    handle_limit_order(
                        OrderDirection::Sell,
                        amount,
                        price,
                        client,
                        connection,
                        &market_info,
                    )
                    .await?;
                } else {
                    println!("Order not submitted.");
                }
            }

            ["market", "buy", quote_amount] => {
                let raw_amount = quote_amount.parse::<f64>().unwrap_or(0.0);

                let lot_size = quote_asset_info.lot_size;
                let decimals = quote_asset_info.decimals;
                let amount = amount_to_lots(raw_amount, lot_size, decimals);
                let Some(amount) = amount else {
                    println!("Invalid amount");
                    continue;
                };

                if amount == 0 {
                    println!("Invalid amount.");
                    continue;
                }

                println!("Market Buy: Spend {}  {}", raw_amount, quote_name);

                if prompt_confirmation()? {
                    handle_market_order(
                        OrderDirection::Buy,
                        amount,
                        client,
                        connection,
                        &market_info,
                    )
                    .await?;
                } else {
                    println!("Order not submitted.");
                }
            }

            ["market", "sell", base_amount] => {
                let raw_amount = base_amount.parse::<f64>().unwrap_or(0.0);

                let lot_size = base_asset_info.lot_size;
                let decimals = base_asset_info.decimals;
                let amount = amount_to_lots(raw_amount, lot_size, decimals);
                let Some(amount) = amount else {
                    println!("Invalid amount");
                    continue;
                };

                if amount == 0 {
                    println!("Invalid amount.");
                    continue;
                }

                println!("Market Sell: Spend {} {}", raw_amount, base_name);

                if prompt_confirmation()? {
                    handle_market_order(
                        OrderDirection::Sell,
                        amount,
                        client,
                        connection,
                        &market_info,
                    )
                    .await?;
                } else {
                    println!("Order not submitted.");
                }
            }

            ["cancel", order_id_str] => {
                let order_id = order_id_str.parse::<u64>().unwrap_or(0);

                if prompt_cancel_confirmation(order_id)? {
                    handle_cancel_order(client, connection, order_id, market_info.market_id)
                        .await?;
                } else {
                    println!("Cancel aborted.");
                }
            }

            _ => {
                println!(
                    "Unrecognized command. Type 'back', 'open_orders', or a valid order command."
                );
            }
        }
    }
}

async fn handle_markets(
    client: &mut Option<ClientAccount>,
    client_connection: &ClientConnection,
    asset_infos: &Vec<Asset>,
) -> std::io::Result<()> {
    let Some(client) = client else {
        println!("Please create or load an account.");
        return Ok(());
    };

    let markets_info = message_protocol::send_markets_query(
        client_connection.reader.clone(),
        client_connection.writer.clone(),
    )
    .await?;

    loop {
        display_spot_markets(&markets_info);
        println!("Enter market ID to select, or 'back' to return:");

        print!("> ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let trimmed = input.trim();

        if trimmed.eq_ignore_ascii_case("back") {
            println!("Returning to previous menu.");
            return Ok(());
        }

        match trimmed.parse::<usize>() {
            Ok(id) => {
                if let Some(market) = markets_info.iter().find(|m| m.market_id == id) {
                    print!("{}", ANSI_ESC);
                    handle_market(client, client_connection, asset_infos, market.market_id).await?;
                    // You can now call a handler like `handle_market_detail(market)`
                } else {
                    println!("No market with ID {}. Try again.", id);
                }
            }
            Err(_) => println!("Invalid input. Enter a numeric ID or 'back'."),
        }
    }
}

pub async fn run_console(addr: &str) -> std::io::Result<()> {
    print!("{}", ANSI_ESC);
    println!("Superliquid Client Console");
    println!("Type `help` to see commands.");
    let connection = ClientConnection::create_client_connection(addr).await?;
    let mut client_account = None;
    let asset_infos = fetch_asset_infos(&connection).await?;
    loop {
        print!("> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        let trimmed = input.trim();

        match trimmed {
            "help" => handle_help(),
            "create" | "c" => {
                client_account = Some(handle_create());
            }
            _ if trimmed.starts_with("load ") => client_account = Some(handle_load(trimmed)),
            _ if trimmed.starts_with("drip ") => {
                handle_drip(trimmed, &client_account, connection.writer.clone()).await?;
            }
            "query" => handle_query(&client_account, &connection, &asset_infos).await?,
            _ if trimmed.starts_with("transfer ") => {
                handle_transfer(trimmed, &mut client_account, &connection).await?
            }
            "markets" => handle_markets(&mut client_account, &connection, &asset_infos).await?,
            "quit" | "q" => return Ok(()),
            _ => println!("Unknown command. Type `help` for options."),
        }
    }
}
