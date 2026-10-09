use serde::{Deserialize, Serialize};

use super::{
    asset::AssetId,
    order::{
        LimitFillResult, LimitOrder, MarketBuyOrder, MarketOrder, MarketOrderMatchingResults,
        MarketSellOrder, OrderDirection, OrderPriceMultiple, OrderStatus, ResidualOrder,
        UserExecutionResult,
    },
    spot_clearinghouse::{MarketId, MarketPrecision, base_to_quote_lots, quote_lots_to_base_lots},
};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LevelInfo {
    pub price: u64,
    pub volume: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MarketInfo {
    pub market_id: MarketId,
    pub market_name: String,
    pub last_executed_price: Option<u64>,
    pub best_asks_info: Option<LevelInfo>,
    pub best_bids_info: Option<LevelInfo>,
    pub tick: u32,
    pub tick_decimals: u8,
    pub base_name: String,
    pub quote_name: String,
    pub base_asset_id: AssetId,
    pub quote_asset_id: AssetId,
}

#[derive(Debug)]
pub struct Level {
    pub price: u64,
    pub volume: u64,
    pub orders: Vec<LimitOrder>,
    pub cancelled: u32,
}
pub struct SpotMarket {
    pub market_id: MarketId,
    pub asset_one: AssetId,
    pub asset_two: AssetId,
    pub base_asset: AssetId,
    pub quote_asset: AssetId,
    pub base_asset_name: String,
    pub quote_asset_name: String,
    pub tick: u32,
    pub tick_decimals: u8,
    pub last_executed_price: Option<u64>,
    // pub lot_size: (),

    // levels are in reverse order, best prices are at the end
    pub bids_levels: Vec<Level>, // 0, 1, 2, ..
    pub asks_levels: Vec<Level>, // 10, 9, 8, ..
}

impl SpotMarket {
    pub(crate) fn new(
        market_id: MarketId,
        normalised_pair: (AssetId, AssetId),
        base_asset: AssetId,
        quote_asset: AssetId,
        base_asset_name: String,
        quote_asset_name: String,
        tick: u32,
        tick_decimals: u8,
    ) -> Self {
        Self {
            market_id,
            asset_one: normalised_pair.0,
            asset_two: normalised_pair.1,
            base_asset,
            quote_asset,
            bids_levels: vec![],
            asks_levels: vec![],
            tick,
            tick_decimals,
            last_executed_price: None,
            base_asset_name,
            quote_asset_name: quote_asset_name.to_string(),
        }
    }

    fn add_order_with_cmp<F>(levels: &mut Vec<Level>, order: LimitOrder, mut compare: F)
    where
        F: FnMut(OrderPriceMultiple, OrderPriceMultiple) -> std::cmp::Ordering,
    {
        let price = order.price_multiple;
        let mut left = 0;
        let mut right = levels.len();

        while left < right {
            let mid = left + (right - left) / 2;
            let mid_price = levels[mid].price;

            if price == mid_price {
                levels[mid].volume += order.base_lots - order.filled_base_lots;
                levels[mid].orders.push(order);
                return;
            } else {
                if compare(price, mid_price) == std::cmp::Ordering::Less {
                    right = mid;
                } else {
                    left = mid + 1;
                }
            }
        }

        levels.insert(
            left,
            Level {
                price,
                volume: order.base_lots - order.filled_base_lots,
                orders: vec![order],
                cancelled: 0,
            },
        )
    }

    fn mark_order_as_cancelled(orders: &mut [LimitOrder], order: &LimitOrder) -> bool {
        let order_id = order.common.id;
        let mut left = 0;
        let mut right = orders.len();
        while left < right {
            let mid = left + (right - left) / 2;
            let mid_id = orders[mid].common.id;

            if order_id == mid_id {
                if orders[mid].common.status == OrderStatus::Cancelled {
                    return false;
                }
                orders[mid].common.status = OrderStatus::Cancelled;
                return true;
            } else if order_id < mid_id {
                right = mid;
            } else {
                left = mid + 1;
            }
        }
        false
    }

    fn cancel_order_with_cmp<F>(levels: &mut Vec<Level>, order: &LimitOrder, mut compare: F) -> u64
    where
        F: FnMut(OrderPriceMultiple, OrderPriceMultiple) -> std::cmp::Ordering,
    {
        let price = order.price_multiple;
        let mut left = 0;
        let mut right = levels.len();

        while left < right {
            let mid = left + (right - left) / 2;
            let mid_price = levels[mid].price;

            if price == mid_price {
                let level = &mut levels[mid];
                if !Self::mark_order_as_cancelled(&mut level.orders, order) {
                    return 0;
                }

                level.cancelled += 1;
                let unfilled_size = order.base_lots - order.filled_base_lots;
                level.volume -= unfilled_size;

                if level.volume == 0 {
                    levels.remove(mid);
                } else if level.cancelled > (level.orders.len() / 2) as u32 {
                    // prune when vector is sparse enough
                    level
                        .orders
                        .retain(|order| order.common.status != OrderStatus::Cancelled);
                    level.cancelled = 0;
                }

                return unfilled_size;
            } else {
                if compare(price, mid_price) == std::cmp::Ordering::Less {
                    right = mid;
                } else {
                    left = mid + 1;
                }
            }
        }
        0
    }

    pub fn add_bid(&mut self, order: LimitOrder) {
        Self::add_order_with_cmp(&mut self.bids_levels, order, |a, b| {
            a.partial_cmp(&b).unwrap()
        });
    }

    pub fn add_ask(&mut self, order: LimitOrder) {
        Self::add_order_with_cmp(&mut self.asks_levels, order, |a, b| {
            b.partial_cmp(&a).unwrap()
        });
    }

    pub fn cancel_bid(&mut self, order: &LimitOrder) -> u64 {
        Self::cancel_order_with_cmp(&mut self.bids_levels, order, |a, b| {
            a.partial_cmp(&b).unwrap()
        })
    }

    pub fn cancel_ask(&mut self, order: &LimitOrder) -> u64 {
        Self::cancel_order_with_cmp(&mut self.asks_levels, order, |a, b| {
            b.partial_cmp(&a).unwrap()
        })
    }

    pub fn execute_limit<F>(
        levels: &mut Vec<Level>,
        order: &mut LimitOrder,
        asset_in: AssetId,
        asset_out: AssetId,
        is_buy: bool,
        precision: &MarketPrecision,
        mut compare: F,
    ) -> LimitFillResult
    where
        F: FnMut(OrderPriceMultiple, OrderPriceMultiple) -> std::cmp::Ordering,
    {
        let mut filled_orders: Vec<LimitOrder> = vec![];

        let mut residual_order: Option<ResidualOrder> = None;
        let mut lots_in: u64 = 0;
        let mut lots_out: u64 = 0;
        let mut last_executed_price: Option<u64> = None;

        let order_price = order.price_multiple;
        let mut remaining_base_amount = order.base_lots;
        while !levels.is_empty() && remaining_base_amount > 0 {
            let level = levels.last_mut();
            let Some(level) = level else {
                break;
            };

            let level_price = level.price;
            let level_filled = remaining_base_amount;
            let mut cancelled_seen = 0;

            match compare(order_price, level_price) {
                std::cmp::Ordering::Less | std::cmp::Ordering::Equal => {
                    // fill the current level orders
                    let mut to_drain_end_index = 0;
                    for order in level.orders.iter_mut() {
                        if order.common.status == OrderStatus::Cancelled {
                            cancelled_seen += 1;
                            to_drain_end_index += 1;
                            continue;
                        }

                        let order_remaining = order.get_order_remaining();

                        let curr_filled_base_amount = remaining_base_amount.min(order_remaining);
                        remaining_base_amount -= curr_filled_base_amount;
                        last_executed_price = Some(level_price);

                        if is_buy {
                            lots_in += curr_filled_base_amount;
                            lots_out +=
                                base_to_quote_lots(curr_filled_base_amount, level_price, precision);
                        } else {
                            lots_out += curr_filled_base_amount;
                            lots_in +=
                                base_to_quote_lots(curr_filled_base_amount, level_price, precision);
                        }

                        if curr_filled_base_amount == order_remaining {
                            // Include current index
                            to_drain_end_index += 1;
                        }

                        if remaining_base_amount == 0 {
                            if curr_filled_base_amount < order_remaining {
                                // Partial fill
                                residual_order = Some(ResidualOrder {
                                    order_id: order.common.id,
                                    price_multiple: level_price,
                                    account_public_key: order.common.account,
                                    filled_base_lots: curr_filled_base_amount,
                                    self_fill: 0,
                                });
                                order.filled_base_lots += curr_filled_base_amount;
                            }
                            break;
                        }
                    }

                    if to_drain_end_index < level.orders.len() {
                        filled_orders
                            .append(&mut level.orders.drain(0..to_drain_end_index).collect());
                        level.volume -= level_filled;
                        level.cancelled -= cancelled_seen;
                        break;
                    }
                    // reached the end of the level without fully filling the order
                    // remove this level from the orderbook
                    filled_orders.append(&mut level.orders);
                    levels.pop();
                }
                _ => break,
            }
        }

        order.filled_base_lots = order.base_lots - remaining_base_amount;

        // Return execution results for clearinghouse to settle
        LimitFillResult {
            filled_orders,
            residual_order,
            user_order: UserExecutionResult {
                order_id: order.common.id,
                lots_out,
                lots_in,
                asset_in,
                asset_out,
                filled_size: order.filled_base_lots,
            },
            last_executed_price,
        }
    }

    pub fn execute_market_buy_order(
        levels: &mut Vec<Level>,
        buy_order: MarketBuyOrder,
        precision: &MarketPrecision,
    ) -> MarketOrderMatchingResults {
        let mut filled_orders: Vec<LimitOrder> = vec![];

        // We can have at most 1 partially filled order for the counter_party
        // which will be the first element in the best price level
        let mut residual_order: Option<ResidualOrder> = None;
        let mut base_lots_in: u64 = 0;
        let mut self_fill_quotes: u64 = 0;
        let mut remaining_quote_lots = buy_order.quote_size;
        let mut last_executed_price: Option<u64> = None;

        while !levels.is_empty() && remaining_quote_lots > 0 {
            let level = levels.last_mut();
            let Some(level) = level else {
                break;
            };

            let mut level_filled = 0;
            let level_price = level.price;
            let mut level_cancelled = 0;

            let mut to_drain_end_index = 0;

            // remaining base lots need as order size are stores in base size
            let mut remaining_base_lots =
                quote_lots_to_base_lots(remaining_quote_lots, level_price, &precision);

            if remaining_base_lots == 0 {
                break;
            }

            for order in level.orders.iter_mut() {
                if order.common.status == OrderStatus::Cancelled {
                    level_cancelled += 1;
                    to_drain_end_index += 1;
                    continue;
                }

                let order_base_remaining = order.get_order_remaining();

                if order.common.account == buy_order.common.account {
                    // self trade
                    let reduce_base = remaining_base_lots.min(order_base_remaining);

                    let reduce_quote = base_to_quote_lots(reduce_base, level_price, precision);

                    order.self_filled += reduce_base;
                    self_fill_quotes += reduce_quote;

                    remaining_base_lots -= reduce_base;
                    remaining_quote_lots -= reduce_quote;

                    level_filled += reduce_base;

                    if reduce_base == order_base_remaining {
                        to_drain_end_index += 1;
                    }

                    if remaining_base_lots == 0 {
                        if reduce_base < order_base_remaining {
                            // residual order
                            residual_order = Some(ResidualOrder {
                                order_id: order.common.id,
                                price_multiple: level_price,
                                account_public_key: order.common.account,
                                filled_base_lots: 0,
                                self_fill: reduce_base,
                            });
                        }
                        break;
                    }
                    continue;
                }

                let filled_base_lots = remaining_base_lots.min(order_base_remaining);
                last_executed_price = Some(level.price);
                remaining_base_lots -= filled_base_lots;
                level_filled += filled_base_lots;

                let filled_quote_lots =
                    base_to_quote_lots(filled_base_lots, level_price, precision);

                base_lots_in += filled_base_lots;
                remaining_quote_lots -= filled_quote_lots;

                // Don't modify the order's filled amount here as we are using it
                // to determine the filled amount when settling the order

                if filled_base_lots == order_base_remaining {
                    // Include the current index
                    to_drain_end_index += 1;
                }

                if remaining_base_lots == 0 {
                    if filled_base_lots < order_base_remaining {
                        // Partial fill

                        residual_order = Some(ResidualOrder {
                            order_id: order.common.id,
                            price_multiple: level_price,
                            account_public_key: order.common.account,
                            filled_base_lots,
                            self_fill: 0,
                        });
                        order.filled_base_lots += filled_base_lots;
                    }
                    break;
                }
            }

            if to_drain_end_index < level.orders.len() {
                filled_orders.append(&mut level.orders.drain(0..to_drain_end_index).collect());
                level.volume -= level_filled;
                level.cancelled -= level_cancelled;
                break;
            }
            // reached the end of the level without fully filling the order
            // remove this level from the orderbook
            filled_orders.append(&mut level.orders);
            levels.pop();
        }

        // Return execution results for clearinghouse to settle
        return MarketOrderMatchingResults::Buy {
            quote_filled_lots: buy_order.quote_size - remaining_quote_lots - self_fill_quotes,
            base_lots_in,
            filled_orders,
            self_fill: self_fill_quotes,
            residual_order,
            order_id: buy_order.common.id,
            last_executed_price,
        };
    }

    /// Denominated in quote/base price
    pub fn execute_market_sell_order(
        levels: &mut Vec<Level>,
        sell_order: MarketSellOrder,
        precision: &MarketPrecision,
    ) -> MarketOrderMatchingResults {
        let mut filled_orders: Vec<LimitOrder> = vec![];

        // We can have at most 1 partially filled order for the counter_party
        // which will be the first element in the best price level
        let mut maker_partial_fill: Option<ResidualOrder> = None;
        let mut quote_lots_in: u64 = 0;
        let mut self_fill: u64 = 0;
        let mut last_executed_price: Option<u64> = None;

        let mut remaining_base_lots = sell_order.base_size;

        while !levels.is_empty() && remaining_base_lots > 0 {
            let level = levels.last_mut();
            let Some(level) = level else {
                break;
            };
            let level_price = level.price;
            let level_filled = remaining_base_lots;
            let mut cancelled_seen = 0;

            let mut to_drain_end_index = 0;
            for order in level.orders.iter_mut() {
                if order.common.status == OrderStatus::Cancelled {
                    cancelled_seen += 1;
                    to_drain_end_index += 1;
                    continue;
                }

                let order_remaining = order.get_order_remaining();

                if order.common.account == sell_order.common.account {
                    // self trade
                    let reduce = remaining_base_lots.min(order_remaining);
                    self_fill += reduce;
                    remaining_base_lots -= reduce;
                    order.self_filled += reduce;

                    if reduce == order_remaining {
                        to_drain_end_index += 1;
                    }

                    if remaining_base_lots == 0 {
                        if reduce < order_remaining {
                            // Residual order
                            maker_partial_fill = Some(ResidualOrder {
                                order_id: order.common.id,
                                price_multiple: level_price,
                                account_public_key: order.common.account,
                                filled_base_lots: 0,
                                self_fill: reduce,
                            });
                        }
                        break;
                    }
                    continue;
                }

                let filled_base_lots = remaining_base_lots.min(order_remaining);

                last_executed_price = Some(level.price);
                remaining_base_lots -= filled_base_lots;
                // Don't modify the order's filled amount here as we are using it
                // to determine the filled amount when settling the order

                quote_lots_in += base_to_quote_lots(filled_base_lots, level_price, precision);

                if filled_base_lots == order_remaining {
                    // include current index
                    to_drain_end_index += 1;
                }

                if remaining_base_lots == 0 {
                    if filled_base_lots < order_remaining {
                        // Residual order
                        maker_partial_fill = Some(ResidualOrder {
                            order_id: order.common.id,
                            price_multiple: level_price,
                            account_public_key: order.common.account,
                            filled_base_lots,
                            self_fill: 0,
                        });
                        order.filled_base_lots += filled_base_lots;
                    }
                    break;
                }
            }

            if to_drain_end_index < level.orders.len() {
                filled_orders.append(&mut level.orders.drain(0..to_drain_end_index).collect());
                level.volume -= level_filled;
                level.cancelled -= cancelled_seen;
                break;
            }
            // reached the end of the level without fully filling the order
            // remove this level from the orderbook
            filled_orders.append(&mut level.orders);
            levels.pop();
        }

        // Return execution results for clearinghouse to settle
        return MarketOrderMatchingResults::Sell {
            filled_orders,
            residual_order: maker_partial_fill,
            base_filled_lots: sell_order.base_size - remaining_base_lots - self_fill,
            quote_lots_in,
            self_fill,
            order_id: sell_order.common.id,
            last_executed_price,
        };
    }

    pub fn handle_market_order(
        &mut self,
        order: MarketOrder,
        precision: &MarketPrecision,
    ) -> MarketOrderMatchingResults {
        let result = match order {
            MarketOrder::Sell(sell_order) => {
                Self::execute_market_sell_order(&mut self.bids_levels, sell_order, precision)
            }
            MarketOrder::Buy(buy_order) => {
                Self::execute_market_buy_order(&mut self.asks_levels, buy_order, precision)
            }
        };
        self.set_last_executed_price(result.get_last_executed_price());
        result
    }

    pub fn add_limit_order(
        &mut self,
        mut order: LimitOrder,
        base_asset: AssetId,
        quote_asset: AssetId,
        precision: &MarketPrecision,
    ) -> Option<LimitFillResult> {
        match order.common.direction {
            OrderDirection::Buy => {
                let best_ask_price = self.get_best_prices().1;

                let Some(best_ask_price) = best_ask_price else {
                    self.add_bid(order);
                    return None;
                };

                if best_ask_price <= order.price_multiple {
                    // Attempt to execute order at a better price

                    let result = Self::execute_limit(
                        &mut self.asks_levels,
                        &mut order,
                        base_asset,
                        quote_asset,
                        true,
                        precision,
                        |a, b| b.partial_cmp(&a).unwrap(),
                    );
                    self.set_last_executed_price(result.last_executed_price);

                    // Determine whether we need to add the order
                    if order.filled_base_lots < order.base_lots {
                        self.add_bid(order);
                    }
                    return Some(result);
                }

                self.add_bid(order);
                return None;
            }
            OrderDirection::Sell => {
                let best_bid_price = self.get_best_prices().0;

                let Some(best_bid_price) = best_bid_price else {
                    self.add_ask(order);
                    return None;
                };

                if best_bid_price >= order.price_multiple {
                    // Attempt to execute order at a better price
                    let result = Self::execute_limit(
                        &mut self.bids_levels,
                        &mut order,
                        quote_asset,
                        base_asset,
                        false,
                        precision,
                        |a, b| a.partial_cmp(&b).unwrap(),
                    );

                    self.set_last_executed_price(result.last_executed_price);

                    // Determine whether we need to add the order
                    if order.filled_base_lots < order.base_lots {
                        self.add_ask(order);
                    }
                    return Some(result);
                }

                self.add_ask(order);
                return None;
            }
        }
    }

    pub fn cancel_order(&mut self, order: &LimitOrder) -> u64 {
        match order.common.direction {
            OrderDirection::Buy => self.cancel_bid(order),
            OrderDirection::Sell => self.cancel_ask(order),
        }
    }

    pub fn get_best_prices(&self) -> (Option<u64>, Option<u64>) {
        let best_bid = self.bids_levels.last().map(|level| level.price);
        let best_ask = self.asks_levels.last().map(|level| level.price);

        (best_bid, best_ask)
    }

    pub fn get_last_executed_price(&self) -> Option<u64> {
        self.last_executed_price
    }

    pub fn set_last_executed_price(&mut self, last_executed_price: Option<u64>) {
        if let Some(price) = last_executed_price {
            self.last_executed_price = Some(price);
        }
    }

    pub fn get_market_info(&self) -> MarketInfo {
        let best_ask_info = self.asks_levels.last().map(|level| LevelInfo {
            price: level.price,
            volume: level.volume,
        });

        let best_bid_info = self.bids_levels.last().map(|level| LevelInfo {
            price: level.price,
            volume: level.volume,
        });

        MarketInfo {
            market_id: self.market_id,
            market_name: format!("{}/{}", self.base_asset_name, self.quote_asset_name),
            last_executed_price: self.last_executed_price,
            best_asks_info: best_ask_info,
            best_bids_info: best_bid_info,
            tick: self.tick,
            tick_decimals: self.tick_decimals,
            base_name: self.base_asset_name.clone(),
            quote_name: self.quote_asset_name.clone(),
            base_asset_id: self.base_asset,
            quote_asset_id: self.quote_asset,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        state::order::{CommonOrderFields, LimitOrder, OrderDirection, OrderId, OrderStatus},
        types::transaction::PublicKeyHash,
    };

    fn new_limit(
        price_tick: u64,
        lot_size: u64,
        direction: OrderDirection,
        id: OrderId,
        account: PublicKeyHash,
    ) -> LimitOrder {
        LimitOrder {
            price_multiple: price_tick,
            base_lots: lot_size,
            filled_base_lots: 0,
            self_filled: 0,
            common: CommonOrderFields {
                id,
                market_id: 0,
                status: OrderStatus::Open,
                account,
                direction,
            },
        }
    }

    fn make_market_buy_order(id: OrderId, quote_size: u64, account: PublicKeyHash) -> MarketOrder {
        MarketOrder::Buy(MarketBuyOrder {
            quote_size,
            filled_size: 0,
            average_execution_price: 0,
            self_filled: 0,
            common: CommonOrderFields {
                id,
                market_id: 0,
                status: OrderStatus::Open,
                account,
                direction: OrderDirection::Buy,
            },
        })
    }

    fn make_market_sell_order(id: OrderId, base_size: u64, account: PublicKeyHash) -> MarketOrder {
        MarketOrder::Sell(MarketSellOrder {
            base_size,
            filled_size: 0,
            average_execution_price: 0,
            self_filled: 0,
            common: CommonOrderFields {
                id,
                market_id: 0,
                status: OrderStatus::Open,
                account,
                direction: OrderDirection::Sell,
            },
        })
    }

    fn assert_user_execution_result(
        result: UserExecutionResult,
        lots_in: u64,
        lots_out: u64,
        asset_in: u32,
        asset_out: u32,
        filled_size: u64,
    ) {
        assert_eq!(result.lots_out, lots_out);
        assert_eq!(result.lots_in, lots_in);
        assert_eq!(result.asset_in, asset_in);
        assert_eq!(result.asset_out, asset_out);
        assert_eq!(result.filled_size, filled_size);
    }

    impl SpotMarket {
        fn add_limit_helper(&mut self, order: LimitOrder, precision: &MarketPrecision) {
            self.add_limit_order(order, 0, 1, precision);
        }

        fn test_new(tick: u32, tick_decimals: u8) -> Self {
            Self {
                bids_levels: vec![],
                asks_levels: vec![],
                market_id: 0,
                asset_one: 0,
                asset_two: 1,
                base_asset: 0,
                quote_asset: 1,
                tick_decimals,
                tick,
                last_executed_price: None,
                base_asset_name: "".to_string(),
                quote_asset_name: "".to_string(),
            }
        }
    }

    mod test_limit_orders {
        use crate::{
            state::{
                order::{OrderDirection, OrderStatus},
                spot_clearinghouse::MarketPrecision,
                spot_market::{Level, SpotMarket, tests::new_limit},
            },
            types::transaction::PublicKeyHash,
        };

        use super::{make_market_buy_order, make_market_sell_order};

        #[test]
        fn test_add_bid_order_inserts_correctly() {
            let mut market = SpotMarket::test_new(100, 2);

            let mp = MarketPrecision {
                base_lot_size: 100,
                quote_lot_size: 100,
                tick: market.tick,
                tick_decimals: market.tick_decimals,
            };
            let account = PublicKeyHash::default();

            market.add_limit_helper(new_limit(100, 10, OrderDirection::Buy, 1, account), &mp);
            market.add_limit_helper(new_limit(105, 5, OrderDirection::Buy, 2, account), &mp);
            market.add_limit_helper(new_limit(103, 7, OrderDirection::Buy, 3, account), &mp);
            market.add_limit_helper(new_limit(1, 7, OrderDirection::Buy, 4, account), &mp);

            let bids_levels = &market.bids_levels;
            assert_eq!(bids_levels.len(), 4);
            assert_eq!(bids_levels.last().unwrap().price, 105);
            assert_eq!(bids_levels.first().unwrap().price, 1);
            assert_eq!(market.get_best_prices().0, Some(105));
        }

        #[test]
        fn test_add_ask_order_inserts_correctly() {
            let mut market = SpotMarket::test_new(100, 2);
            let mp = MarketPrecision {
                base_lot_size: 100,
                quote_lot_size: 100,
                tick: market.tick,
                tick_decimals: market.tick_decimals,
            };

            let account = PublicKeyHash::default();
            market.add_limit_helper(new_limit(110, 8, OrderDirection::Sell, 1, account), &mp);
            market.add_limit_helper(new_limit(1000, 8, OrderDirection::Sell, 2, account), &mp);
            market.add_limit_helper(new_limit(1000, 10, OrderDirection::Sell, 5, account), &mp);
            market.add_limit_helper(new_limit(107, 6, OrderDirection::Sell, 3, account), &mp);
            market.add_limit_helper(new_limit(109, 4, OrderDirection::Sell, 4, account), &mp);

            assert_eq!(market.asks_levels.len(), 4);
            assert_eq!(market.asks_levels.last().unwrap().price, 107);
            assert_eq!(market.asks_levels.first().unwrap().price, 1000);
            assert_eq!(market.get_best_prices().1, Some(107));

            assert_eq!(market.asks_levels[0].volume, 18); // price 1000
        }

        fn setup_test_market(market: &mut SpotMarket, mp: &MarketPrecision) {
            let mm = [1; 32];
            // Sells
            market.add_limit_helper(new_limit(2_500, 1100, OrderDirection::Sell, 1, mm), mp);
            market.add_limit_helper(new_limit(2_500, 800, OrderDirection::Sell, 2, mm), mp);
            // Cancelled order
            market.add_limit_helper(new_limit(2_550, 1000, OrderDirection::Sell, 3, mm), mp);
            market.add_limit_helper(new_limit(2_550, 600, OrderDirection::Sell, 4, mm), mp);
            market.add_limit_helper(new_limit(2_700, 800, OrderDirection::Sell, 5, mm), mp);
            market.add_limit_helper(new_limit(2_800, 400, OrderDirection::Sell, 6, mm), mp);

            market.cancel_order(&new_limit(2_550, 1000, OrderDirection::Sell, 3, mm));
            // total size

            // Buys
            market.add_limit_helper(new_limit(2_000, 1100, OrderDirection::Buy, 7, mm), &mp);
            market.add_limit_helper(new_limit(2_000, 800, OrderDirection::Buy, 8, mm), &mp);
            // Cancelled order
            market.add_limit_helper(new_limit(2_200, 1_000, OrderDirection::Buy, 9, mm), &mp);
            market.add_limit_helper(new_limit(2_200, 600, OrderDirection::Buy, 10, mm), &mp);
            market.add_limit_helper(new_limit(2_300, 800, OrderDirection::Buy, 11, mm), &mp);
            market.add_limit_helper(new_limit(2400, 400, OrderDirection::Buy, 12, mm), &mp);

            market.cancel_order(&new_limit(2_200, 1000, OrderDirection::Buy, 9, mm));
        }

        mod test_limit_execution {
            use crate::{
                state::{
                    order::{OrderDirection, OrderStatus},
                    spot_clearinghouse::MarketPrecision,
                    spot_market::{
                        SpotMarket,
                        tests::{assert_user_execution_result, new_limit},
                    },
                },
                types::transaction::PublicKeyHash,
            };

            use super::setup_test_market;

            #[test]
            fn test_limit_buy_above_best_ask_price_fully_filled() {
                let base_asset = 0;
                let quote_asset = 1;
                let tick = 100;
                let tick_decimals = 2;
                let mut market = SpotMarket::test_new(tick, tick_decimals);
                let precision = MarketPrecision {
                    base_lot_size: 10,
                    quote_lot_size: 10,
                    tick,
                    tick_decimals,
                };

                setup_test_market(&mut market, &precision);

                let account = PublicKeyHash::default();
                // Check market state before execution
                {
                    assert_eq!(market.get_best_prices(), (Some(2_400), Some(2_500)));
                    let asks_levels = &market.asks_levels;
                    let bids_levels = &market.bids_levels;

                    assert_eq!(asks_levels.len(), 4);
                    assert_eq!(bids_levels.len(), 4);
                    assert_eq!(bids_levels.last().unwrap().price, 2_400);

                    // Check state of the level that limit order will fill to
                    let resting_level = asks_levels.get(2).unwrap();
                    assert_eq!(resting_level.volume, 600);
                    assert_eq!(resting_level.cancelled, 1);
                }

                let order_lot_size = 2_000;
                let order = new_limit(2_550, order_lot_size, OrderDirection::Buy, 7, account);
                let limit_fill_result = market.add_limit_order(order, 0, 1, &precision);

                let Some(result) = limit_fill_result else {
                    panic!("Expected to be some result");
                };

                // Check market state
                {
                    assert_eq!(market.asks_levels.len(), 3);
                    let resting_level = market
                        .asks_levels
                        .last()
                        .expect("Level should not be empty");
                    assert_eq!(resting_level.volume, 500);
                    assert_eq!(resting_level.cancelled, 0);

                    let residual_order =
                        resting_level.orders.get(0).expect("Level should not be 0");

                    assert_eq!(residual_order.filled_base_lots, 100);

                    // new Limit buy should not be inserted
                    assert_eq!(market.bids_levels.len(), 4);
                    assert_eq!(market.bids_levels.last().unwrap().price, 2_400);

                    assert_eq!(market.get_best_prices(), (Some(2_400), Some(2_550)));
                }

                // Check matching result
                {
                    let filled_orders = &result.filled_orders;
                    assert_eq!(filled_orders.len(), 3);
                    // filled quote size should be 0 to calculate token changes
                    assert_eq!(filled_orders[0].filled_base_lots, 0);
                    assert_eq!(filled_orders[1].filled_base_lots, 0);
                    assert_eq!(filled_orders[2].common.status, OrderStatus::Cancelled);

                    let Some(residual_order) = result.residual_order else {
                        panic!("Expected to have a partial fill");
                    };

                    assert_eq!(residual_order.order_id, 4);
                    assert_eq!(residual_order.filled_base_lots, 100);

                    let execution_result = result.user_order;

                    let expected_lots_out = 2500 * 1900 + 2550 * 100;
                    assert_user_execution_result(
                        execution_result,
                        order_lot_size,
                        expected_lots_out,
                        base_asset,
                        quote_asset,
                        order_lot_size,
                    );
                }
            }

            #[test]
            fn test_limit_buy_above_best_ask_price_partial_filled() {
                let base_asset = 0;
                let quote_asset = 1;
                let tick = 100;
                let tick_decimals = 2;
                let mut market = SpotMarket::test_new(tick, tick_decimals);
                let precision = MarketPrecision {
                    base_lot_size: 10,
                    quote_lot_size: 10,
                    tick,
                    tick_decimals,
                };

                setup_test_market(&mut market, &precision);

                let account = PublicKeyHash::default();

                // Check market state before execution
                {
                    assert_eq!(market.get_best_prices(), (Some(2_400), Some(2_500)));
                    assert_eq!(market.asks_levels.len(), 4);

                    assert_eq!(market.bids_levels.len(), 4);
                    assert_eq!(market.bids_levels.last().unwrap().price, 2_400);
                }

                let order = new_limit(2_650, 5_000, OrderDirection::Buy, 7, account);
                let limit_fill_result = market.add_limit_order(order, 0, 1, &precision);

                let Some(result) = limit_fill_result else {
                    panic!("Expected to be some result");
                };

                // Check market state after execution
                {
                    assert_eq!(market.asks_levels.len(), 2);
                    let best_ask_level = market
                        .asks_levels
                        .last()
                        .expect("Level should not be empty");
                    assert_eq!(best_ask_level.price, 2_700);
                    assert_eq!(best_ask_level.volume, 800);
                    assert_eq!(best_ask_level.cancelled, 0);

                    // Order should not be inserted
                    let bids_levels = &market.bids_levels;
                    assert_eq!(bids_levels.len(), 5);
                    let best_bid_level = market.bids_levels.last().unwrap();
                    assert_eq!(best_bid_level.price, 2_650);
                    assert_eq!(best_bid_level.volume, 2_500);

                    assert_eq!(market.get_best_prices(), (Some(2_650), Some(2_700)));
                }

                // Check matching result
                {
                    let filled_orders = &result.filled_orders;
                    assert_eq!(filled_orders.len(), 4);
                    // filled quote size should be 0 to calculate token changes
                    assert_eq!(filled_orders[0].filled_base_lots, 0);
                    assert_eq!(filled_orders[1].filled_base_lots, 0);
                    assert_eq!(filled_orders[2].common.status, OrderStatus::Cancelled);
                    assert_eq!(filled_orders[3].filled_base_lots, 0);

                    if let Some(_) = result.residual_order {
                        panic!("Expected to not have a residual order");
                    };

                    let user_execution_result = result.user_order;

                    let expected_lots_out = 2_500 * 1_900 + 2_550 * 600;
                    assert_user_execution_result(
                        user_execution_result,
                        2_500,
                        expected_lots_out,
                        base_asset,
                        quote_asset,
                        2_500,
                    );
                }
            }

            #[test]
            fn test_limit_buy_above_best_ask_price_fully_consume_book() {
                let base_asset = 0;
                let quote_asset = 1;
                let tick = 100;
                let tick_decimals = 2;
                let mut market = SpotMarket::test_new(tick, tick_decimals);
                let precision = MarketPrecision {
                    base_lot_size: 10,
                    quote_lot_size: 10,
                    tick,
                    tick_decimals,
                };

                // Asks
                setup_test_market(&mut market, &precision);

                let account = PublicKeyHash::default();
                // Check market state before execution
                {
                    let asks_levels = &market.asks_levels;
                    let bids_levels = &market.bids_levels;

                    assert_eq!(market.get_best_prices(), (Some(2_400), Some(2_500)));
                    assert_eq!(asks_levels.len(), 4);
                    assert_eq!(bids_levels.len(), 4);
                    assert_eq!(bids_levels.last().unwrap().price, 2_400);
                }

                // fully consume the order book
                let order = new_limit(3_550, 4_000, OrderDirection::Buy, 7, account);
                let limit_fill_result = market.add_limit_order(order, 0, 1, &precision);

                let Some(result) = limit_fill_result else {
                    panic!("Expected to be some result");
                };

                // Check market state
                {
                    let asks_levels = &market.asks_levels;
                    let bids_levels = &market.bids_levels;
                    assert_eq!(asks_levels.len(), 0);
                    // Buy order should have been inserted
                    assert_eq!(bids_levels.len(), 5);
                    // Buy order is the last level
                    assert_eq!(bids_levels.last().unwrap().volume, 300);
                    assert_eq!(bids_levels.last().unwrap().price, 3_550);

                    assert_eq!(market.get_best_prices(), (Some(3_550), None));
                }

                // Check matching result
                {
                    let filled_orders = &result.filled_orders;
                    assert_eq!(filled_orders.len(), 6);
                    // filled quote size should be 0 to calculate token changes
                    assert_eq!(filled_orders[0].filled_base_lots, 0);
                    assert_eq!(filled_orders[1].filled_base_lots, 0);
                    assert_eq!(filled_orders[2].common.status, OrderStatus::Cancelled);
                    assert_eq!(filled_orders[3].filled_base_lots, 0);
                    assert_eq!(filled_orders[4].filled_base_lots, 0);
                    assert_eq!(filled_orders[5].filled_base_lots, 0);

                    if let Some(_) = result.residual_order {
                        panic!("Expected to have no residual order");
                    };

                    let user_execution_result = result.user_order;

                    let expected_lots_out = 2_500 * 1_900 + 2_550 * 600 + 2_700 * 800 + 2_800 * 400;
                    let expected_lots_in = 3_700;
                    assert_user_execution_result(
                        user_execution_result,
                        expected_lots_in,
                        expected_lots_out,
                        base_asset,
                        quote_asset,
                        expected_lots_in,
                    );
                }
            }

            #[test]
            fn test_sell_limit_above_best_buy_price_fully_executed() {
                let base_asset = 0;
                let quote_asset = 1;
                let tick = 100;
                let tick_decimals = 2;
                let mut market = SpotMarket::test_new(tick, tick_decimals);
                let precision = MarketPrecision {
                    base_lot_size: 10,
                    quote_lot_size: 10,
                    tick,
                    tick_decimals,
                };
                let account = PublicKeyHash::default();

                setup_test_market(&mut market, &precision);

                {
                    let asks_levels = &market.asks_levels;
                    let bids_levels = &market.bids_levels;

                    assert_eq!(market.get_best_prices(), (Some(2_400), Some(2_500)));

                    assert_eq!(asks_levels.len(), 4);
                    assert_eq!(bids_levels.len(), 4);
                    assert_eq!(asks_levels.last().unwrap().price, 2_500);

                    // Check state of the level that limit order will fill to
                    let resting_level = bids_levels.get(1).unwrap();
                    assert_eq!(resting_level.volume, 600); // price_level 2_200
                    assert_eq!(resting_level.cancelled, 1);
                }

                let order_lot_size = 1_400;
                let order = new_limit(2_200, 1_400, OrderDirection::Sell, 7, account);
                let limit_fill_result = market.add_limit_order(order, 0, 1, &precision);

                let Some(result) = limit_fill_result else {
                    panic!("Expected to be some result");
                };

                // Check market state
                {
                    assert_eq!(market.bids_levels.len(), 2);
                    let partial_fill_level = market
                        .bids_levels
                        .last()
                        .expect("Level should not be empty");
                    assert_eq!(partial_fill_level.volume, 400);
                    assert_eq!(partial_fill_level.cancelled, 0); // Should remove cancel

                    let residual_order = partial_fill_level
                        .orders
                        .get(0)
                        .expect("Level should not be 0");

                    assert_eq!(residual_order.filled_base_lots, 200);
                    // new ask order should not be inserted
                    assert_eq!(market.asks_levels.len(), 4);
                    assert_eq!(market.asks_levels.last().unwrap().price, 2500);

                    assert_eq!(market.get_best_prices(), (Some(2_200), Some(2500)));
                }

                // Check matching result
                {
                    let filled_orders = &result.filled_orders;
                    assert_eq!(filled_orders.len(), 3);
                    // filled quote size should be 0 to calculate token changes
                    assert_eq!(filled_orders[0].filled_base_lots, 0);
                    assert_eq!(filled_orders[1].filled_base_lots, 0);
                    assert_eq!(filled_orders[2].common.status, OrderStatus::Cancelled);

                    let Some(residual_order) = result.residual_order else {
                        panic!("Expected to have a residual order");
                    };

                    assert_eq!(residual_order.order_id, 10);
                    assert_eq!(residual_order.filled_base_lots, 200);

                    let user_execution_result = result.user_order;

                    let expected_lots_in = 2_400 * 400 + 2_300 * 800 + 2_200 * 200;
                    assert_user_execution_result(
                        user_execution_result,
                        expected_lots_in,
                        order_lot_size,
                        quote_asset,
                        base_asset,
                        order_lot_size,
                    );
                }
            }

            #[test]
            fn test_sell_limit_above_best_buy_price_partially_filled() {
                let base_asset = 0;
                let quote_asset = 1;
                let tick = 100;
                let tick_decimals = 2;
                let mut market = SpotMarket::test_new(tick, tick_decimals);
                let precision = MarketPrecision {
                    base_lot_size: 10,
                    quote_lot_size: 10,
                    tick,
                    tick_decimals,
                };

                let account = PublicKeyHash::default();
                // Asks

                setup_test_market(&mut market, &precision);

                // Check market state before execution
                {
                    assert_eq!(market.get_best_prices(), (Some(2_400), Some(2_500)));

                    assert_eq!(market.asks_levels.len(), 4);
                    assert_eq!(market.bids_levels.len(), 4);
                }

                let order = new_limit(2_100, 5_000, OrderDirection::Sell, 13, account);
                let limit_fill_result = market.add_limit_order(order, 0, 1, &precision);

                let Some(result) = limit_fill_result else {
                    panic!("Expected to be some result");
                };

                // Check market state
                {
                    let bids_levels = &market.bids_levels;
                    let asks_levels = &market.asks_levels;

                    assert_eq!(bids_levels.len(), 1);
                    assert_eq!(bids_levels[0].price, 2_000);
                    assert_eq!(bids_levels[0].volume, 1_900);

                    // Order should be inserted
                    assert_eq!(asks_levels.len(), 5);
                    let best_asks_levels = market.asks_levels.last().unwrap();
                    assert_eq!(best_asks_levels.price, 2_100);
                    assert_eq!(best_asks_levels.volume, 3_200);

                    assert_eq!(market.get_best_prices(), (Some(2_000), Some(2_100)));
                }

                // Check matching result
                {
                    let filled_orders = &result.filled_orders;
                    assert_eq!(filled_orders.len(), 4);
                    // filled quote size should be 0 to calculate token changes
                    assert_eq!(filled_orders[0].filled_base_lots, 0);
                    assert_eq!(filled_orders[1].filled_base_lots, 0);
                    assert_eq!(filled_orders[2].common.status, OrderStatus::Cancelled);
                    assert_eq!(filled_orders[3].filled_base_lots, 0);

                    if let Some(_) = result.residual_order {
                        panic!("Expected to not have a residual order");
                    };

                    let user_execution_result = result.user_order;

                    let expected_lots_in = 2_400 * 400 + 2_300 * 800 + 2_200 * 600;
                    assert_user_execution_result(
                        user_execution_result,
                        expected_lots_in,
                        1_800,
                        quote_asset,
                        base_asset,
                        1_800,
                    );
                }
            }

            #[test]
            fn test_sell_limit_above_best_buy_price_fully_consume_orderbook() {
                let base_asset = 0;
                let quote_asset = 1;
                let tick = 100;
                let tick_decimals = 2;
                let mut market = SpotMarket::test_new(tick, tick_decimals);
                let precision = MarketPrecision {
                    base_lot_size: 10,
                    quote_lot_size: 10,
                    tick,
                    tick_decimals,
                };

                let account = PublicKeyHash::default();

                setup_test_market(&mut market, &precision);
                // Check market state before execution
                {
                    assert_eq!(market.get_best_prices(), (Some(2_400), Some(2_500)));

                    assert_eq!(market.asks_levels.len(), 4);
                    assert_eq!(market.bids_levels.len(), 4);
                }

                let order = new_limit(2_000, 5_000, OrderDirection::Sell, 7, account);
                let limit_fill_result = market.add_limit_order(order, 0, 1, &precision);

                let Some(result) = limit_fill_result else {
                    panic!("Expected to be some result");
                };

                // Check market state
                {
                    assert_eq!(market.bids_levels.len(), 0);

                    // Order should be inserted
                    assert_eq!(market.asks_levels.len(), 5);
                    assert_eq!(market.asks_levels.last().unwrap().price, 2_000);
                    assert_eq!(market.asks_levels.last().unwrap().volume, 1_300);

                    assert_eq!(market.get_best_prices(), (None, Some(2_000)));
                }

                // Check matching result
                {
                    let filled_orders = &result.filled_orders;
                    assert_eq!(filled_orders.len(), 6);
                    // filled quote size should be 0 to calculate token changes
                    assert_eq!(filled_orders[0].filled_base_lots, 0);
                    assert_eq!(filled_orders[1].filled_base_lots, 0);
                    assert_eq!(filled_orders[2].common.status, OrderStatus::Cancelled);
                    assert_eq!(filled_orders[3].filled_base_lots, 0);
                    assert_eq!(filled_orders[4].filled_base_lots, 0);
                    assert_eq!(filled_orders[5].filled_base_lots, 0);

                    if let Some(_) = result.residual_order {
                        panic!("Expected to not have a residual order");
                    };

                    let user_execution_result = result.user_order;

                    let expected_lots_in = 2_400 * 400 + 2_300 * 800 + 2_200 * 600 + 2_000 * 1_900;
                    assert_user_execution_result(
                        user_execution_result,
                        expected_lots_in,
                        3_700,
                        quote_asset,
                        base_asset,
                        3_700,
                    );
                }
            }
        }

        mod test_market_execution {

            use crate::{
                state::{
                    order::{MarketOrderMatchingResults, OrderDirection, OrderStatus},
                    spot_clearinghouse::MarketPrecision,
                    spot_market::{
                        SpotMarket,
                        tests::{make_market_buy_order, make_market_sell_order, new_limit},
                    },
                },
                types::transaction::PublicKeyHash,
            };

            use super::setup_test_market;

            #[test]
            fn test_market_buy_no_fills() {
                let tick = 100;
                let tick_decimals = 2;
                let mut market = SpotMarket::test_new(tick, tick_decimals);
                let precision = MarketPrecision {
                    base_lot_size: 10,
                    quote_lot_size: 10,
                    tick,
                    tick_decimals,
                };

                let account = PublicKeyHash::default();

                market.add_limit_helper(
                    new_limit(2_500, 1100, OrderDirection::Buy, 1, account),
                    &precision,
                );
                market.add_limit_helper(
                    new_limit(2_500, 800, OrderDirection::Buy, 2, account),
                    &precision,
                );

                // Check market state before execution
                {
                    assert_eq!(market.get_best_prices(), (Some(2_500), None));
                }

                let order = make_market_buy_order(3, 2000, account);
                let market_result = market.handle_market_order(order, &precision);

                match market_result {
                    MarketOrderMatchingResults::Sell { .. } => panic!("Expected Buy"),
                    MarketOrderMatchingResults::Buy {
                        order_id,
                        quote_filled_lots,
                        base_lots_in,
                        filled_orders,
                        residual_order,
                        self_fill: _,
                        last_executed_price,
                    } => {
                        assert_eq!(market.get_best_prices(), (Some(2500), None));
                        assert_eq!(order_id, 3);
                        assert_eq!(last_executed_price, None);
                        assert_eq!(quote_filled_lots, 0);
                        assert_eq!(base_lots_in, 0);
                        assert_eq!(filled_orders.len(), 0);
                        assert!(residual_order.is_none());
                    }
                }
            }

            #[test]
            fn test_market_buy_fully_filled_with_residual_order() {
                let tick = 100;
                let tick_decimals = 2;
                let mut market = SpotMarket::test_new(tick, tick_decimals);
                let precision = MarketPrecision {
                    base_lot_size: 10,
                    quote_lot_size: 10,
                    tick,
                    tick_decimals,
                };

                setup_test_market(&mut market, &precision);

                // Check market state before execution
                {
                    assert_eq!(market.get_best_prices(), (Some(2_400), Some(2_500)));

                    assert_eq!(market.asks_levels.len(), 4);
                    assert_eq!(market.bids_levels.len(), 4);
                    assert_eq!(market.bids_levels.last().unwrap().price, 2_400);

                    // Check state of the level that limit order will fill to
                    assert_eq!(market.asks_levels.get(2).unwrap().volume, 600);
                    assert_eq!(market.asks_levels.get(2).unwrap().cancelled, 1);
                }

                let order = make_market_buy_order(3, 5_050_000, PublicKeyHash::default());
                let market_result = market.handle_market_order(order, &precision);

                match market_result {
                    MarketOrderMatchingResults::Sell { .. } => panic!("Expected Buy"),
                    MarketOrderMatchingResults::Buy {
                        order_id,
                        quote_filled_lots,
                        base_lots_in,
                        filled_orders,
                        residual_order,
                        self_fill: _,
                        last_executed_price,
                    } => {
                        // Check market state
                        {
                            assert_eq!(market.get_best_prices(), (Some(2400), Some(2550)));
                            assert_eq!(market.bids_levels.len(), 4);
                            assert_eq!(market.asks_levels.len(), 3);
                            let resting_level = market.asks_levels.last().unwrap();
                            assert_eq!(last_executed_price, Some(resting_level.price));
                            // Should buy 100 of this level;
                            assert_eq!(market.asks_levels.last().unwrap().volume, 483);
                        }

                        assert_eq!(order_id, 3);

                        assert_eq!(quote_filled_lots, 2_500 * 1_900 + 2_550 * 117);
                        assert_eq!(base_lots_in, 1900 + 117);
                        assert_eq!(filled_orders.len(), 3);

                        let Some(residual_order) = residual_order else {
                            panic!("Expected to have residual order");
                        };

                        assert_eq!(residual_order.filled_base_lots, 117);
                        assert_eq!(residual_order.price_multiple, 2_550);
                    }
                }
            }

            #[test]
            fn test_market_buy_fully_filled_with_residual_order_and_self_fill() {
                let tick = 100;
                let tick_decimals = 2;
                let mut market = SpotMarket::test_new(tick, tick_decimals);
                let precision = MarketPrecision {
                    base_lot_size: 10,
                    quote_lot_size: 10,
                    tick,
                    tick_decimals,
                };

                setup_test_market(&mut market, &precision);

                // partially buy 800
                let order = make_market_buy_order(3, 2_000_000, [1; 32]);
                market.handle_market_order(order, &precision);

                // Check market state before execution
                {
                    assert_eq!(market.get_best_prices(), (Some(2_400), Some(2_500)));

                    assert_eq!(market.asks_levels.len(), 4);
                    assert_eq!(market.bids_levels.len(), 4);
                    assert_eq!(market.bids_levels.last().unwrap().price, 2_400);

                    assert_eq!(market.asks_levels.get(3).unwrap().volume, 1100);
                }
                // partially buy 800
                let order = make_market_buy_order(4, 3_000_000, [0; 32]);
                let market_result = market.handle_market_order(order, &precision);

                match market_result {
                    MarketOrderMatchingResults::Sell { .. } => panic!("Expected Buy"),
                    MarketOrderMatchingResults::Buy {
                        order_id,
                        quote_filled_lots,
                        base_lots_in,
                        filled_orders,
                        residual_order,
                        self_fill: _,
                        last_executed_price,
                    } => {
                        // Check market state
                        {
                            assert_eq!(market.get_best_prices(), (Some(2400), Some(2550)));
                            assert_eq!(market.bids_levels.len(), 4);
                            assert_eq!(market.asks_levels.len(), 3);
                            // Should buy 100 of this level;
                            let resting_level = market.asks_levels.last().unwrap();
                            assert_eq!(resting_level.volume, 502);
                            assert_eq!(last_executed_price, Some(resting_level.price));
                        }

                        assert_eq!(order_id, 4);

                        assert_eq!(quote_filled_lots, 2_500 * 1_100 + 2_550 * 98);
                        assert_eq!(base_lots_in, 1100 + 98);
                        assert_eq!(filled_orders.len(), 3);

                        let Some(residual_order) = residual_order else {
                            panic!("Expected to have residual order");
                        };

                        assert_eq!(residual_order.filled_base_lots, 98);
                        assert_eq!(residual_order.price_multiple, 2_550);
                    }
                }
            }

            #[test]
            fn test_market_sell_no_fill() {
                let tick = 100;
                let tick_decimals = 2;
                let mut market = SpotMarket::test_new(tick, tick_decimals);
                let precision = MarketPrecision {
                    base_lot_size: 10,
                    quote_lot_size: 10,
                    tick,
                    tick_decimals,
                };
                let account = PublicKeyHash::default();

                market.add_limit_helper(
                    new_limit(2_500, 1100, OrderDirection::Sell, 1, account),
                    &precision,
                );
                market.add_limit_helper(
                    new_limit(2_500, 800, OrderDirection::Sell, 2, account),
                    &precision,
                );

                // Check market state before execution
                {
                    assert_eq!(market.get_best_prices(), (None, Some(2_500)));
                }

                let order = make_market_sell_order(3, 2400, account);
                let market_result = market.handle_market_order(order, &precision);

                match market_result {
                    MarketOrderMatchingResults::Buy { .. } => panic!("Expected Sell"),
                    MarketOrderMatchingResults::Sell {
                        order_id,
                        base_filled_lots,
                        quote_lots_in,
                        filled_orders,
                        residual_order,
                        self_fill: _,
                        last_executed_price,
                    } => {
                        assert_eq!(market.get_best_prices(), (None, Some(2_500)));
                        assert_eq!(order_id, 3);
                        assert_eq!(last_executed_price, None);

                        assert_eq!(base_filled_lots, 0);
                        assert_eq!(quote_lots_in, 0);
                        assert_eq!(filled_orders.len(), 0);
                        assert!(residual_order.is_none());
                    }
                }
            }

            #[test]
            fn test_market_sell_fully_filled() {
                let tick = 100;
                let tick_decimals = 2;
                let mut market = SpotMarket::test_new(tick, tick_decimals);
                let precision = MarketPrecision {
                    base_lot_size: 10,
                    quote_lot_size: 10,
                    tick,
                    tick_decimals,
                };

                setup_test_market(&mut market, &precision);

                // Check market state before execution
                {
                    assert_eq!(market.get_best_prices(), (Some(2_400), Some(2_500)));

                    assert_eq!(market.asks_levels.len(), 4);
                    assert_eq!(market.bids_levels.len(), 4);
                    assert_eq!(market.bids_levels.last().unwrap().price, 2_400);

                    // Check state of the level that limit order will fill to
                    assert_eq!(market.asks_levels.get(2).unwrap().volume, 600);
                    assert_eq!(market.asks_levels.get(2).unwrap().cancelled, 1);
                }

                let order = make_market_sell_order(3, 1400, PublicKeyHash::default());
                let market_result = market.handle_market_order(order, &precision);

                match market_result {
                    MarketOrderMatchingResults::Buy { .. } => panic!("Expected Sell"),
                    MarketOrderMatchingResults::Sell {
                        order_id,
                        filled_orders,
                        residual_order,
                        base_filled_lots,
                        quote_lots_in,
                        self_fill: _,
                        last_executed_price,
                    } => {
                        // Check market state
                        {
                            assert_eq!(market.get_best_prices(), (Some(2_200), Some(2_500)));
                            assert_eq!(market.bids_levels.len(), 2);
                            assert_eq!(market.asks_levels.len(), 4);
                            // Should buy 100 of this level;
                            let resting_bids_level = market.bids_levels.last().unwrap();
                            assert_eq!(resting_bids_level.volume, 400);
                            assert_eq!(last_executed_price, Some(resting_bids_level.price));
                        }

                        assert_eq!(order_id, 3);

                        assert_eq!(base_filled_lots, 1_400);
                        assert_eq!(quote_lots_in, 2_400 * 400 + 2_300 * 800 + 2_200 * 200);
                        assert_eq!(filled_orders.len(), 3);
                        assert_eq!(
                            filled_orders.get(2).unwrap().common.status,
                            OrderStatus::Cancelled
                        );

                        let Some(residual_order) = residual_order else {
                            panic!("Expected to have residual order");
                        };

                        assert_eq!(residual_order.filled_base_lots, 200);
                        assert_eq!(residual_order.price_multiple, 2_200);
                    }
                }
            }

            #[test]
            fn test_market_sell_fully_filled_with_residual() {
                let tick = 100;
                let tick_decimals = 2;
                let mut market = SpotMarket::test_new(tick, tick_decimals);
                let precision = MarketPrecision {
                    base_lot_size: 10,
                    quote_lot_size: 10,
                    tick,
                    tick_decimals,
                };

                setup_test_market(&mut market, &precision);

                let order = make_market_sell_order(2, 100, [1; 32]);
                market.handle_market_order(order, &precision);
                // Check market state before execution
                {
                    assert_eq!(market.get_best_prices(), (Some(2_400), Some(2_500)));

                    assert_eq!(market.asks_levels.len(), 4);
                    assert_eq!(market.bids_levels.len(), 4);
                    assert_eq!(market.bids_levels.last().unwrap().price, 2_400);
                    assert_eq!(market.bids_levels.last().unwrap().volume, 300);

                    // Check state of the level that limit order will fill to
                    assert_eq!(market.asks_levels.get(2).unwrap().volume, 600);
                    assert_eq!(market.asks_levels.get(2).unwrap().cancelled, 1);
                }

                let order = make_market_sell_order(3, 1400, PublicKeyHash::default());
                let market_result = market.handle_market_order(order, &precision);

                match market_result {
                    MarketOrderMatchingResults::Buy { .. } => panic!("Expected Sell"),
                    MarketOrderMatchingResults::Sell {
                        order_id,
                        filled_orders,
                        residual_order,
                        base_filled_lots,
                        quote_lots_in,
                        self_fill: _,
                        last_executed_price,
                    } => {
                        // Check market state
                        {
                            assert_eq!(market.get_best_prices(), (Some(2_200), Some(2_500)));
                            assert_eq!(market.bids_levels.len(), 2);
                            assert_eq!(market.asks_levels.len(), 4);
                            // Should buy 300 of this level;
                            let resting_bids_level = market.bids_levels.last().unwrap();
                            assert_eq!(resting_bids_level.volume, 300);
                            assert_eq!(last_executed_price, Some(resting_bids_level.price))
                        }

                        assert_eq!(order_id, 3);

                        assert_eq!(base_filled_lots, 1_400);
                        assert_eq!(quote_lots_in, 2_400 * 300 + 2_300 * 800 + 2_200 * 300);
                        assert_eq!(filled_orders.len(), 3);
                        assert_eq!(filled_orders[0].self_filled, 100);
                        assert_eq!(
                            filled_orders.get(2).unwrap().common.status,
                            OrderStatus::Cancelled
                        );

                        let Some(residual_order) = residual_order else {
                            panic!("Expected to have residual order");
                        };

                        assert_eq!(residual_order.filled_base_lots, 300);
                        assert_eq!(residual_order.price_multiple, 2_200);
                    }
                }
            }
        }

        #[test]
        fn test_order_aggregation_on_same_price() {
            let mut market = SpotMarket::test_new(100, 2);
            let precision = MarketPrecision {
                base_lot_size: 100,
                quote_lot_size: 100,
                tick: market.tick,
                tick_decimals: market.tick_decimals,
            };

            let account = PublicKeyHash::default();

            market.add_limit_helper(
                new_limit(100, 10, OrderDirection::Buy, 1, account),
                &precision,
            );
            market.add_limit_helper(
                new_limit(100, 15, OrderDirection::Buy, 2, account),
                &precision,
            );
            assert_eq!(market.bids_levels.len(), 1);
            assert_eq!(market.bids_levels[0].volume, 25);
            assert_eq!(market.bids_levels[0].orders.len(), 2);
        }

        #[test]
        fn test_get_best_prices_returns_none_when_empty() {
            let market = SpotMarket::test_new(100, 2);

            assert_eq!(market.get_best_prices(), (None, None));
        }

        #[test]
        fn test_cancels_ask_order_correctly() {
            let mut market = SpotMarket::test_new(100, 2);
            let mp = MarketPrecision {
                base_lot_size: 100,
                quote_lot_size: 100,
                tick: market.tick,
                tick_decimals: market.tick_decimals,
            };

            let account = PublicKeyHash::default();

            market.add_limit_helper(new_limit(1000, 8, OrderDirection::Sell, 2, account), &mp);
            market.add_limit_helper(new_limit(107, 6, OrderDirection::Sell, 3, account), &mp);
            market.add_limit_helper(new_limit(110, 8, OrderDirection::Sell, 1, account), &mp);
            market.add_limit_helper(new_limit(109, 4, OrderDirection::Sell, 4, account), &mp);
            market.add_limit_helper(new_limit(1000, 10, OrderDirection::Sell, 5, account), &mp);
            market.add_limit_helper(new_limit(1000, 9, OrderDirection::Sell, 6, account), &mp);
            market.add_limit_helper(new_limit(1000, 19, OrderDirection::Sell, 7, account), &mp);
            assert_eq!(market.asks_levels.len(), 4);
            assert_eq!(market.bids_levels.len(), 0);

            market.cancel_order(&new_limit(1000, 8, OrderDirection::Sell, 5, account));
            let price_level = &market.asks_levels[0];
            println!("{:?}", price_level.orders);
            assert_eq!(price_level.orders.len(), 4);
            assert_eq!(price_level.orders[1].common.status, OrderStatus::Cancelled);
            assert_eq!(price_level.cancelled, 1);
        }

        #[test]
        fn test_cancels_buy_order_correctly() {
            let mut market = SpotMarket::test_new(100, 2);
            let mp = MarketPrecision {
                base_lot_size: 100,
                quote_lot_size: 100,
                tick: market.tick,
                tick_decimals: market.tick_decimals,
            };
            let account = PublicKeyHash::default();

            market.add_limit_helper(new_limit(1, 8, OrderDirection::Buy, 1, account), &mp);
            market.add_limit_helper(new_limit(3, 8, OrderDirection::Buy, 2, account), &mp);
            market.add_limit_helper(new_limit(4, 6, OrderDirection::Buy, 3, account), &mp);
            market.add_limit_helper(new_limit(3, 4, OrderDirection::Buy, 4, account), &mp);
            market.add_limit_helper(new_limit(8, 10, OrderDirection::Buy, 5, account), &mp);
            market.add_limit_helper(new_limit(3, 9, OrderDirection::Buy, 6, account), &mp);
            assert_eq!(market.bids_levels.len(), 4);
            assert_eq!(market.asks_levels.len(), 0);

            market.cancel_order(&new_limit(3, 9, OrderDirection::Buy, 6, account));
            let price_level = &market.bids_levels[1];
            assert_eq!(price_level.orders[2].common.status, OrderStatus::Cancelled);
            assert_eq!(price_level.cancelled, 1);
        }

        #[test]
        fn test_prunes_cancelled_orders_correctly() {
            fn assert_bid_level(
                bids_level: &Level,
                cancelled: u32,
                orders_len: usize,
                volume: u64,
            ) {
                assert_eq!(bids_level.cancelled, cancelled);
                assert_eq!(bids_level.orders.len(), orders_len);
                assert_eq!(bids_level.volume, volume);
            }
            let mut market = SpotMarket::test_new(100, 2);
            let mp = MarketPrecision {
                base_lot_size: 100,
                quote_lot_size: 100,
                tick: market.tick,
                tick_decimals: market.tick_decimals,
            };

            let account = PublicKeyHash::default();

            let order_1 = new_limit(1, 8, OrderDirection::Buy, 1, account);
            let order_2 = new_limit(3, 8, OrderDirection::Buy, 2, account);
            let order_3 = new_limit(1, 6, OrderDirection::Buy, 3, account);
            let order_4 = new_limit(1, 4, OrderDirection::Buy, 4, account);
            let order_5 = new_limit(1, 10, OrderDirection::Buy, 5, account);
            let order_6 = new_limit(1, 9, OrderDirection::Buy, 6, account);

            market.add_limit_helper(order_1.clone(), &mp);
            market.add_limit_helper(order_2.clone(), &mp);
            market.add_limit_helper(order_3.clone(), &mp);
            market.add_limit_helper(order_4.clone(), &mp);
            market.add_limit_helper(order_5.clone(), &mp);
            market.add_limit_helper(order_6.clone(), &mp);

            assert_eq!(market.bids_levels.len(), 2);
            assert_eq!(market.asks_levels.len(), 0);

            let mut expected_level_volume = 37;
            assert_eq!(market.bids_levels[0].volume, expected_level_volume);

            market.cancel_order(&order_1);
            expected_level_volume -= order_1.base_lots;
            let bids_levels = &market.bids_levels;
            assert_bid_level(&bids_levels[0], 1, 5, expected_level_volume);

            // try cancel again
            market.cancel_order(&order_1);
            let bids_levels = &market.bids_levels;
            assert_bid_level(&bids_levels[0], 1, 5, expected_level_volume);

            market.cancel_order(&order_5);
            expected_level_volume -= order_5.base_lots;
            let bids_levels = &market.bids_levels;
            assert_bid_level(&bids_levels[0], 2, 5, expected_level_volume);

            // should prune here
            market.cancel_order(&new_limit(1, 9, OrderDirection::Buy, 6, account));
            expected_level_volume -= order_6.base_lots;
            let bids_levels = &market.bids_levels;
            assert_bid_level(&bids_levels[0], 0, 2, expected_level_volume);
        }

        #[test]
        fn test_self_fills_buys_correctly() {
            let mut market = SpotMarket::test_new(100, 2);
            let mp = MarketPrecision {
                base_lot_size: 100,
                quote_lot_size: 100,
                tick: market.tick,
                tick_decimals: market.tick_decimals,
            };

            let account = PublicKeyHash::default();

            let order_1 = new_limit(1, 8, OrderDirection::Buy, 1, account);
            let order_2 = new_limit(3, 8, OrderDirection::Buy, 2, account);
            let order_3 = new_limit(1, 6, OrderDirection::Buy, 3, account);
            let order_4 = new_limit(1, 4, OrderDirection::Buy, 4, account);
            let order_5 = new_limit(1, 10, OrderDirection::Buy, 5, account);
            let order_6 = new_limit(1, 9, OrderDirection::Buy, 6, account);

            market.add_limit_helper(order_1.clone(), &mp);
            market.add_limit_helper(order_2.clone(), &mp);
            market.add_limit_helper(order_3.clone(), &mp);
            market.add_limit_helper(order_4.clone(), &mp);
            market.add_limit_helper(order_5.clone(), &mp);
            market.add_limit_helper(order_6.clone(), &mp);

            let order = make_market_sell_order(3, 40, account);
            let market_result = market.handle_market_order(order, &mp);

            match market_result {
                crate::state::order::MarketOrderMatchingResults::Sell {
                    order_id: _,
                    base_filled_lots,
                    quote_lots_in,
                    self_fill,
                    filled_orders,
                    residual_order,
                    last_executed_price,
                } => {
                    assert_eq!(self_fill, 40);
                    assert_eq!(base_filled_lots, 0);
                    assert_eq!(quote_lots_in, 0);
                    assert_eq!(filled_orders.len(), 5);
                    assert_eq!(last_executed_price, None);

                    assert_eq!(filled_orders[0].self_filled, 8);
                    assert_eq!(filled_orders[1].self_filled, 8);
                    assert_eq!(filled_orders[2].self_filled, 6);
                    assert_eq!(filled_orders[3].self_filled, 4);
                    assert_eq!(filled_orders[4].self_filled, 10);

                    assert_eq!(filled_orders[0].filled_base_lots, 0);
                    assert_eq!(filled_orders[1].filled_base_lots, 0);
                    assert_eq!(filled_orders[2].filled_base_lots, 0);
                    assert_eq!(filled_orders[3].filled_base_lots, 0);
                    assert_eq!(filled_orders[4].filled_base_lots, 0);

                    assert_eq!(market.bids_levels[0].orders[0].self_filled, 4);

                    let residual = residual_order.unwrap();
                    assert_eq!(residual.self_fill, 4);
                    assert_eq!(residual.filled_base_lots, 0);
                }
                crate::state::order::MarketOrderMatchingResults::Buy { .. } => {
                    panic!("Expected Sell")
                }
            }
        }

        #[test]
        fn test_self_fills_sell_correctly() {
            let mut market = SpotMarket::test_new(100, 2);
            let mp = MarketPrecision {
                base_lot_size: 100,
                quote_lot_size: 100,
                tick: market.tick,
                tick_decimals: market.tick_decimals,
            };

            let account = PublicKeyHash::default();

            let order_1 = new_limit(10, 8, OrderDirection::Sell, 1, account);
            let order_2 = new_limit(30, 9, OrderDirection::Sell, 2, account);
            let order_3 = new_limit(10, 6, OrderDirection::Sell, 3, account);
            let order_4 = new_limit(10, 4, OrderDirection::Sell, 4, account);
            let order_5 = new_limit(10, 10, OrderDirection::Sell, 5, account);
            let order_6 = new_limit(10, 9, OrderDirection::Sell, 6, account);

            market.add_limit_helper(order_1.clone(), &mp);
            market.add_limit_helper(order_2.clone(), &mp);
            market.add_limit_helper(order_3.clone(), &mp);
            market.add_limit_helper(order_4.clone(), &mp);
            market.add_limit_helper(order_5.clone(), &mp);
            market.add_limit_helper(order_6.clone(), &mp);

            let order = make_market_buy_order(3, 400, account);
            let market_result = market.handle_market_order(order, &mp);

            match market_result {
                crate::state::order::MarketOrderMatchingResults::Buy {
                    order_id: _,

                    self_fill,
                    filled_orders,
                    residual_order,
                    quote_filled_lots,
                    base_lots_in,
                    last_executed_price,
                } => {
                    assert_eq!(self_fill, 400);
                    assert_eq!(base_lots_in, 0);
                    assert_eq!(quote_filled_lots, 0);
                    assert_eq!(filled_orders.len(), 5);
                    assert_eq!(last_executed_price, None);

                    assert_eq!(filled_orders[0].self_filled, 8); // 80
                    assert_eq!(filled_orders[1].self_filled, 6); // 60 
                    assert_eq!(filled_orders[2].self_filled, 4); // 40
                    assert_eq!(filled_orders[3].self_filled, 10); // 100
                    assert_eq!(filled_orders[4].self_filled, 9); // 90

                    assert_eq!(filled_orders[0].filled_base_lots, 0);
                    assert_eq!(filled_orders[1].filled_base_lots, 0);
                    assert_eq!(filled_orders[2].filled_base_lots, 0);
                    assert_eq!(filled_orders[3].filled_base_lots, 0);
                    assert_eq!(filled_orders[4].filled_base_lots, 0);

                    assert_eq!(market.asks_levels[0].orders[0].self_filled, 1);

                    let residual = residual_order.unwrap();
                    assert_eq!(residual.self_fill, 1);
                    assert_eq!(residual.filled_base_lots, 0);
                }
                crate::state::order::MarketOrderMatchingResults::Sell { .. } => {
                    panic!("Expected Buy")
                }
            }
        }
    }
}
