use jiff::civil::DateTime;
use nautilus_common::messages::execution::ModifyOrder;
use nautilus_core::correctness::CorrectnessError;
use nautilus_core::datetime::NANOSECONDS_IN_MINUTE;
use nautilus_core::{Params, UnixNanos};
use nautilus_model::enums::{LiquiditySide, OrderStatus, OrderType, PositionSide, TimeInForce};
use nautilus_model::identifiers::{AccountId, ClientOrderId, InstrumentId, TradeId, VenueOrderId};
use nautilus_model::instruments::{Instrument, InstrumentAny};
use nautilus_model::orders::{Order, OrderAny};
use nautilus_model::reports::{FillReport, OrderStatusReport, PositionStatusReport};
use nautilus_model::types::{AccountBalance, Currency, MarginBalance, Money, Quantity};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use thiserror::Error;

use crate::common::consts::{INDIA_TIME_ZONE, VENUE_BFO, VENUE_BSE, VENUE_NFO, VENUE_NSE};
use crate::common::parse::{instrument_price, instrument_quantity};
use crate::execution::models::{
    KITE_PRODUCT_PARAM, KiteModifyOrderRequest, KiteOrderRecord, KiteOrderStatus, KiteOrderType,
    KiteOrderUpdate, KitePlaceOrderRequest, KiteProduct, KiteTradeRecord, KiteValidity,
};
use crate::http::models::{KiteHolding, KitePosition, KiteUserMargins};
use crate::instruments::instrument_token;

#[derive(Debug, Error)]
pub(crate) enum KiteExecutionParseError {
    #[error(transparent)]
    Correctness(#[from] CorrectnessError),

    #[error("unsupported execution value: {0}")]
    Unsupported(String),

    #[error("invalid Kite timestamp {value}: {source}")]
    InvalidTimestamp {
        value: DateTime,
        source: jiff::Error,
    },

    #[error("numeric value is outside the supported range")]
    NumericRange,

    #[error("Kite order update {order_id} has no status")]
    MissingOrderStatus { order_id: String },

    #[error("Kite record does not match instrument {instrument_id}: {reason}")]
    InstrumentMismatch {
        instrument_id: InstrumentId,
        reason: String,
    },
}

pub(crate) fn parse_account_state(
    margins: &KiteUserMargins,
) -> Result<(AccountBalance, MarginBalance), KiteExecutionParseError> {
    let segment = &margins.equity;
    let (free, locked, initial_margin) = if segment.enabled {
        (
            segment.net,
            segment.utilised.debits,
            segment.utilised.span + segment.utilised.exposure,
        )
    } else {
        (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
    };
    let currency = Currency::INR();
    let balance = AccountBalance::from_total_and_free(free + locked, free, currency)?;
    // Kite reports SPAN and exposure account-wide, without a maintenance-margin split.
    let margin = MarginBalance::new_checked(
        Money::from_decimal(initial_margin, currency)?,
        Money::zero(currency),
        None,
    )?;

    Ok((balance, margin))
}

pub(crate) fn parse_order_status_report(
    order: &KiteOrderRecord,
    instrument: &InstrumentAny,
    account_id: AccountId,
    client_order_id: Option<ClientOrderId>,
    ts_init: UnixNanos,
) -> Result<OrderStatusReport, KiteExecutionParseError> {
    validate_instrument(
        instrument,
        order.instrument_token,
        &order.exchange,
        &order.trading_symbol,
    )?;
    let (ts_accepted, ts_last) = parse_order_timestamps(
        order.order_timestamp,
        order.exchange_timestamp,
        order.exchange_update_timestamp,
    )?;
    let mut report = OrderStatusReport::new(
        account_id,
        instrument.id(),
        client_order_id,
        VenueOrderId::new_checked(&order.order_id)?,
        Some(order.transaction_type.into()),
        order.order_type.into(),
        order.validity.into(),
        parse_order_status(&order.status, order.filled_quantity)?,
        instrument_quantity(Decimal::from(order.quantity), instrument)?,
        instrument_quantity(Decimal::from(order.filled_quantity), instrument)?,
        ts_accepted,
        ts_last,
        ts_init,
        None,
    );

    if !order.price.is_zero() {
        report = report.with_price(instrument_price(order.price, instrument)?);
    }
    if !order.trigger_price.is_zero() {
        report = report.with_trigger_price(instrument_price(order.trigger_price, instrument)?);
    }
    if !order.average_price.is_zero() {
        report = report.with_avg_px(order.average_price);
    }
    if order.disclosed_quantity > 0 {
        report = report.with_display_qty(instrument_quantity(
            Decimal::from(order.disclosed_quantity),
            instrument,
        )?);
    }
    if let (KiteValidity::TimeToLive, Some(minutes)) = (order.validity, order.validity_ttl) {
        let expiry = parse_datetime(order.order_timestamp)?
            .as_u64()
            .checked_add(u64::from(minutes) * NANOSECONDS_IN_MINUTE)
            .ok_or(KiteExecutionParseError::NumericRange)?;
        report = report.with_expire_time(UnixNanos::from(expiry));
    }
    if matches!(
        order.status,
        KiteOrderStatus::Cancelled | KiteOrderStatus::Rejected
    ) && let Some(reason) = order
        .status_message
        .as_ref()
        .or(order.status_message_raw.as_ref())
    {
        // Nautilus exposes one terminal-order reason field named `cancel_reason`.
        report = report.with_cancel_reason(reason.clone());
    }

    Ok(report)
}

pub(crate) fn parse_order_update_report(
    update: &KiteOrderUpdate,
    instrument: &InstrumentAny,
    account_id: AccountId,
    client_order_id: Option<ClientOrderId>,
    ts_init: UnixNanos,
) -> Result<OrderStatusReport, KiteExecutionParseError> {
    validate_instrument(
        instrument,
        update.instrument_token,
        &update.exchange,
        &update.trading_symbol,
    )?;
    let status =
        update
            .status
            .as_ref()
            .ok_or_else(|| KiteExecutionParseError::MissingOrderStatus {
                order_id: update.order_id.clone(),
            })?;
    let (ts_accepted, ts_last) = parse_order_timestamps(
        update.order_timestamp,
        update.exchange_timestamp,
        update.exchange_update_timestamp,
    )?;
    let mut report = OrderStatusReport::new(
        account_id,
        instrument.id(),
        client_order_id,
        VenueOrderId::new_checked(&update.order_id)?,
        Some(update.transaction_type.into()),
        update.order_type.into(),
        update.validity.into(),
        parse_order_status(status, update.filled_quantity)?,
        instrument_quantity(Decimal::from(update.quantity), instrument)?,
        instrument_quantity(Decimal::from(update.filled_quantity), instrument)?,
        ts_accepted,
        ts_last,
        ts_init,
        None,
    );

    if !update.price.is_zero() {
        report = report.with_price(instrument_price(update.price, instrument)?);
    }
    if !update.trigger_price.is_zero() {
        report = report.with_trigger_price(instrument_price(update.trigger_price, instrument)?);
    }
    if !update.average_price.is_zero() {
        report = report.with_avg_px(update.average_price);
    }
    if update.disclosed_quantity > 0 {
        report = report.with_display_qty(instrument_quantity(
            Decimal::from(update.disclosed_quantity),
            instrument,
        )?);
    }
    if matches!(
        status,
        KiteOrderStatus::Cancelled | KiteOrderStatus::Rejected
    ) && let Some(reason) = update
        .status_message
        .as_ref()
        .or(update.status_message_raw.as_ref())
    {
        // Nautilus exposes one terminal-order reason field named `cancel_reason`.
        report = report.with_cancel_reason(reason.clone());
    }

    Ok(report)
}

pub(crate) fn parse_fill_report(
    trade: &KiteTradeRecord,
    instrument: &InstrumentAny,
    account_id: AccountId,
    client_order_id: Option<ClientOrderId>,
    commission: Money,
    ts_init: UnixNanos,
) -> Result<FillReport, KiteExecutionParseError> {
    validate_instrument(
        instrument,
        trade.instrument_token,
        &trade.exchange,
        &trade.trading_symbol,
    )?;

    Ok(FillReport::new(
        account_id,
        instrument.id(),
        VenueOrderId::new_checked(&trade.order_id)?,
        TradeId::new_checked(&trade.trade_id)?,
        trade.transaction_type.into(),
        instrument_quantity(Decimal::from(trade.quantity), instrument)?,
        instrument_price(trade.average_price, instrument)?,
        commission,
        LiquiditySide::NoLiquiditySide,
        client_order_id,
        None,
        parse_datetime(trade.exchange_timestamp)?,
        ts_init,
        None,
    ))
}

pub(crate) fn parse_position_status_report(
    position: &KitePosition,
    instrument: &InstrumentAny,
    account_id: AccountId,
    ts_init: UnixNanos,
) -> Result<PositionStatusReport, KiteExecutionParseError> {
    validate_instrument(
        instrument,
        position.instrument_token,
        &position.exchange,
        &position.trading_symbol,
    )?;
    parse_position(
        position.quantity,
        position.average_price,
        instrument,
        account_id,
        ts_init,
    )
}

pub(crate) fn parse_holding_status_report(
    holding: &KiteHolding,
    instrument: &InstrumentAny,
    account_id: AccountId,
    ts_init: UnixNanos,
) -> Result<PositionStatusReport, KiteExecutionParseError> {
    validate_instrument(
        instrument,
        holding.instrument_token,
        &holding.exchange,
        &holding.trading_symbol,
    )?;
    let quantity = holding
        .quantity
        .checked_add(holding.t1_quantity)
        .ok_or(KiteExecutionParseError::NumericRange)?;
    parse_position(
        quantity,
        holding.average_price,
        instrument,
        account_id,
        ts_init,
    )
}

pub(crate) fn parse_place_order_request(
    order: &OrderAny,
    instrument: &InstrumentAny,
    product: KiteProduct,
    tag: Option<String>,
    ts_submission: UnixNanos,
) -> Result<KitePlaceOrderRequest, KiteExecutionParseError> {
    validate_order_instrument(order.instrument_id(), instrument)?;
    validate_derivative_product(instrument, product)?;
    validate_order_quantity(instrument, order.quantity())?;

    let instrument_id = instrument.id();
    // Kite's regular equity endpoint cannot represent maker-only, position-closing,
    // quote-denominated, or linked/conditional Nautilus order instructions.
    if order.is_post_only()
        || order.is_reduce_only()
        || order.is_quote_quantity()
        || order.contingency_type().is_some()
        || order.order_list_id().is_some()
        || order.parent_order_id().is_some()
    {
        return Err(KiteExecutionParseError::Unsupported(
            "advanced order instructions are not supported".to_owned(),
        ));
    }

    let order_type = match order.order_type() {
        OrderType::Market => KiteOrderType::Market,
        OrderType::Limit => KiteOrderType::Limit,
        OrderType::StopMarket => KiteOrderType::StopMarket,
        OrderType::StopLimit => KiteOrderType::StopLimit,
        order_type => {
            return Err(KiteExecutionParseError::Unsupported(format!(
                "order type {order_type} is not supported"
            )));
        }
    };
    let transaction_type = order.order_side().into();
    let (validity, validity_ttl) = match order.time_in_force() {
        TimeInForce::Day => (KiteValidity::Day, None),
        TimeInForce::Ioc => (KiteValidity::ImmediateOrCancel, None),
        TimeInForce::Gtd => (
            KiteValidity::TimeToLive,
            Some(time_to_live_minutes(order.expire_time(), ts_submission)?),
        ),
        time_in_force => {
            return Err(KiteExecutionParseError::Unsupported(format!(
                "time in force {time_in_force} is not supported"
            )));
        }
    };
    let quantity = kite_order_quantity(order.quantity())?;

    Ok(KitePlaceOrderRequest {
        trading_symbol: instrument.raw_symbol().as_str().to_owned(),
        exchange: instrument_id.venue.as_str().to_owned(),
        transaction_type,
        order_type,
        quantity,
        product,
        price: order.price().map(|price| price.as_decimal()),
        trigger_price: order.trigger_price().map(|price| price.as_decimal()),
        disclosed_quantity: order
            .display_qty()
            .map(|quantity| {
                quantity
                    .as_decimal()
                    .to_u64()
                    .ok_or(KiteExecutionParseError::NumericRange)
            })
            .transpose()?,
        validity,
        validity_ttl,
        iceberg_legs: None,
        iceberg_quantity: None,
        auction_number: None,
        // Let Kite choose a protected limit for otherwise unbounded market execution.
        market_protection: matches!(
            order_type,
            KiteOrderType::Market | KiteOrderType::StopMarket
        )
        .then_some(-1),
        autoslice: None,
        algo_id: None,
        tag,
    })
}

pub(crate) fn parse_kite_product(
    params: Option<&Params>,
) -> Result<KiteProduct, KiteExecutionParseError> {
    match params.and_then(|params| params.get_str(KITE_PRODUCT_PARAM)) {
        None => Ok(KiteProduct::default()),
        Some(value) => {
            let product = value.parse().map_err(|_| {
                KiteExecutionParseError::Unsupported(format!(
                    "invalid {KITE_PRODUCT_PARAM} {value}"
                ))
            })?;
            match product {
                KiteProduct::CashAndCarry | KiteProduct::Intraday | KiteProduct::Normal => {
                    Ok(product)
                }
                _ => Err(KiteExecutionParseError::Unsupported(format!(
                    "unsupported {KITE_PRODUCT_PARAM} {value}"
                ))),
            }
        }
    }
}

pub(crate) fn parse_modify_order_request(
    cmd: &ModifyOrder,
    instrument: &InstrumentAny,
) -> Result<KiteModifyOrderRequest, KiteExecutionParseError> {
    validate_order_instrument(cmd.instrument_id, instrument)?;
    if let Some(quantity) = cmd.quantity {
        validate_order_quantity(instrument, quantity)?;
    }
    let quantity = cmd.quantity.map(kite_order_quantity).transpose()?;
    let price = cmd.price.map(|price| price.as_decimal());
    let trigger_price = cmd.trigger_price.map(|price| price.as_decimal());
    if quantity.is_none() && price.is_none() && trigger_price.is_none() {
        return Err(KiteExecutionParseError::Unsupported(
            "Kite modification requires quantity, price, or trigger price".to_owned(),
        ));
    }

    Ok(KiteModifyOrderRequest {
        order_id: None,
        parent_order_id: None,
        order_type: None,
        quantity,
        price,
        trigger_price,
        disclosed_quantity: None,
        validity: None,
        market_protection: None,
    })
}

fn parse_position(
    signed_quantity: i64,
    average_price: Decimal,
    instrument: &InstrumentAny,
    account_id: AccountId,
    ts_init: UnixNanos,
) -> Result<PositionStatusReport, KiteExecutionParseError> {
    let side = match signed_quantity.cmp(&0) {
        std::cmp::Ordering::Greater => PositionSide::Long,
        std::cmp::Ordering::Less => PositionSide::Short,
        std::cmp::Ordering::Equal => PositionSide::Flat,
    };
    let quantity = Decimal::from(signed_quantity.unsigned_abs());

    Ok(PositionStatusReport::new(
        account_id,
        instrument.id(),
        side,
        instrument_quantity(quantity, instrument)?,
        ts_init,
        ts_init,
        None,
        None,
        (!average_price.is_zero()).then_some(average_price),
    ))
}

fn validate_order_instrument(
    requested_instrument_id: InstrumentId,
    instrument: &InstrumentAny,
) -> Result<(), KiteExecutionParseError> {
    let instrument_id = instrument.id();
    let venue = instrument_id.venue.as_str();
    let is_supported = match instrument {
        InstrumentAny::Equity(_) => matches!(venue, VENUE_NSE | VENUE_BSE),
        InstrumentAny::FuturesContract(_) | InstrumentAny::OptionContract(_) => {
            matches!(venue, VENUE_NFO | VENUE_BFO)
        }
        _ => false,
    };
    if !is_supported {
        return Err(KiteExecutionParseError::Unsupported(
            "only NSE/BSE equities and NFO/BFO futures and options are supported".to_owned(),
        ));
    }
    if requested_instrument_id != instrument_id {
        return Err(KiteExecutionParseError::InstrumentMismatch {
            instrument_id,
            reason: format!("request is for {requested_instrument_id}"),
        });
    }
    Ok(())
}

fn validate_derivative_product(
    instrument: &InstrumentAny,
    product: KiteProduct,
) -> Result<(), KiteExecutionParseError> {
    if is_derivative(instrument) && !matches!(product, KiteProduct::Intraday | KiteProduct::Normal)
    {
        return Err(KiteExecutionParseError::Unsupported(
            "Kite derivative orders require MIS or NRML".to_owned(),
        ));
    }
    Ok(())
}

fn validate_order_quantity(
    instrument: &InstrumentAny,
    quantity: Quantity,
) -> Result<(), KiteExecutionParseError> {
    let lot_size = instrument
        .lot_size()
        .map_or(Decimal::ONE, |lot_size| lot_size.as_decimal());
    if quantity.as_decimal() % lot_size != Decimal::ZERO {
        return Err(KiteExecutionParseError::Unsupported(format!(
            "Kite order quantity {quantity} is not a multiple of lot size {lot_size}"
        )));
    }
    Ok(())
}

const fn is_derivative(instrument: &InstrumentAny) -> bool {
    matches!(
        instrument,
        InstrumentAny::FuturesContract(_) | InstrumentAny::OptionContract(_)
    )
}

fn kite_order_quantity(quantity: Quantity) -> Result<u64, KiteExecutionParseError> {
    quantity
        .as_decimal()
        .to_u64()
        .filter(|_| quantity.as_decimal().fract().is_zero())
        .ok_or_else(|| {
            KiteExecutionParseError::Unsupported(
                "Kite order quantity must be a whole number".to_owned(),
            )
        })
}

fn parse_order_status(
    status: &KiteOrderStatus,
    filled_quantity: u64,
) -> Result<OrderStatus, KiteExecutionParseError> {
    let working = if filled_quantity == 0 {
        OrderStatus::Accepted
    } else {
        OrderStatus::PartiallyFilled
    };
    Ok(match status {
        KiteOrderStatus::Open
        | KiteOrderStatus::TriggerPending
        | KiteOrderStatus::Modified
        | KiteOrderStatus::Update => working,
        KiteOrderStatus::Complete => OrderStatus::Filled,
        KiteOrderStatus::Cancelled => OrderStatus::Canceled,
        KiteOrderStatus::Rejected => OrderStatus::Rejected,
        KiteOrderStatus::PutOrderRequestReceived
        | KiteOrderStatus::ValidationPending
        | KiteOrderStatus::OpenPending
        | KiteOrderStatus::AmoRequestReceived => OrderStatus::Submitted,
        KiteOrderStatus::ModifyValidationPending | KiteOrderStatus::ModifyPending => {
            OrderStatus::PendingUpdate
        }
        KiteOrderStatus::CancelPending => OrderStatus::PendingCancel,
        KiteOrderStatus::Other(_) => {
            return Err(KiteExecutionParseError::Unsupported(format!(
                "Kite order status {status:?}"
            )));
        }
    })
}

fn parse_datetime(datetime: DateTime) -> Result<UnixNanos, KiteExecutionParseError> {
    let timestamp = datetime
        .to_zoned(INDIA_TIME_ZONE.clone())
        .map_err(|source| KiteExecutionParseError::InvalidTimestamp {
            value: datetime,
            source,
        })?
        .timestamp();
    Ok(UnixNanos::from(timestamp))
}

fn parse_order_timestamps(
    order_timestamp: DateTime,
    exchange_timestamp: Option<DateTime>,
    exchange_update_timestamp: Option<DateTime>,
) -> Result<(UnixNanos, UnixNanos), KiteExecutionParseError> {
    Ok((
        parse_datetime(exchange_timestamp.unwrap_or(order_timestamp))?,
        parse_datetime(
            exchange_update_timestamp
                .or(exchange_timestamp)
                .unwrap_or(order_timestamp),
        )?,
    ))
}

fn time_to_live_minutes(
    expiry: Option<UnixNanos>,
    ts_submission: UnixNanos,
) -> Result<u32, KiteExecutionParseError> {
    let expiry = expiry.ok_or_else(|| {
        KiteExecutionParseError::Unsupported("GTD order has no expiry".to_owned())
    })?;
    let duration = expiry
        .as_u64()
        .checked_sub(ts_submission.as_u64())
        .filter(|duration| *duration > 0)
        .ok_or_else(|| {
            KiteExecutionParseError::Unsupported(
                "GTD order expiry must be after its submission time".to_owned(),
            )
        })?;
    u32::try_from(duration.div_ceil(NANOSECONDS_IN_MINUTE))
        .map_err(|_| KiteExecutionParseError::NumericRange)
}

fn validate_instrument(
    instrument: &InstrumentAny,
    token: u32,
    exchange: &str,
    trading_symbol: &str,
) -> Result<(), KiteExecutionParseError> {
    let mismatch = if instrument_token(instrument)? != u64::from(token) {
        Some(format!("token does not match {token}"))
    } else if instrument.id().venue.as_str() != exchange {
        Some(format!("venue does not match {exchange}"))
    } else if instrument.raw_symbol().as_str() != trading_symbol {
        Some(format!("symbol does not match {trading_symbol}"))
    } else {
        None
    };
    if let Some(reason) = mismatch {
        return Err(KiteExecutionParseError::InstrumentMismatch {
            instrument_id: instrument.id(),
            reason,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use nautilus_core::UnixNanos;
    use nautilus_core::datetime::NANOSECONDS_IN_MINUTE;
    use nautilus_core::params::Params;
    use nautilus_model::enums::{OrderSide, OrderStatus, OrderType, PositionSide, TimeInForce};
    use nautilus_model::identifiers::{AccountId, ClientOrderId, InstrumentId, Symbol};
    use nautilus_model::instruments::stubs::futures_contract_es;
    use nautilus_model::instruments::{Equity, Instrument, InstrumentAny};
    use nautilus_model::orders::builder::OrderTestBuilder;
    use nautilus_model::types::{Currency, Money, Price, Quantity};
    use rust_decimal::{Decimal, dec};
    use serde_json::json;

    use crate::common::consts::{VENUE_NFO, VENUE_NSE};
    use crate::execution::models::{
        KiteOrderRecord, KiteOrderType, KiteOrderUpdate, KiteProduct, KiteTradeRecord,
    };
    use crate::execution::parse::{
        KiteExecutionParseError, parse_account_state, parse_fill_report,
        parse_holding_status_report, parse_order_status_report, parse_order_update_report,
        parse_place_order_request, parse_position_status_report,
    };
    use crate::http::models::{
        KiteAvailableMargins, KiteHolding, KitePosition, KiteSegmentMargins, KiteUserMargins,
        KiteUtilisedMargins,
    };

    #[test]
    fn maps_equity_segment_account_state_without_commodity_funds() {
        let margins = KiteUserMargins {
            equity: segment(
                dec!(99_725.05),
                dec!(145_706.55),
                dec!(101_989),
                dec!(38_981.25),
            ),
            commodity: segment(dec!(100_661.7), Decimal::ZERO, Decimal::ZERO, Decimal::ZERO),
        };

        let (balance, margin) = parse_account_state(&margins).unwrap();

        assert_eq!(balance.total.as_decimal(), dec!(245_431.60));
        assert_eq!(balance.locked.as_decimal(), dec!(145_706.55));
        assert_eq!(balance.free.as_decimal(), dec!(99_725.05));
        assert_eq!(margin.initial.as_decimal(), dec!(140_970.25));
        assert_eq!(margin.maintenance.as_decimal(), Decimal::ZERO);
        assert_eq!(margin.instrument_id, None);
    }

    #[test]
    fn converts_execution_reports_and_inventory() {
        let instrument = instrument();
        let account_id = AccountId::from("KITE-001");
        let ts_init = UnixNanos::from(1);
        let order: KiteOrderRecord = serde_json::from_value(order_json()).unwrap();
        let report =
            parse_order_status_report(&order, &instrument, account_id, None, ts_init).unwrap();
        assert_eq!(report.order_status, OrderStatus::PartiallyFilled);
        assert_eq!(report.filled_qty.as_decimal(), dec!(4));

        let trade: KiteTradeRecord = serde_json::from_value(json!({
            "trade_id": "10000000",
            "order_id": "200000000000000",
            "exchange_order_id": "300000000000000",
            "tradingsymbol": "SBIN",
            "exchange": VENUE_NSE,
            "instrument_token": 779521,
            "transaction_type": "BUY",
            "product": "CNC",
            "average_price": 420.65,
            "quantity": 1,
            "fill_timestamp": "2021-05-31 16:00:36",
            "order_timestamp": "16:00:36",
            "exchange_timestamp": "2021-05-31 16:00:36"
        }))
        .unwrap();
        let fill = parse_fill_report(
            &trade,
            &instrument,
            account_id,
            None,
            Money::new(0.0, Currency::INR()),
            ts_init,
        )
        .unwrap();
        assert_eq!(fill.trade_id.as_str(), "10000000");
        assert_eq!(fill.order_side, OrderSide::Buy);
        assert_eq!(fill.last_px.as_decimal(), dec!(420.65));

        let position: KitePosition = serde_json::from_value(position_json()).unwrap();
        let position_report =
            parse_position_status_report(&position, &instrument, account_id, ts_init).unwrap();
        assert_eq!(position_report.position_side, PositionSide::Short);
        assert_eq!(position_report.quantity.as_decimal(), dec!(3));

        let holding: KiteHolding = serde_json::from_value(holding_json()).unwrap();
        let holding_report =
            parse_holding_status_report(&holding, &instrument, account_id, ts_init).unwrap();
        assert_eq!(holding_report.quantity.as_decimal(), dec!(3));
    }

    #[test]
    fn converts_order_update_snapshot() {
        let instrument = instrument();
        let client_order_id = ClientOrderId::from("O-1");
        let update: KiteOrderUpdate = serde_json::from_value(order_update_json()).unwrap();

        let report = parse_order_update_report(
            &update,
            &instrument,
            AccountId::from("KITE-001"),
            Some(client_order_id),
            UnixNanos::from(1),
        )
        .unwrap();

        assert_eq!(report.client_order_id, Some(client_order_id));
        assert_eq!(report.order_status, OrderStatus::PartiallyFilled);
        assert_eq!(report.filled_qty.as_decimal(), dec!(4));
        assert_eq!(report.avg_px, Some(dec!(420.65)));
    }

    #[test]
    fn rejects_order_update_without_status() {
        let mut value = order_update_json();
        value["status"] = serde_json::Value::Null;
        let update: KiteOrderUpdate = serde_json::from_value(value).unwrap();

        let error = parse_order_update_report(
            &update,
            &instrument(),
            AccountId::from("KITE-001"),
            None,
            UnixNanos::from(1),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            KiteExecutionParseError::MissingOrderStatus { order_id }
                if order_id == "200000000000000"
        ));
    }

    #[test]
    fn converts_supported_orders_to_kite_requests() {
        let instrument = instrument();
        let market_order = OrderTestBuilder::new(OrderType::Market)
            .instrument_id(instrument.id())
            .side(OrderSide::Buy)
            .quantity(Quantity::from(10))
            .time_in_force(TimeInForce::Day)
            .build();
        let ts_submission = UnixNanos::from(61 * NANOSECONDS_IN_MINUTE);
        let request = parse_place_order_request(
            &market_order,
            &instrument,
            KiteProduct::Intraday,
            None,
            ts_submission,
        )
        .unwrap();
        assert_eq!(request.market_protection, Some(-1));

        let ttl_order = OrderTestBuilder::new(OrderType::Limit)
            .instrument_id(instrument.id())
            .side(OrderSide::Buy)
            .quantity(Quantity::from(10))
            .price(Price::from("420.00"))
            .time_in_force(TimeInForce::Gtd)
            .ts_init(UnixNanos::from(NANOSECONDS_IN_MINUTE))
            .expire_time(UnixNanos::from(121 * NANOSECONDS_IN_MINUTE))
            .build();
        let request = parse_place_order_request(
            &ttl_order,
            &instrument,
            KiteProduct::Intraday,
            None,
            ts_submission,
        )
        .unwrap();

        assert_eq!(request.trading_symbol, "SBIN");
        assert_eq!(request.exchange, VENUE_NSE);
        assert_eq!(request.order_type, KiteOrderType::Limit);
        assert_eq!(request.product, KiteProduct::Intraday);
        assert_eq!(request.quantity, 10);
        assert_eq!(request.validity_ttl, Some(60));
        assert_eq!(request.market_protection, None);
    }

    #[test]
    fn validates_derivative_product_and_lot_size() {
        let mut future = futures_contract_es(None, None);
        future.id = InstrumentId::from("NIFTY26AUGFUT.NFO");
        future.raw_symbol = Symbol::from("NIFTY26AUGFUT");
        future.lot_size = Quantity::from(75);
        let instrument = InstrumentAny::FuturesContract(future);
        let order = |quantity| {
            OrderTestBuilder::new(OrderType::Market)
                .instrument_id(instrument.id())
                .side(OrderSide::Buy)
                .quantity(Quantity::from(quantity))
                .time_in_force(TimeInForce::Day)
                .build()
        };

        assert!(
            parse_place_order_request(
                &order(75),
                &instrument,
                KiteProduct::CashAndCarry,
                None,
                UnixNanos::default(),
            )
            .is_err()
        );
        assert!(
            parse_place_order_request(
                &order(76),
                &instrument,
                KiteProduct::Normal,
                None,
                UnixNanos::default(),
            )
            .is_err()
        );

        let request = parse_place_order_request(
            &order(75),
            &instrument,
            KiteProduct::Normal,
            None,
            UnixNanos::default(),
        )
        .unwrap();
        assert_eq!(request.exchange, VENUE_NFO);
        assert_eq!(request.product, KiteProduct::Normal);
        assert_eq!(request.quantity, 75);
    }

    fn instrument() -> InstrumentAny {
        let info = Params::new([("instrument_token".to_owned(), json!(779521))].into());
        Equity::build_checked(
            "SBIN.NSE".into(),
            "SBIN".into(),
            None,
            Currency::INR(),
            2,
            Price::from("0.05"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(info),
            UnixNanos::default(),
            UnixNanos::default(),
        )
        .unwrap()
        .into()
    }

    fn order_json() -> serde_json::Value {
        json!({
            "order_id": "200000000000000",
            "parent_order_id": null,
            "exchange_order_id": "300000000000000",
            "modified": false,
            "placed_by": "AB1234",
            "variety": "regular",
            "status": "OPEN",
            "tradingsymbol": "SBIN",
            "exchange": VENUE_NSE,
            "instrument_token": 779521,
            "transaction_type": "BUY",
            "order_type": "LIMIT",
            "product": "CNC",
            "validity": "DAY",
            "validity_ttl": null,
            "price": 421,
            "quantity": 10,
            "trigger_price": 0,
            "average_price": 420.65,
            "pending_quantity": 6,
            "filled_quantity": 4,
            "disclosed_quantity": 0,
            "order_timestamp": "2021-05-31 16:00:35",
            "exchange_timestamp": "2021-05-31 16:00:35",
            "exchange_update_timestamp": "2021-05-31 16:00:36",
            "status_message": null,
            "status_message_raw": null,
            "cancelled_quantity": 0,
            "auction_number": null,
            "meta": {},
            "tag": null,
            "tags": null,
            "guid": "guid",
            "market_protection": 0
        })
    }

    fn order_update_json() -> serde_json::Value {
        let mut value = order_json();
        value["status"] = json!("UPDATE");
        value["app_id"] = json!(1234);
        value["user_id"] = json!("AB1234");
        value["unfilled_quantity"] = json!(6);
        value["checksum"] = json!("checksum");
        value
    }

    fn position_json() -> serde_json::Value {
        json!({
            "tradingsymbol": "SBIN", "exchange": VENUE_NSE, "instrument_token": 779521,
            "product": "CNC", "quantity": -3, "overnight_quantity": 0, "multiplier": 1,
            "average_price": 420.65, "close_price": 420, "last_price": 419, "value": 0,
            "pnl": -4.95, "m2m": -4.95, "unrealised": -4.95, "realised": 0,
            "buy_quantity": 0, "buy_price": 0, "buy_value": 0, "buy_m2m": 0,
            "sell_quantity": 3, "sell_price": 420.65, "sell_value": 1261.95, "sell_m2m": 1261.95,
            "day_buy_quantity": 0, "day_buy_price": 0, "day_buy_value": 0,
            "day_sell_quantity": 3, "day_sell_price": 420.65, "day_sell_value": 1261.95
        })
    }

    fn holding_json() -> serde_json::Value {
        json!({
            "tradingsymbol": "SBIN", "exchange": VENUE_NSE, "instrument_token": 779521,
            "isin": "INE062A01020", "product": "CNC", "price": 0, "quantity": 2,
            "used_quantity": 0, "t1_quantity": 1, "realised_quantity": 2,
            "authorised_quantity": 0, "authorised_date": null, "authorisation": {},
            "opening_quantity": 2, "short_quantity": 0, "collateral_quantity": 0,
            "collateral_type": "", "discrepancy": false, "average_price": 400,
            "last_price": 420, "close_price": 419, "pnl": 40, "day_change": 1,
            "day_change_percentage": 0.24,
            "mtf": {"quantity": 0, "used_quantity": 0, "average_price": 0, "value": 0, "initial_margin": 0}
        })
    }

    fn segment(
        net: Decimal,
        debits: Decimal,
        span: Decimal,
        exposure: Decimal,
    ) -> KiteSegmentMargins {
        KiteSegmentMargins {
            enabled: true,
            net,
            available: KiteAvailableMargins {
                cash: Decimal::ZERO,
                opening_balance: Decimal::ZERO,
                live_balance: Decimal::ZERO,
                intraday_payin: Decimal::ZERO,
                adhoc_margin: Decimal::ZERO,
                collateral: Decimal::ZERO,
            },
            utilised: KiteUtilisedMargins {
                m2m_unrealised: Decimal::ZERO,
                m2m_realised: Decimal::ZERO,
                debits,
                span,
                option_premium: Decimal::ZERO,
                holding_sales: Decimal::ZERO,
                exposure,
                liquid_collateral: Decimal::ZERO,
                delivery: Decimal::ZERO,
                stock_collateral: Decimal::ZERO,
                turnover: Decimal::ZERO,
                payout: Decimal::ZERO,
            },
        }
    }
}
