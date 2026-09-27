use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{
    config,
    hotstuff::block::Block,
    node::client::handler::{ClientQuery, ClientResponse},
    types::transaction::{
        CancelOrderTransaction, OrderTransaction, PublicKeyHash, PublicKeyString,
        SignedTransaction, TransactionStatus, TransferTransaction, UnsignedTransaction,
    },
};

use super::{
    asset::{Asset, AssetId, AssetManager},
    order::{
        self, ExecutionResults, LimitOrder, MarketOrder, Order, OrderDirection, OrderId,
        OrderStateManager, OrderStatus, ResidualOrder,
    },
    spot_clearinghouse::{
        AccountBalance, AccountTokenBalance, MarketId, MarketPrecision, SpotClearingHouse,
    },
    spot_market::MarketInfo,
    transaction_delta::{AssetDelta, TransferDelta},
};

pub type Balance = u128;
pub type Nonce = u64;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AccountInfo {
    pub expected_nonce: Nonce,
    pub open_orders: Vec<LimitOrder>, // sorted by orderId
    pub completed_orders: Vec<Order>, // sorted by completion
    _private: (),                     // prevent creation of accountinfo outside of this struct
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AccountInfoWithBalances {
    pub account_info: AccountInfo,
    pub spot_balances: AccountBalance,
}

pub struct AccountInfoWithBalancesRef<'a> {
    pub account_info: &'a AccountInfo,
    pub spot_balances: &'a AccountBalance,
}

impl<'a> From<AccountInfoWithBalancesRef<'a>> for AccountInfoWithBalances {
    fn from(r: AccountInfoWithBalancesRef<'a>) -> Self {
        Self {
            account_info: r.account_info.clone(),
            spot_balances: r.spot_balances.clone(),
        }
    }
}

impl AccountInfo {
    pub(crate) fn new() -> Self {
        Self {
            expected_nonce: 0,
            open_orders: vec![],
            completed_orders: vec![],
            _private: (),
        }
    }

    fn create_faucet() -> Self {
        Self {
            expected_nonce: 0,
            open_orders: vec![],
            completed_orders: vec![],
            _private: (),
        }
    }

    fn get_open_order(&self, order_id: OrderId) -> Option<&LimitOrder> {
        self.open_orders
            .iter()
            .find(|&order| order.common.id == order_id)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub enum Resource {
    Market(MarketId),
    Asset(AssetId),
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub enum ExecError {
    InsufficientFunds {
        from: PublicKeyString,
        have: u128,
        need: u128,
    },
    ResourceNotFound(Resource),
}

pub struct LedgerState {
    pub accounts: HashMap<PublicKeyHash, AccountInfo>,
    pub asset_manager: AssetManager,
    pub order_manager: OrderStateManager,
    pub spot_clearinghouse: SpotClearingHouse,
    pub perps_clearinghouse: (),
}

impl LedgerState {
    pub(crate) fn new() -> Self {
        let (pk, _) = config::retrieve_faucet_keys();
        let mut accounts: HashMap<PublicKeyHash, AccountInfo> = HashMap::new();
        accounts.insert(pk.to_bytes(), AccountInfo::create_faucet());
        let asset_manager = AssetManager::new();

        let asset_0 = asset_manager.assets.get(0).expect("Asset 0 to be present");
        let asset_1 = asset_manager.assets.get(1).expect("Asset 1 to be present");

        let mut spot_clearinghouse = SpotClearingHouse::new();
        spot_clearinghouse.add_faucet_account();

        spot_clearinghouse.add_market(
            0,
            1,
            asset_0.asset_name.clone(),
            asset_1.asset_name.clone(),
            100,
            3,
        );

        LedgerState {
            accounts,
            asset_manager: AssetManager::new(),
            order_manager: OrderStateManager::new(),
            spot_clearinghouse: spot_clearinghouse,
            perps_clearinghouse: (),
        }
    }

    pub fn get_asset_info(&self) -> Vec<Asset> {
        self.asset_manager.assets.clone()
    }

    pub fn get_market_info(&self, market_id: MarketId) -> Option<MarketInfo> {
        self.spot_clearinghouse.get_market_info_from_id(market_id)
    }

    pub fn get_markets(&self) -> Vec<MarketInfo> {
        self.spot_clearinghouse.get_markets()
    }

    pub(crate) fn get_account_info_with_balances_or_default(
        &self,
        public_key: &PublicKeyHash,
    ) -> AccountInfoWithBalances {
        let account_opt = self.get_account_info_with_balances(public_key);
        let Some(account_info) = account_opt else {
            return AccountInfoWithBalances::default();
        };

        account_info.into()
    }

    pub(crate) fn get_account_info_with_balances(
        &self,
        public_key: &PublicKeyHash,
    ) -> Option<AccountInfoWithBalancesRef> {
        let account_info = self.accounts.get(public_key)?;
        let account_balances = self.spot_clearinghouse.get_account_balance(public_key)?;

        Some(AccountInfoWithBalancesRef {
            account_info,
            spot_balances: account_balances,
        })
    }

    pub(crate) fn get_account_info_or_default(&self, public_key: &PublicKeyHash) -> AccountInfo {
        self.accounts.get(public_key).cloned().unwrap_or_default()
    }

    // retrieves account info by public key, creates one if one doesn't exist
    pub(crate) fn get_account_info_mut(&mut self, public_key: &PublicKeyHash) -> &mut AccountInfo {
        self.accounts
            .entry(*public_key)
            .or_insert_with(|| AccountInfo::new())
    }

    pub(crate) fn get_account_spot_balances_mut(
        &mut self,
        public_key: &PublicKeyHash,
    ) -> &mut AccountBalance {
        self.spot_clearinghouse.get_account_balance_mut(public_key)
    }

    fn get_order_position_from_open_orders(
        account_info: &mut AccountInfo,
        order_id: OrderId,
    ) -> Option<usize> {
        account_info
            .open_orders
            .iter()
            .position(|order| order.common.id == order_id)
    }

    fn is_self_cross(
        open_orders: &Vec<LimitOrder>,
        curr_direction: &OrderDirection,
        curr_price: u64,
    ) -> bool {
        match curr_direction {
            OrderDirection::Buy => open_orders.iter().any(|order| {
                order.common.direction == OrderDirection::Sell && curr_price >= order.price_multiple
            }),
            OrderDirection::Sell => open_orders.iter().any(|order| {
                order.common.direction == OrderDirection::Buy && curr_price <= order.price_multiple
            }),
        }
    }

    fn prepare_transfer_transaction(
        &mut self,
        transaction: &TransferTransaction,
    ) -> Result<TransferDelta, ExecError> {
        let from_account_balances = self.get_account_spot_balances_mut(&transaction.from);

        let from = PublicKeyString::from_bytes(transaction.from);

        let from_token_balance_opt = from_account_balances.find_asset_id(transaction.asset_id);

        let Some(from_token_balance) = from_token_balance_opt else {
            return Err(ExecError::InsufficientFunds {
                from,
                have: 0,
                need: transaction.amount,
            });
        };

        if from_token_balance.available_balance < transaction.amount {
            return Err(ExecError::InsufficientFunds {
                from,
                have: from_token_balance.available_balance,
                need: transaction.amount,
            });
        }

        let asset_out = AssetDelta {
            account: transaction.from,
            asset_id: transaction.asset_id,
            amount: transaction.amount,
            is_increase: false,
        };

        // Create account info if not created
        {
            self.get_account_info_mut(&transaction.to);
        }

        let asset_in = AssetDelta {
            account: transaction.to,
            asset_id: transaction.asset_id,
            amount: transaction.amount,
            is_increase: true,
        };

        Ok(TransferDelta {
            asset_in,
            asset_out,
            nonce_delta: transaction.from,
        })
    }

    fn commit_transfer_transaction(&mut self, delta: TransferDelta) -> Nonce {
        let TransferDelta {
            asset_in,
            asset_out,
            nonce_delta,
        } = delta;

        let from_account_info = self.get_account_info_mut(&nonce_delta);
        from_account_info.expected_nonce += 1;
        let expected_nonce = from_account_info.expected_nonce;

        let from_account_balances = self.get_account_spot_balances_mut(&asset_out.account);

        let from_token_balance =
            &mut from_account_balances.asset_balances[asset_out.asset_id as usize];

        from_token_balance.available_balance -= asset_out.amount;

        let to_account_balances = self.get_account_spot_balances_mut(&asset_in.account);

        let to_token_balance_opt = to_account_balances
            .asset_balances
            .iter_mut()
            .find(|a| a.asset_id == asset_in.asset_id);

        match to_token_balance_opt {
            Some(account_balance) => {
                account_balance.total_balance += asset_in.amount;
                account_balance.available_balance += asset_in.amount;
            }

            None => to_account_balances
                .asset_balances
                .push(AccountTokenBalance {
                    asset_id: asset_in.asset_id,
                    available_balance: asset_in.amount,
                    total_balance: asset_in.amount,
                }),
        }
        expected_nonce
    }

    pub(crate) fn handle_order_transaction(
        &mut self,
        transaction: &mut OrderTransaction,
    ) -> Option<(PublicKeyHash, Nonce)> {
        let market_id = transaction.market_id;
        let user_account = transaction.from;
        let direction = transaction.direction.clone();
        let order_type = transaction.order_type.clone();
        let nonce = transaction.nonce;

        // check nonce
        let from_account_info = self.get_account_info_mut(&transaction.from);
        if nonce < from_account_info.expected_nonce {
            transaction.status = TransactionStatus::Rejected("Duplicate nonce".to_string());
            return None;
        }

        if nonce > from_account_info.expected_nonce {
            transaction.status = TransactionStatus::Rejected("Out of order nonce".to_string());
            return None;
        }

        let order = match order_type {
            order::OrderType::Limit(price, quote_size) => {
                if Self::is_self_cross(&from_account_info.open_orders, &direction, price) {
                    transaction.status = TransactionStatus::Rejected("Self Cross".to_string());
                    return None;
                }

                let order = self.order_manager.new_limit_order(
                    market_id,
                    user_account,
                    direction,
                    price,
                    quote_size,
                );
                let account_info = self.get_account_info_mut(&user_account);
                account_info.open_orders.push(order.clone());
                Order::Limit(order)
            }
            order::OrderType::Market(order_size) => {
                let order = self.order_manager.new_market_order(
                    market_id,
                    user_account,
                    direction,
                    order_size,
                );

                Order::Market(order)
            }
        };
        let Some((quote_asset, base_asset, tick, tick_decimals)) = self
            .spot_clearinghouse
            .get_quote_base_tick_from_id(market_id)
        else {
            transaction.status =
                TransactionStatus::Error(ExecError::ResourceNotFound(Resource::Market(market_id)));
            return None;
        };
        let Some(quote_asset) = self.asset_manager.assets.get(quote_asset as usize) else {
            transaction.status =
                TransactionStatus::Error(ExecError::ResourceNotFound(Resource::Asset(quote_asset)));
            return None;
        };
        let Some(base_asset) = self.asset_manager.assets.get(base_asset as usize) else {
            transaction.status =
                TransactionStatus::Error(ExecError::ResourceNotFound(Resource::Asset(base_asset)));
            return None;
        };

        let precision = MarketPrecision {
            base_lot_size: base_asset.lot_size,
            quote_lot_size: quote_asset.lot_size,
            tick,
            tick_decimals,
        };

        // Transaction should be atomic here
        let result = self
            .spot_clearinghouse
            .handle_order(order.clone(), &precision);

        // Update changes to respective account infos
        match result {
            Some(result) => {
                let ExecutionResults {
                    filled_orders,
                    residual_order,
                    user_order_change,
                } = result;

                match user_order_change {
                    Some(order_change) => match order_change {
                        order::OrderChange::LimitOrderChange {
                            order_id,
                            filled_lots: filled_amount,
                            average_execution_price: _,
                        } => {
                            let account_info = self.get_account_info_mut(&user_account);
                            let limit_order_index =
                                Self::get_order_position_from_open_orders(account_info, order_id)
                                    .expect("No open order with order_id");

                            let limit_order = account_info
                                .open_orders
                                .get_mut(limit_order_index)
                                .expect("No open order with order_id");

                            let remaining_size = limit_order.get_order_remaining();

                            if filled_amount < remaining_size {
                                limit_order.common.status = OrderStatus::PartiallyFilled;
                                limit_order.filled_base_lots += filled_amount;
                            } else {
                                // fulled filled
                                limit_order.common.status = OrderStatus::Filled;
                                let limit_order =
                                    account_info.open_orders.remove(limit_order_index);
                                account_info
                                    .completed_orders
                                    .push(Order::Limit(limit_order));
                            }
                        }
                        order::OrderChange::MarketOrderChange {
                            order_id: _,
                            filled_lots,
                            average_execution_price,
                            self_fill,
                        } => match order {
                            Order::Market(MarketOrder::Buy(mut order)) => {
                                if filled_lots < order.quote_size {
                                    order.common.status = OrderStatus::PartiallyFilled;
                                } else {
                                    order.common.status = OrderStatus::Filled;
                                }
                                order.filled_size = filled_lots;
                                order.average_execution_price = average_execution_price;
                                order.self_filled = self_fill;

                                let account_info = self.get_account_info_mut(&order.common.account);

                                account_info
                                    .completed_orders
                                    .push(Order::Market(MarketOrder::Buy(order)));
                            }
                            Order::Market(MarketOrder::Sell(mut order)) => {
                                if filled_lots < order.base_size {
                                    order.common.status = OrderStatus::PartiallyFilled;
                                } else {
                                    order.common.status = OrderStatus::Filled;
                                }
                                order.filled_size = filled_lots;
                                order.average_execution_price = average_execution_price;
                                order.self_filled = self_fill;

                                let account_info = self.get_account_info_mut(&order.common.account);

                                account_info
                                    .completed_orders
                                    .push(Order::Market(MarketOrder::Sell(order)));
                            }
                            _ => {}
                        },
                    },
                    None => {}
                }

                // Update filled orders
                for filled_order in filled_orders.iter() {
                    let order_account = self.get_account_info_mut(&filled_order.common.account);

                    let removed = Self::get_order_position_from_open_orders(
                        order_account,
                        filled_order.common.id,
                    )
                    .map(|i| order_account.open_orders.remove(i));

                    let Some(mut removed) = removed else {
                        continue;
                    };

                    removed.common.status = OrderStatus::Filled;
                    removed.self_filled = filled_order.self_filled;
                    removed.filled_base_lots = removed.base_lots - removed.self_filled;
                    order_account.completed_orders.push(Order::Limit(removed));
                }

                match residual_order {
                    Some(residual_order) => {
                        let ResidualOrder {
                            order_id,
                            account_public_key,
                            filled_base_lots,
                            self_fill,
                            ..
                        } = residual_order;

                        let order_account = self.get_account_info_mut(&account_public_key);
                        let order = order_account
                            .open_orders
                            .iter_mut()
                            .find(|order| order.common.id == order_id)
                            .expect("Cant find open order with order id");
                        order.filled_base_lots += filled_base_lots;
                        order.self_filled += self_fill;
                    }
                    None => {
                        // do nothing
                    }
                }
            }
            None => {
                // do nothing
            }
        }
        let account = self.get_account_info_mut(&user_account);
        account.expected_nonce += 1;
        transaction.status = TransactionStatus::Executed;

        Some((user_account, account.expected_nonce))
    }

    pub(crate) fn handle_cancel_order_transaction(
        &mut self,
        transaction: &mut CancelOrderTransaction,
    ) -> Option<(PublicKeyHash, Nonce)> {
        let market_id = transaction.market_id;
        let user_account = transaction.from;
        let nonce = transaction.nonce;

        // check nonce
        let from_account_info = self.get_account_info_or_default(&transaction.from);
        // todo should change the clone
        if nonce < from_account_info.expected_nonce {
            transaction.status = TransactionStatus::Rejected("Duplicate nonce".to_string());
            return None;
        }

        if nonce > from_account_info.expected_nonce {
            transaction.status = TransactionStatus::Rejected("Out of order nonce".to_string());
            return None;
        }

        let order = from_account_info.get_open_order(transaction.order_id);

        let Some(order) = order else {
            transaction.status =
                TransactionStatus::Error(ExecError::ResourceNotFound(Resource::Market(market_id)));
            return None;
        };

        let Some((quote_asset, base_asset, tick, tick_decimals)) = self
            .spot_clearinghouse
            .get_quote_base_tick_from_id(market_id)
        else {
            transaction.status =
                TransactionStatus::Error(ExecError::ResourceNotFound(Resource::Market(market_id)));
            return None;
        };
        let Some(quote_asset) = self.asset_manager.assets.get(quote_asset as usize) else {
            transaction.status =
                TransactionStatus::Error(ExecError::ResourceNotFound(Resource::Asset(quote_asset)));
            return None;
        };
        let Some(base_asset) = self.asset_manager.assets.get(base_asset as usize) else {
            transaction.status =
                TransactionStatus::Error(ExecError::ResourceNotFound(Resource::Asset(base_asset)));
            return None;
        };

        let precision = MarketPrecision {
            base_lot_size: base_asset.lot_size,
            quote_lot_size: quote_asset.lot_size,
            tick,
            tick_decimals,
        };

        if self.spot_clearinghouse.cancel_order(order, &precision) {
            let account = self.get_account_info_mut(&user_account);
            let pos = account
                .open_orders
                .iter()
                .position(|o| o.common.id == order.common.id)?;
            account.open_orders.remove(pos);
            account.completed_orders.push(Order::Limit(order.clone()));
        };

        let account = self.get_account_info_mut(&user_account);
        account.expected_nonce += 1;
        transaction.status = TransactionStatus::Executed;

        Some((user_account, account.expected_nonce))
    }

    pub(crate) fn handle_transfer_transaction(
        &mut self,
        transaction: &mut TransferTransaction,
    ) -> Option<(PublicKeyHash, Nonce)> {
        let from_account_info = self.get_account_info_mut(&transaction.from);

        if transaction.nonce < from_account_info.expected_nonce {
            transaction.status = TransactionStatus::Rejected("Duplicate Nonce".to_string());
            return None;
        }

        if transaction.nonce > from_account_info.expected_nonce {
            transaction.status = TransactionStatus::Rejected("Out of order nonce".to_string());
            return None;
        }

        let res = self.prepare_transfer_transaction(transaction);
        match res {
            Ok(delta) => {
                let expected_nonce = self.commit_transfer_transaction(delta);
                transaction.status = TransactionStatus::Executed;
                Some((transaction.from, expected_nonce))
            }
            Err(err) => {
                transaction.status = TransactionStatus::Error(err);
                None
            }
        }
    }

    pub(crate) fn apply(
        &mut self,
        transactions: &mut Vec<SignedTransaction>,
    ) -> Vec<Option<(PublicKeyHash, Nonce)>> {
        let mut account_nonces: Vec<Option<(PublicKeyHash, Nonce)>> = vec![];

        for transaction in transactions.iter_mut() {
            match &mut transaction.tx {
                UnsignedTransaction::Transfer(transfer_transaction) => {
                    account_nonces.push(self.handle_transfer_transaction(transfer_transaction))
                }
                UnsignedTransaction::Order(order_transaction) => {
                    account_nonces.push(self.handle_order_transaction(order_transaction))
                }
                UnsignedTransaction::CancelOrder(cancel_order_transaction) => {
                    account_nonces
                        .push(self.handle_cancel_order_transaction(cancel_order_transaction));
                }
            }
        }
        account_nonces
    }

    pub(crate) fn apply_block(&mut self, block: &mut Block) -> Vec<Option<(PublicKeyHash, Nonce)>> {
        self.apply(block.transactions_mut())
    }

    pub fn handle_query(&self, query: ClientQuery) -> ClientResponse {
        match query {
            crate::node::client::handler::ClientQuery::AccountQuery(public_key) => {
                let account_info_with_balances =
                    self.get_account_info_with_balances_or_default(&public_key);
                ClientResponse::AccountQueryReponse(account_info_with_balances)
            }
            crate::node::client::handler::ClientQuery::AssetQuery => {
                let asset_info = self.get_asset_info();
                ClientResponse::AssetQueryResponse(asset_info)
            }
            crate::node::client::handler::ClientQuery::MarketInfoQuery(market_id) => {
                let market_info = self.get_market_info(market_id);
                ClientResponse::MarketInfoQueryResponse(market_info)
            }
            crate::node::client::handler::ClientQuery::MarketsQuery => {
                let market_infos = self.get_markets();
                ClientResponse::MarketsQueryResponse(market_infos)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    mod test_spot_clearinghouse {
        use ed25519_dalek::SigningKey;

        use crate::{
            config,
            hotstuff::{block::Block, crypto::QuorumCertificate},
            state::{
                order::{Order, OrderDirection, OrderId, OrderStatus, OrderType},
                spot_clearinghouse::{MarketId, MarketPrecision},
                state::{AccountInfo, LedgerState, Nonce},
            },
            types::transaction::{
                CancelOrderTransaction, OrderTransaction, PublicKeyHash, SignedTransaction,
                TransactionStatus, TransferTransaction, UnsignedTransaction,
            },
            utils::test_utils::test_helpers::{get_alice_sk, get_bob_sk, get_carol_sk},
        };

        fn create_faucet_txn(
            faucet_sk: &mut SigningKey,
            to: PublicKeyHash,
            asset_id: u32,
            amount: u128,
            nonce: Nonce,
        ) -> SignedTransaction {
            let pk = faucet_sk.verifying_key().to_bytes();
            let unsigned = UnsignedTransaction::Transfer(TransferTransaction {
                from: pk,
                to,
                amount,
                asset_id,
                nonce,
                status: TransactionStatus::Pending,
            });
            unsigned.sign(faucet_sk)
        }

        fn _create_transfer_txn(
            sk: &mut SigningKey,
            to: PublicKeyHash,
            amount: u128,
            asset_id: u32,
            nonce: Nonce,
        ) -> SignedTransaction {
            let binding = sk.verifying_key();
            let pk = binding.as_bytes();
            let unsigned = UnsignedTransaction::Transfer(TransferTransaction {
                from: *pk,
                to,
                amount,
                asset_id,
                nonce,
                status: TransactionStatus::Pending,
            });
            unsigned.sign(sk)
        }

        fn create_order_txn(
            sk: &mut SigningKey,
            market_id: MarketId,
            direction: OrderDirection,
            order_type: OrderType,
            nonce: Nonce,
        ) -> SignedTransaction {
            let binding = sk.verifying_key();
            let pk = binding.as_bytes();
            let unsigned = UnsignedTransaction::Order(OrderTransaction {
                from: *pk,
                market_id,
                direction,
                order_type,
                status: TransactionStatus::Pending,
                nonce,
            });
            unsigned.sign(sk)
        }

        fn create_cancel_txn(
            sk: &mut SigningKey,
            market_id: MarketId,
            order_id: u64,
            nonce: Nonce,
        ) -> SignedTransaction {
            let binding = sk.verifying_key();
            let pk = binding.as_bytes();
            let unsigned = UnsignedTransaction::CancelOrder(CancelOrderTransaction {
                from: *pk,
                market_id,
                order_id,
                status: TransactionStatus::Pending,
                nonce,
            });
            unsigned.sign(sk)
        }

        fn create_block(transactions: Vec<SignedTransaction>) -> Block {
            Block::Normal {
                parent_id: [0; 32],
                transactions,
                view_number: 0,
                justify: QuorumCertificate::mock(0),
                merkle_root: [0; 32],
            }
        }

        fn assert_open_order(
            account_info: &AccountInfo,
            order_id: OrderId,
            filled_base_lots: u64,
            self_filled: u64,
        ) {
            let order = account_info.get_open_order(order_id).unwrap();
            assert_eq!(order.filled_base_lots, filled_base_lots);
            assert_eq!(order.self_filled, self_filled);
            assert_eq!(order.common.status, OrderStatus::Open);
        }

        fn assert_completed_limit_order(
            order: &Order,
            order_id: OrderId,
            filled_base_lots: u64,
            self_filled: u64,
        ) {
            match order {
                crate::state::order::Order::Limit(limit_order) => {
                    assert_eq!(limit_order.filled_base_lots, filled_base_lots);
                    assert_eq!(limit_order.self_filled, self_filled);
                    assert_eq!(limit_order.common.id, order_id);
                }
                crate::state::order::Order::Market(_market_order) => {
                    panic!("Expected limit order")
                }
            }
        }

        fn assert_completed_market_buy(
            order: &Order,
            order_id: OrderId,
            filled_size: u64,
            self_filled: u64,
            quote_size: u64,
        ) {
            match order {
                crate::state::order::Order::Limit(_) => {
                    panic!("Expected market order")
                }
                crate::state::order::Order::Market(market_order) => match market_order {
                    crate::state::order::MarketOrder::Sell(_) => {
                        panic!("Expect market buy")
                    }
                    crate::state::order::MarketOrder::Buy(market_buy_order) => {
                        assert_eq!(market_buy_order.filled_size, filled_size);
                        assert_eq!(market_buy_order.common.id, order_id);
                        assert_eq!(market_buy_order.self_filled, self_filled);
                        assert_eq!(market_buy_order.quote_size, quote_size);
                    }
                },
            }
        }

        fn assert_completed_market_sell(
            order: &Order,
            order_id: OrderId,
            filled_size: u64,
            self_filled: u64,
            base_size: u64,
        ) {
            match order {
                crate::state::order::Order::Limit(_) => {
                    panic!("Expected market order")
                }
                crate::state::order::Order::Market(market_order) => match market_order {
                    crate::state::order::MarketOrder::Sell(market_sell_order) => {
                        assert_eq!(market_sell_order.filled_size, filled_size);
                        assert_eq!(market_sell_order.common.id, order_id);
                        assert_eq!(market_sell_order.self_filled, self_filled);
                        assert_eq!(market_sell_order.base_size, base_size);
                    }
                    crate::state::order::MarketOrder::Buy(_) => {
                        panic!("Expect market sell")
                    }
                },
            }
        }

        fn test_setup() -> LedgerState {
            // Setup
            let mut ledger_state = LedgerState::new();
            let base = 0;
            let quote = 1;
            let base_asset_name = "".to_string();
            let quote_asset_name = "".to_string();

            let tick = 100;
            let tick_decimals = 2;
            let _precision = MarketPrecision {
                base_lot_size: 100,
                quote_lot_size: 100,
                tick: tick,
                tick_decimals: tick_decimals,
            };
            let market_id = 0;
            const DEFAULT_BASE: u128 = 1_000_000_000;
            const DEFAULT_QUOTE: u128 = 1_000_000_000_000;

            ledger_state.spot_clearinghouse.add_market(
                base,
                quote,
                base_asset_name,
                quote_asset_name,
                tick,
                tick_decimals,
            );

            let user_sk = get_alice_sk();
            let mut mm_1_sk = get_bob_sk();
            let mut mm_2_sk = get_carol_sk();

            let (_faucet_pk, mut faucet_sk) = config::retrieve_faucet_keys();

            let user_pk = user_sk.verifying_key().to_bytes();
            let mm_1_pk = mm_1_sk.verifying_key().to_bytes();
            let mm_2_pk = mm_2_sk.verifying_key().to_bytes();

            let mut mm_1_nonce = 0;
            let mut mm_2_nonce = 0;

            // Drip assets to users
            {
                let user_base_drip_tx =
                    create_faucet_txn(&mut faucet_sk, user_pk, base, DEFAULT_BASE, 0);
                let user_quote_drip_tx =
                    create_faucet_txn(&mut faucet_sk, user_pk, quote, DEFAULT_QUOTE, 1);
                let mm_1_base_drip_tx =
                    create_faucet_txn(&mut faucet_sk, mm_1_pk, base, DEFAULT_BASE, 2);
                let mm_1_quote_drip_tx =
                    create_faucet_txn(&mut faucet_sk, mm_1_pk, quote, DEFAULT_QUOTE, 3);
                let mm_2_base_drip_tx =
                    create_faucet_txn(&mut faucet_sk, mm_2_pk, base, DEFAULT_BASE, 4);
                let mm_2_quote_drip_tx =
                    create_faucet_txn(&mut faucet_sk, mm_2_pk, quote, DEFAULT_QUOTE, 5);

                let mut block_1 = create_block(vec![user_base_drip_tx]);
                let mut block_2 = create_block(vec![user_quote_drip_tx]);
                let mut block_3 = create_block(vec![mm_1_base_drip_tx]);
                let mut block_4 = create_block(vec![mm_1_quote_drip_tx]);
                let mut block_5 = create_block(vec![mm_2_base_drip_tx]);
                let mut block_6 = create_block(vec![mm_2_quote_drip_tx]);

                ledger_state.apply_block(&mut block_1);
                ledger_state.apply_block(&mut block_2);
                ledger_state.apply_block(&mut block_3);
                ledger_state.apply_block(&mut block_4);
                ledger_state.apply_block(&mut block_5);
                ledger_state.apply_block(&mut block_6);
            }

            // Check user balances
            {
                let base = base as usize;
                let quote = quote as usize;

                let user_balance = ledger_state.get_account_info_with_balances_or_default(&user_pk);
                let user_asset_balances = &user_balance.spot_balances.asset_balances;
                assert_eq!(user_asset_balances[base].available_balance, DEFAULT_BASE);
                assert_eq!(user_asset_balances[quote].available_balance, DEFAULT_QUOTE);

                let mm_1_balance = ledger_state.get_account_info_with_balances_or_default(&mm_1_pk);
                let mm_1_asset_balances = &mm_1_balance.spot_balances.asset_balances;
                assert_eq!(mm_1_asset_balances[base].available_balance, DEFAULT_BASE);
                assert_eq!(mm_1_asset_balances[quote].available_balance, DEFAULT_QUOTE);

                let mm_2_balance = ledger_state.get_account_info_with_balances_or_default(&mm_2_pk);
                let mm_2_asset_balances = &mm_2_balance.spot_balances.asset_balances;
                assert_eq!(mm_2_asset_balances[base].available_balance, DEFAULT_BASE);
                assert_eq!(mm_2_asset_balances[quote].available_balance, DEFAULT_QUOTE);
            }

            // setup market (C = cancelled)
            // Bids:
            // Price | Size
            // 2_200 | 700
            // 2_300 | 700, 400, 700 (C)
            // 2_450 | 1_000
            //
            // Asks
            // 2_500 | 600, 1_000, 300 (C)
            // 2_600 | 1_200
            // 2_700 | 700
            // 2_800 | 300
            {
                // id 0
                let mm_1_buy_1 = create_order_txn(
                    &mut mm_1_sk,
                    market_id,
                    OrderDirection::Buy,
                    OrderType::Limit(2_200, 700),
                    mm_1_nonce,
                );
                mm_1_nonce += 1;

                // id 1
                let mm_2_buy_1 = create_order_txn(
                    &mut mm_2_sk,
                    market_id,
                    OrderDirection::Buy,
                    OrderType::Limit(2_300, 700),
                    mm_2_nonce,
                );
                mm_2_nonce += 1;

                // id 2
                let mm_1_buy_2 = create_order_txn(
                    &mut mm_1_sk,
                    market_id,
                    OrderDirection::Buy,
                    OrderType::Limit(2_450, 1_000),
                    mm_1_nonce,
                );
                mm_1_nonce += 1;

                // id 3
                let mm_2_buy_2 = create_order_txn(
                    &mut mm_2_sk,
                    market_id,
                    OrderDirection::Buy,
                    OrderType::Limit(2_300, 400),
                    mm_2_nonce,
                );
                mm_2_nonce += 1;

                // id 4
                let mm_1_sell_1 = create_order_txn(
                    &mut mm_1_sk,
                    market_id,
                    OrderDirection::Sell,
                    OrderType::Limit(2_500, 600),
                    mm_1_nonce,
                );
                mm_1_nonce += 1;

                // id 5
                let mm_2_buy_3 = create_order_txn(
                    &mut mm_2_sk,
                    market_id,
                    OrderDirection::Buy,
                    OrderType::Limit(2_300, 700),
                    mm_2_nonce,
                );
                mm_2_nonce += 1;

                // id 6
                let mm_2_sell_1 = create_order_txn(
                    &mut mm_2_sk,
                    market_id,
                    OrderDirection::Sell,
                    OrderType::Limit(2_500, 1_000),
                    mm_2_nonce,
                );
                mm_2_nonce += 1;

                // id 7
                let mm_1_sell_2 = create_order_txn(
                    &mut mm_1_sk,
                    market_id,
                    OrderDirection::Sell,
                    OrderType::Limit(2_700, 700),
                    mm_1_nonce,
                );
                mm_1_nonce += 1;

                // id 8
                let mm_1_sell_3 = create_order_txn(
                    &mut mm_1_sk,
                    market_id,
                    OrderDirection::Sell,
                    OrderType::Limit(2_800, 300),
                    mm_1_nonce,
                );
                mm_1_nonce += 1;

                // id 9
                let mm_2_sell_2 = create_order_txn(
                    &mut mm_2_sk,
                    market_id,
                    OrderDirection::Sell,
                    OrderType::Limit(2_600, 1_200),
                    mm_2_nonce,
                );
                mm_2_nonce += 1;

                // id 10
                let mm_1_sell_4 = create_order_txn(
                    &mut mm_1_sk,
                    market_id,
                    OrderDirection::Sell,
                    OrderType::Limit(2_500, 300),
                    mm_1_nonce,
                );
                mm_1_nonce += 1;

                let mm_2_cancel_1 = create_cancel_txn(&mut mm_2_sk, 0, 5, mm_2_nonce);

                let mm_1_cancel_1 = create_cancel_txn(&mut mm_1_sk, 0, 10, mm_1_nonce);

                let mut block_1 = create_block(vec![mm_1_buy_1, mm_2_buy_1]);
                let mut block_2 = create_block(vec![mm_1_buy_2, mm_2_buy_2]);
                let mut block_3 = create_block(vec![mm_1_sell_1, mm_2_buy_3]);
                let mut block_4 = create_block(vec![mm_2_sell_1, mm_1_sell_2]);
                let mut block_5 = create_block(vec![mm_1_sell_3, mm_2_sell_2]);
                let mut block_6 = create_block(vec![mm_2_cancel_1, mm_1_sell_4]);
                let mut block_7 = create_block(vec![mm_1_cancel_1]);

                ledger_state.apply_block(&mut block_1);
                ledger_state.apply_block(&mut block_2);
                ledger_state.apply_block(&mut block_3);
                ledger_state.apply_block(&mut block_4);
                ledger_state.apply_block(&mut block_5);
                ledger_state.apply_block(&mut block_6);
                ledger_state.apply_block(&mut block_7);

                // Check account info state
                {
                    let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                    let open_orders = &mm_1_account_info.open_orders;
                    let completed_orders = &mm_1_account_info.completed_orders;

                    assert_eq!(open_orders.len(), 5);

                    let is_cancelled_still_in_open = open_orders.iter().find(|o| o.common.id == 10);
                    assert!(is_cancelled_still_in_open.is_none());

                    assert_eq!(completed_orders.len(), 1);
                    assert_eq!(completed_orders[0].get_id(), 10);
                }

                {
                    let mm_2_account_info = ledger_state.accounts.get(&mm_2_pk).unwrap();
                    let open_orders = &mm_2_account_info.open_orders;
                    let completed_orders = &mm_2_account_info.completed_orders;

                    assert_eq!(open_orders.len(), 4);

                    let is_cancelled_still_in_open =
                        open_orders.iter().find(|order| order.common.id == 5);
                    assert!(is_cancelled_still_in_open.is_none());

                    assert_eq!(completed_orders.len(), 1);
                    assert_eq!(completed_orders[0].get_id(), 5);
                }
            }

            ledger_state
        }

        #[test]
        pub fn test_user_limit_order_without_fill() {
            let mut ledger_state = test_setup();

            let mut user_nonce = 0;

            let mut user_sk = get_alice_sk();
            let user_pk = user_sk.verifying_key().to_bytes();

            // id 11
            let user_buy_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Buy,
                OrderType::Limit(2_300, 300),
                user_nonce,
            );
            user_nonce += 1;

            // id 12
            let user_buy_2 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Buy,
                OrderType::Limit(2_300, 400),
                user_nonce,
            );
            user_nonce += 1;

            // id 13
            let user_sell_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Sell,
                OrderType::Limit(2_600, 400),
                user_nonce,
            );
            user_nonce += 1;

            // id 14
            let user_sell_2 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Sell,
                OrderType::Limit(2_700, 400),
                user_nonce,
            );
            user_nonce += 1;

            let user_cancel_buy_2 = create_cancel_txn(&mut user_sk, 0, 12, user_nonce);
            user_nonce += 1;

            let user_cancel_sell_1 = create_cancel_txn(&mut user_sk, 0, 13, user_nonce);

            let mut block_1 = create_block(vec![user_buy_1]);
            let mut block_2 = create_block(vec![user_buy_2]);
            let mut block_3 = create_block(vec![user_sell_1]);
            let mut block_4 = create_block(vec![user_sell_2]);
            let mut block_5 = create_block(vec![user_cancel_buy_2]);
            let mut block_6 = create_block(vec![user_cancel_sell_1]);

            ledger_state.apply_block(&mut block_1);
            ledger_state.apply_block(&mut block_2);
            ledger_state.apply_block(&mut block_3);
            ledger_state.apply_block(&mut block_4);
            ledger_state.apply_block(&mut block_5);
            ledger_state.apply_block(&mut block_6);

            // Check account info state
            {
                let user_account_info = ledger_state.accounts.get(&user_pk).unwrap();
                let open_orders = &user_account_info.open_orders;
                let completed_orders = &user_account_info.completed_orders;

                assert_eq!(open_orders.len(), 2);

                assert_eq!(completed_orders.len(), 2);
                assert_eq!(completed_orders[0].get_id(), 12);
                assert_eq!(completed_orders[1].get_id(), 13);
            }
        }

        #[test]
        pub fn test_user_limit_order_with_residual_order() {
            let mut ledger_state = test_setup();

            let mut user_nonce = 0;

            let mut user_sk = get_alice_sk();
            let user_pk = user_sk.verifying_key().to_bytes();

            // id 11 - Should be filled by mm1 - order id 2
            let user_buy_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Buy,
                OrderType::Limit(2_600, 400),
                user_nonce,
            );
            user_nonce += 1;

            // id 12 - Should be filled by mm1 - order id 4
            let user_sell_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Sell,
                OrderType::Limit(2_400, 500),
                user_nonce,
            );

            let mut block_1 = create_block(vec![user_buy_1]);
            let mut block_2 = create_block(vec![user_sell_1]);

            ledger_state.apply_block(&mut block_1);
            ledger_state.apply_block(&mut block_2);
            // Check user account info state
            {
                let user_account_info = ledger_state.accounts.get(&user_pk).unwrap();
                let open_orders = &user_account_info.open_orders;
                let completed_orders = &user_account_info.completed_orders;

                assert_eq!(open_orders.len(), 0);

                assert_eq!(completed_orders.len(), 2);
                assert_eq!(completed_orders[0].get_id(), 11);
                assert_eq!(completed_orders[1].get_id(), 12);
            }

            // Check mm state
            {
                // mm_1
                let mm_1_pk = get_bob_sk().verifying_key().to_bytes();
                let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                let open_orders = &mm_1_account_info.open_orders;
                let completed_orders = &mm_1_account_info.completed_orders;

                assert_eq!(open_orders.len(), 5);

                assert_eq!(completed_orders.len(), 1);
                assert_eq!(completed_orders[0].get_id(), 10);

                assert_open_order(mm_1_account_info, 2, 500, 0);
                assert_open_order(mm_1_account_info, 4, 400, 0);

                // mm 2
                let mm_2_pk = get_carol_sk().verifying_key().to_bytes();
                let mm_2_account_info = ledger_state.accounts.get(&mm_2_pk).unwrap();
                let open_orders = &mm_2_account_info.open_orders;
                let completed_orders = &mm_2_account_info.completed_orders;

                assert_eq!(open_orders.len(), 4);
                assert_eq!(completed_orders.len(), 1);
            }
        }

        #[test]
        pub fn test_user_limit_buy_order_with_partial_fill() {
            let mut ledger_state = test_setup();
            let user_nonce = 0;

            let mut user_sk = get_alice_sk();
            let user_pk = user_sk.verifying_key().to_bytes();

            // id 11 - Should be filled by mm1 - order id 4 & mm2 - order id 6
            let user_buy_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Buy,
                OrderType::Limit(2_550, 1700),
                user_nonce,
            );

            let mut block_1 = create_block(vec![user_buy_1]);

            ledger_state.apply_block(&mut block_1);
            // Check account info state
            {
                let user_account_info = ledger_state.accounts.get(&user_pk).unwrap();
                let open_orders = &user_account_info.open_orders;
                let completed_orders = &user_account_info.completed_orders;
                assert_eq!(open_orders.len(), 1);
                assert_eq!(open_orders[0].filled_base_lots, 1600);
                assert_eq!(completed_orders.len(), 0);
                assert_eq!(open_orders[0].common.id, 11);
            }

            // Check mm state
            {
                // mm_1
                let mm_1_pk = get_bob_sk().verifying_key().to_bytes();
                let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                let open_orders = &mm_1_account_info.open_orders;
                let completed_orders = &mm_1_account_info.completed_orders;

                assert_eq!(open_orders.len(), 4);

                assert_eq!(completed_orders.len(), 2);
                assert_eq!(completed_orders[0].get_id(), 10);
                assert_eq!(completed_orders[1].get_id(), 4);

                let completed_order = &completed_orders[1];
                assert_completed_limit_order(completed_order, 4, 600, 0);

                // mm 2
                let mm_2_pk = get_carol_sk().verifying_key().to_bytes();
                let mm_2_account_info = ledger_state.accounts.get(&mm_2_pk).unwrap();
                let open_orders = &mm_2_account_info.open_orders;
                let completed_orders = &mm_2_account_info.completed_orders;

                assert_eq!(open_orders.len(), 3);
                assert_eq!(completed_orders.len(), 2);

                assert_eq!(completed_orders[0].get_id(), 5);
                assert_eq!(completed_orders[1].get_id(), 6);

                let completed_order = &completed_orders[1];

                assert_completed_limit_order(completed_order, 6, 1000, 0);
            }
        }

        #[test]
        pub fn test_user_limit_sell_order_with_partial_fill() {
            let mut ledger_state = test_setup();

            let user_nonce = 0;

            let mut user_sk = get_alice_sk();
            let user_pk = user_sk.verifying_key().to_bytes();

            // id 11 - Should be filled by mm1 - order id 4 & mm2 - order id 6
            let user_buy_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Sell,
                OrderType::Limit(2_400, 1700),
                user_nonce,
            );

            let mut block_1 = create_block(vec![user_buy_1]);

            ledger_state.apply_block(&mut block_1);
            // Check account info state
            {
                let user_account_info = ledger_state.accounts.get(&user_pk).unwrap();
                let open_orders = &user_account_info.open_orders;
                let completed_orders = &user_account_info.completed_orders;

                assert_eq!(open_orders.len(), 1);
                assert_eq!(open_orders[0].filled_base_lots, 1000);

                assert_eq!(completed_orders.len(), 0);
                assert_eq!(open_orders[0].common.id, 11);
            }

            // Check mm state
            {
                // mm_1
                let mm_1_pk = get_bob_sk().verifying_key().to_bytes();
                let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                let open_orders = &mm_1_account_info.open_orders;
                let completed_orders = &mm_1_account_info.completed_orders;

                assert_eq!(open_orders.len(), 4);

                assert_eq!(completed_orders.len(), 2);
                assert_eq!(completed_orders[0].get_id(), 10);
                assert_eq!(completed_orders[1].get_id(), 2);

                let completed_order = &completed_orders[1];
                assert_completed_limit_order(completed_order, 2, 1000, 0);

                // mm 2
                let mm_2_pk = get_carol_sk().verifying_key().to_bytes();
                let mm_2_account_info = ledger_state.accounts.get(&mm_2_pk).unwrap();
                let open_orders = &mm_2_account_info.open_orders;
                let completed_orders = &mm_2_account_info.completed_orders;

                assert_eq!(open_orders.len(), 4);
                assert_eq!(completed_orders.len(), 1);
                assert_eq!(completed_orders[0].get_id(), 5);
            }
        }

        #[test]
        pub fn test_user_limit_buy_with_self_fill_and_partial_fill() {
            let mut ledger_state = test_setup();
            let user_nonce = 0;

            let mut user_sk = get_alice_sk();
            let user_pk = user_sk.verifying_key().to_bytes();

            {
                // self fill
                let mut mm_1_sk = get_bob_sk();
                let mm_self_fill = create_order_txn(
                    &mut mm_1_sk,
                    0,
                    OrderDirection::Buy,
                    OrderType::Market(250_00),
                    7,
                );
                let mut block_1 = create_block(vec![mm_self_fill]);

                ledger_state.apply_block(&mut block_1);

                {
                    // mm_1
                    let mm_1_pk = get_bob_sk().verifying_key().to_bytes();
                    let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                    let self_filled_order = mm_1_account_info.get_open_order(4).unwrap();
                    assert_eq!(self_filled_order.self_filled, 100);
                    assert_eq!(self_filled_order.filled_base_lots, 0);
                }
            }

            // id 12 - Should be filled by mm1 - order id 4 & mm2 - order id 6
            let user_buy_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Buy,
                OrderType::Limit(2_550, 1700),
                user_nonce,
            );

            let mut block_1 = create_block(vec![user_buy_1]);

            ledger_state.apply_block(&mut block_1);
            // Check account info state
            {
                let user_account_info = ledger_state.accounts.get(&user_pk).unwrap();
                let open_orders = &user_account_info.open_orders;
                let completed_orders = &user_account_info.completed_orders;

                assert_eq!(open_orders.len(), 1);
                assert_eq!(open_orders[0].filled_base_lots, 1500);

                assert_eq!(completed_orders.len(), 0);
                assert_eq!(open_orders[0].common.id, 12);
            }

            // Check mm state
            {
                // mm_1
                let mm_1_pk = get_bob_sk().verifying_key().to_bytes();
                let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                let open_orders = &mm_1_account_info.open_orders;
                let completed_orders = &mm_1_account_info.completed_orders;

                assert_eq!(open_orders.len(), 4);

                assert_eq!(completed_orders.len(), 3);
                assert_eq!(completed_orders[0].get_id(), 10);
                assert_eq!(completed_orders[1].get_id(), 11);
                assert_eq!(completed_orders[2].get_id(), 4);

                let completed_order = &completed_orders[2];
                assert_completed_limit_order(completed_order, 4, 500, 100);

                // mm 2
                let mm_2_pk = get_carol_sk().verifying_key().to_bytes();
                let mm_2_account_info = ledger_state.accounts.get(&mm_2_pk).unwrap();
                let open_orders = &mm_2_account_info.open_orders;
                let completed_orders = &mm_2_account_info.completed_orders;

                assert_eq!(open_orders.len(), 3);
                assert_eq!(completed_orders.len(), 2);

                assert_eq!(completed_orders[0].get_id(), 5);
                assert_eq!(completed_orders[1].get_id(), 6);

                let completed_order = &completed_orders[1];
                assert_completed_limit_order(completed_order, 6, 1000, 0);
            }
        }

        #[test]
        pub fn test_user_self_cross_sell_transaction_fails() {
            let mut ledger_state = test_setup();

            let mut user_nonce = 0;

            let mut user_sk = get_alice_sk();
            let user_pk = user_sk.verifying_key().to_bytes();

            // id 11
            let user_buy_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Buy,
                OrderType::Limit(2_450, 2_000),
                user_nonce,
            );
            user_nonce += 1;

            //
            let user_sell_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Sell,
                OrderType::Limit(2_450, 1_000),
                user_nonce,
            );

            let mut block_1 = create_block(vec![user_buy_1]);
            let mut block_2 = create_block(vec![user_sell_1]);

            ledger_state.apply_block(&mut block_1);
            ledger_state.apply_block(&mut block_2);

            // Check account info state
            {
                let user_account_info = ledger_state.accounts.get(&user_pk).unwrap();
                let open_orders = &user_account_info.open_orders;
                let completed_orders = &user_account_info.completed_orders;

                assert_eq!(completed_orders.len(), 0);
                assert_eq!(open_orders.len(), 1);
                assert_eq!(open_orders[0].common.id, 11);
            }

            assert_eq!(
                block_2.transactions()[0].get_status(),
                TransactionStatus::Rejected("Self Cross".to_string())
            )
        }

        #[test]
        pub fn test_user_self_cross_buy_transaction_fails() {
            let mut ledger_state = test_setup();

            let mut user_nonce = 0;

            let mut user_sk = get_alice_sk();
            let user_pk = user_sk.verifying_key().to_bytes();

            // id 11, crosses the spread and rests on order book
            let user_sell_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Sell,
                OrderType::Limit(2_450, 2_000),
                user_nonce,
            );
            user_nonce += 1;

            //
            let user_cross_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Buy,
                OrderType::Limit(2_450, 1_000),
                user_nonce,
            );

            let mut block_1 = create_block(vec![user_sell_1]);
            let mut block_2 = create_block(vec![user_cross_1]);

            ledger_state.apply_block(&mut block_1);
            ledger_state.apply_block(&mut block_2);

            // Check account info state
            {
                let user_account_info = ledger_state.accounts.get(&user_pk).unwrap();
                let open_orders = &user_account_info.open_orders;
                let completed_orders = &user_account_info.completed_orders;

                assert_eq!(completed_orders.len(), 0);
                assert_eq!(open_orders.len(), 1);
                assert_eq!(open_orders[0].common.id, 11);
                assert_eq!(open_orders[0].filled_base_lots, 1000)
            }

            assert_eq!(
                block_2.transactions()[0].get_status(),
                TransactionStatus::Rejected("Self Cross".to_string())
            )
        }

        #[test]
        pub fn test_market_buy_with_self_fill() {
            let mut ledger_state = test_setup();
            let user_nonce = 0;

            let mut user_sk = get_alice_sk();
            let user_pk = user_sk.verifying_key().to_bytes();

            // self fill
            {
                let mut mm_1_sk = get_bob_sk();
                let mm_self_fill = create_order_txn(
                    &mut mm_1_sk,
                    0,
                    OrderDirection::Buy,
                    OrderType::Market(250_00),
                    7,
                );
                let mut block_1 = create_block(vec![mm_self_fill]);

                ledger_state.apply_block(&mut block_1);

                {
                    // mm_1
                    let mm_1_pk = get_bob_sk().verifying_key().to_bytes();
                    let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                    let self_filled_order = mm_1_account_info.get_open_order(4).unwrap();
                    assert_eq!(self_filled_order.self_filled, 100);
                    assert_eq!(self_filled_order.filled_base_lots, 0);
                }
            }

            // id 12 - Should be filled by mm1
            let user_buy_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Buy,
                OrderType::Market(1000 * 250),
                user_nonce,
            );

            let mut block_1 = create_block(vec![user_buy_1]);

            ledger_state.apply_block(&mut block_1);
            // Check account info state
            {
                let user_account_info = ledger_state.accounts.get(&user_pk).unwrap();
                let open_orders = &user_account_info.open_orders;
                let completed_orders = &user_account_info.completed_orders;

                assert_eq!(open_orders.len(), 0);

                assert_eq!(completed_orders.len(), 1);
                let completed_order = completed_orders[0].clone();
                assert_completed_market_buy(&completed_order, 12, 2_500_00, 0, 2_500_00);
            }

            // Check mm state
            {
                // mm_1
                let mm_1_pk = get_bob_sk().verifying_key().to_bytes();
                let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                let open_orders = &mm_1_account_info.open_orders;
                let completed_orders = &mm_1_account_info.completed_orders;

                assert_eq!(open_orders.len(), 4);

                assert_eq!(completed_orders.len(), 3);
                assert_eq!(completed_orders[0].get_id(), 10);
                assert_eq!(completed_orders[1].get_id(), 11);
                assert_eq!(completed_orders[2].get_id(), 4);

                let completed_order = &completed_orders[2];
                assert_completed_limit_order(completed_order, 4, 500, 100);

                // mm 2
                let mm_2_pk = get_carol_sk().verifying_key().to_bytes();
                let mm_2_account_info = ledger_state.accounts.get(&mm_2_pk).unwrap();
                let open_orders = &mm_2_account_info.open_orders;
                let completed_orders = &mm_2_account_info.completed_orders;

                assert_eq!(open_orders.len(), 4);
                assert_eq!(completed_orders.len(), 1);

                assert_eq!(completed_orders[0].get_id(), 5);

                let order = mm_2_account_info.get_open_order(6).unwrap();

                assert_eq!(order.filled_base_lots, 500);
            }
        }

        #[test]
        pub fn test_market_sell_with_self_fill() {
            let mut ledger_state = test_setup();
            let user_nonce = 0;

            let mut user_sk = get_alice_sk();
            let user_pk = user_sk.verifying_key().to_bytes();

            // self fill
            {
                let mut mm_1_sk = get_bob_sk();
                let mm_self_fill = create_order_txn(
                    &mut mm_1_sk,
                    0,
                    OrderDirection::Sell,
                    OrderType::Market(300),
                    7,
                );
                let mut block_1 = create_block(vec![mm_self_fill]);

                ledger_state.apply_block(&mut block_1);

                {
                    // mm_1
                    let mm_1_pk = get_bob_sk().verifying_key().to_bytes();
                    let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                    let self_filled_order = mm_1_account_info.get_open_order(2).unwrap();
                    assert_eq!(self_filled_order.self_filled, 300);
                    assert_eq!(self_filled_order.filled_base_lots, 0);
                }
            }

            // id 12 - Should be filled by mm1
            let user_buy_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Sell,
                OrderType::Market(1000),
                user_nonce,
            );

            let mut block_1 = create_block(vec![user_buy_1]);

            ledger_state.apply_block(&mut block_1);
            // Check account info state
            {
                let user_account_info = ledger_state.accounts.get(&user_pk).unwrap();
                let open_orders = &user_account_info.open_orders;
                let completed_orders = &user_account_info.completed_orders;

                assert_eq!(open_orders.len(), 0);

                assert_eq!(completed_orders.len(), 1);
                let completed_order = completed_orders[0].clone();
                assert_completed_market_sell(&completed_order, 12, 1000, 0, 1000);
            }

            // Check mm state
            {
                // mm_1
                let mm_1_pk = get_bob_sk().verifying_key().to_bytes();
                let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                let open_orders = &mm_1_account_info.open_orders;
                let completed_orders = &mm_1_account_info.completed_orders;

                assert_eq!(open_orders.len(), 4);

                assert_eq!(completed_orders.len(), 3);
                assert_eq!(completed_orders[0].get_id(), 10);
                assert_eq!(completed_orders[1].get_id(), 11);
                assert_eq!(completed_orders[2].get_id(), 2);

                let completed_order = &completed_orders[2];
                assert_completed_limit_order(completed_order, 2, 700, 300);

                // mm 2
                let mm_2_pk = get_carol_sk().verifying_key().to_bytes();
                let mm_2_account_info = ledger_state.accounts.get(&mm_2_pk).unwrap();
                let open_orders = &mm_2_account_info.open_orders;
                let completed_orders = &mm_2_account_info.completed_orders;

                assert_eq!(open_orders.len(), 4);
                assert_eq!(completed_orders.len(), 1);
                assert_eq!(completed_orders[0].get_id(), 5);

                let order = mm_2_account_info.get_open_order(1).unwrap();

                assert_eq!(order.filled_base_lots, 300);
            }
        }

        #[test]
        pub fn test_market_buy_with_user_self_fill() {
            let mut ledger_state = test_setup();
            let mut user_nonce = 0;

            let mut user_sk = get_alice_sk();
            let user_pk = user_sk.verifying_key().to_bytes();

            // self fill - 100 lots
            {
                let mut mm_1_sk = get_bob_sk();
                let mm_self_fill = create_order_txn(
                    &mut mm_1_sk,
                    0,
                    OrderDirection::Buy,
                    OrderType::Market(250_00),
                    7,
                );
                let mut block_1 = create_block(vec![mm_self_fill]);

                ledger_state.apply_block(&mut block_1);

                {
                    // mm_1
                    let mm_1_pk = get_bob_sk().verifying_key().to_bytes();
                    let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                    println!("{:?}", mm_1_account_info);
                    let self_filled_order = mm_1_account_info.get_open_order(4).unwrap();
                    assert_eq!(self_filled_order.self_filled, 100);
                    assert_eq!(self_filled_order.filled_base_lots, 0);
                }
            }

            // id 12
            let user_sell_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Sell,
                OrderType::Limit(2500, 300),
                user_nonce,
            );
            user_nonce += 1;

            let mut block_1 = create_block(vec![user_sell_1]);

            ledger_state.apply_block(&mut block_1);

            // id 13 - Should be filled by mm1
            let user_buy_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Buy,
                OrderType::Market(1650 * 250),
                user_nonce,
            );

            let mut block_2 = create_block(vec![user_buy_1]);

            ledger_state.apply_block(&mut block_2);
            // Check account info state
            {
                let user_account_info = ledger_state.accounts.get(&user_pk).unwrap();
                let open_orders = &user_account_info.open_orders;
                let completed_orders = &user_account_info.completed_orders;

                // User's market buy should self fill 150 lots
                assert_eq!(open_orders.len(), 1);
                assert_open_order(user_account_info, 12, 0, 150);

                assert_eq!(completed_orders.len(), 1);
                let completed_order = completed_orders[0].clone();
                assert_completed_market_buy(&completed_order, 13, 3_750_00, 150 * 250, 250 * 1650);
            }

            // Check mm state
            {
                // mm_1
                let mm_1_pk = get_bob_sk().verifying_key().to_bytes();
                let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                let open_orders = &mm_1_account_info.open_orders;
                let completed_orders = &mm_1_account_info.completed_orders;

                assert_eq!(open_orders.len(), 4);

                assert_eq!(completed_orders.len(), 3);
                assert_eq!(completed_orders[0].get_id(), 10);
                assert_eq!(completed_orders[1].get_id(), 11);
                assert_eq!(completed_orders[2].get_id(), 4);

                let completed_order = &completed_orders[2];
                assert_completed_limit_order(completed_order, 4, 500, 100);

                // mm 2
                let mm_2_pk = get_carol_sk().verifying_key().to_bytes();
                let mm_2_account_info = ledger_state.accounts.get(&mm_2_pk).unwrap();
                let open_orders = &mm_2_account_info.open_orders;
                let completed_orders = &mm_2_account_info.completed_orders;

                assert_eq!(open_orders.len(), 3);
                assert_eq!(completed_orders.len(), 2);

                assert_eq!(completed_orders[0].get_id(), 5);
                assert_eq!(completed_orders[1].get_id(), 6);
                let completed_order = &completed_orders[1];
                assert_completed_limit_order(completed_order, 6, 1000, 0);
            }
        }

        #[test]
        pub fn test_market_sell_with_user_self_fill() {
            let mut ledger_state = test_setup();
            let mut user_nonce = 0;

            let mut user_sk = get_alice_sk();
            let user_pk = user_sk.verifying_key().to_bytes();

            // self fill
            {
                let mut mm_1_sk = get_bob_sk();
                let mm_self_fill = create_order_txn(
                    &mut mm_1_sk,
                    0,
                    OrderDirection::Sell,
                    OrderType::Market(300),
                    7,
                );
                let mut block_1 = create_block(vec![mm_self_fill]);

                ledger_state.apply_block(&mut block_1);

                {
                    // mm_1
                    let mm_1_pk = get_bob_sk().verifying_key().to_bytes();
                    let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                    let self_filled_order = mm_1_account_info.get_open_order(2).unwrap();
                    assert_eq!(self_filled_order.self_filled, 300);
                    assert_eq!(self_filled_order.filled_base_lots, 0);
                }
            }
            // id 12
            let user_buy_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Buy,
                OrderType::Limit(2450, 500),
                user_nonce,
            );
            user_nonce += 1;

            let mut block_1 = create_block(vec![user_buy_1]);

            ledger_state.apply_block(&mut block_1);

            // id 13 - Should be filled by mm1
            let user_sell_1 = create_order_txn(
                &mut user_sk,
                0,
                OrderDirection::Sell,
                OrderType::Market(1100),
                user_nonce,
            );

            let mut block_2 = create_block(vec![user_sell_1]);

            ledger_state.apply_block(&mut block_2);
            // Check account info state
            {
                let user_account_info = ledger_state.accounts.get(&user_pk).unwrap();
                let open_orders = &user_account_info.open_orders;
                let completed_orders = &user_account_info.completed_orders;

                assert_eq!(open_orders.len(), 1);
                assert_open_order(user_account_info, 12, 0, 400);

                assert_eq!(completed_orders.len(), 1);
                let completed_order = completed_orders[0].clone();
                assert_completed_market_sell(&completed_order, 13, 700, 400, 1100);
            }

            // Check mm state
            {
                // mm_1
                let mm_1_pk = get_bob_sk().verifying_key().to_bytes();
                let mm_1_account_info = ledger_state.accounts.get(&mm_1_pk).unwrap();
                let open_orders = &mm_1_account_info.open_orders;
                let completed_orders = &mm_1_account_info.completed_orders;

                assert_eq!(open_orders.len(), 4);

                assert_eq!(completed_orders.len(), 3);
                assert_eq!(completed_orders[0].get_id(), 10);
                assert_eq!(completed_orders[1].get_id(), 11);
                assert_eq!(completed_orders[2].get_id(), 2);

                let completed_order = &completed_orders[2];
                assert_completed_limit_order(completed_order, 2, 700, 300);

                // mm 2
                let mm_2_pk = get_carol_sk().verifying_key().to_bytes();
                let mm_2_account_info = ledger_state.accounts.get(&mm_2_pk).unwrap();
                let open_orders = &mm_2_account_info.open_orders;
                let completed_orders = &mm_2_account_info.completed_orders;

                assert_eq!(open_orders.len(), 4);
                assert_eq!(completed_orders.len(), 1);
                assert_eq!(completed_orders[0].get_id(), 5);
            }
        }
    }
}
