use std::collections::HashMap;
use std::sync::Mutex;

use nautilus_core::correctness::CorrectnessError;
use nautilus_execution::models::fee::FeeModel;
use nautilus_model::enums::OptionKind;
use nautilus_model::identifiers::ClientOrderId;
use nautilus_model::instruments::{Instrument, InstrumentAny};
use nautilus_model::orders::{Order, OrderAny};
use nautilus_model::types::{Currency, Money, Price, Quantity};
use rust_decimal::{Decimal, RoundingStrategy, dec};
use serde::Deserialize;
use thiserror::Error;

use crate::common::consts::{VENUE_BFO, VENUE_BSE, VENUE_NFO, VENUE_NSE};
use crate::execution::models::{KiteProduct, KiteTransactionType};
use crate::http::models::KiteInstrumentType;

const BROKERAGE_CAP: Decimal = dec!(20);
const BROKERAGE_RATE: Decimal = dec!(0.0003);
const GST_RATE: Decimal = dec!(0.18);
const NSE_IPFT_RATE: Decimal = dec!(0.000001);
const SEBI_TURNOVER_RATE: Decimal = dec!(0.000001);

const DELIVERY_STT_RATE: Decimal = dec!(0.001);
const DELIVERY_STAMP_DUTY_RATE: Decimal = dec!(0.00015);
const INTRADAY_STT_RATE: Decimal = dec!(0.00025);
const INTRADAY_STAMP_DUTY_RATE: Decimal = dec!(0.00003);
const BSE_EQUITY_TRANSACTION_RATE: Decimal = dec!(0.0000375);
const NSE_EQUITY_TRANSACTION_RATE: Decimal = dec!(0.0000297);

const FUTURES_STT_RATE: Decimal = dec!(0.0005);
const FUTURES_STAMP_DUTY_RATE: Decimal = dec!(0.00002);
const NFO_FUTURES_TRANSACTION_RATE: Decimal = dec!(0.0000183);

const BFO_OPTIONS_TRANSACTION_RATE: Decimal = dec!(0.000325);
const NFO_OPTIONS_TRANSACTION_RATE: Decimal = dec!(0.0003553);
const OPTIONS_STT_RATE: Decimal = dec!(0.0015);
const OPTIONS_STAMP_DUTY_RATE: Decimal = dec!(0.00003);

const STT: &str = "stt";

/// Failure while calculating Kite brokerage or statutory charges.
#[derive(Debug, Error)]
pub enum KiteChargesError {
    /// Turnover cannot be negative.
    #[error("turnover cannot be negative")]
    NegativeTurnover,

    /// The selected Kite product is not supported for the instrument.
    #[error("unsupported Kite product {0:?}")]
    UnsupportedProduct(KiteProduct),

    /// The instrument or venue does not have a supported charge schedule.
    #[error("unsupported Kite fee instrument")]
    UnsupportedInstrument,
}

/// Failure while converting calculated charges into a Nautilus commission.
#[derive(Debug, Error)]
pub enum KiteFeeModelError {
    /// Kite charge calculation failed.
    #[error(transparent)]
    Charges(#[from] KiteChargesError),

    /// A calculated value violated a Nautilus model constraint.
    #[error(transparent)]
    Correctness(#[from] CorrectnessError),
}

/// Itemized charges for an order or cumulative sequence of fills.
///
/// Corresponds to Kite's [charges structure].
///
/// [charges structure]: https://kite.trade/docs/connect/v3/margins/#charges-structure
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct KiteChargeBreakdown {
    /// Total charges after statutory rounding.
    pub total: Decimal,
    /// Securities or commodities transaction tax.
    pub transaction_tax: Decimal,
    /// Name reported for the transaction tax.
    pub transaction_tax_type: String,
    /// Exchange transaction and applicable investor-protection charges.
    pub exchange_turnover_charge: Decimal,
    /// SEBI turnover charge.
    pub sebi_turnover_charge: Decimal,
    /// Zerodha brokerage.
    pub brokerage: Decimal,
    /// Stamp duty.
    pub stamp_duty: Decimal,
    /// Goods and services tax breakdown.
    pub gst: KiteGstBreakdown,
}

/// Goods and services tax components in Kite's [charges structure].
///
/// [charges structure]: https://kite.trade/docs/connect/v3/margins/#charges-structure
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct KiteGstBreakdown {
    /// Integrated GST.
    pub igst: Decimal,
    /// Central GST.
    pub cgst: Decimal,
    /// State GST.
    pub sgst: Decimal,
    /// Total GST across all components.
    pub total: Decimal,
}

/// Calculates Zerodha charges for equity and derivative fills.
///
/// Turnover is tracked by client order ID so brokerage caps apply across all partial fills of an
/// order. A completed order removes its tracked turnover. When an order is partially filled and
/// then cancelled, Nautilus does not notify the fee model of the cancellation, so that order's
/// entry remains until the model is dropped.
///
/// Exercised-long option STT at expiry is not included because ordinary fills do not contain the
/// option's intrinsic value. Rates were verified against Zerodha's calculator on 2026-10-23.
#[derive(Debug)]
pub struct KiteFeeModel {
    equity_product: KiteProduct,
    commissions: KiteCommissionTracker,
}

/// Calculates charges for one cumulative NFO/BFO derivative turnover using
/// Zerodha's calculator as verified on 2026-10-23:
/// <https://zerodha.com/brokerage-calculator>.
pub fn derivative_order_charges(
    instrument_type: KiteInstrumentType,
    venue: &str,
    transaction_type: KiteTransactionType,
    turnover: Decimal,
) -> Result<KiteChargeBreakdown, KiteChargesError> {
    if turnover.is_sign_negative() {
        return Err(KiteChargesError::NegativeTurnover);
    }
    if !matches!(venue, VENUE_NFO | VENUE_BFO) {
        return Err(KiteChargesError::UnsupportedInstrument);
    }

    let (brokerage, transaction_rate, stamp_duty_rate, exchange_rate) = match instrument_type {
        KiteInstrumentType::Future => (
            capped_brokerage(turnover),
            FUTURES_STT_RATE,
            FUTURES_STAMP_DUTY_RATE,
            if venue == VENUE_NFO {
                NFO_FUTURES_TRANSACTION_RATE
            } else {
                Decimal::ZERO
            },
        ),
        KiteInstrumentType::CallOption | KiteInstrumentType::PutOption => (
            flat_brokerage(turnover),
            OPTIONS_STT_RATE,
            OPTIONS_STAMP_DUTY_RATE,
            if venue == VENUE_NFO {
                NFO_OPTIONS_TRANSACTION_RATE
            } else {
                BFO_OPTIONS_TRANSACTION_RATE
            },
        ),
        KiteInstrumentType::Equity => return Err(KiteChargesError::UnsupportedInstrument),
    };
    let transaction_tax = match transaction_type {
        // TODO: Account for exercised-long option STT during expiry settlement; ordinary
        // fills do not contain the option's intrinsic value.
        KiteTransactionType::Buy => Decimal::ZERO,
        KiteTransactionType::Sell => round_paise_then_rupee(turnover * transaction_rate),
    };
    let stamp_duty = match transaction_type {
        KiteTransactionType::Buy => round_paise_then_rupee(turnover * stamp_duty_rate),
        KiteTransactionType::Sell => Decimal::ZERO,
    };
    let exchange_turnover_charge = round_paise(turnover * exchange_rate);

    Ok(KiteChargeBreakdown::new(
        turnover,
        brokerage,
        transaction_tax,
        exchange_turnover_charge,
        stamp_duty,
    ))
}

#[derive(Debug, Default)]
struct KiteCommissionTracker {
    turnover_by_order: Mutex<HashMap<ClientOrderId, Decimal>>,
}

impl KiteChargeBreakdown {
    fn new(
        turnover: Decimal,
        brokerage: Decimal,
        transaction_tax: Decimal,
        exchange_turnover_charge: Decimal,
        stamp_duty: Decimal,
    ) -> Self {
        let sebi_turnover_charge = round_paise(turnover * SEBI_TURNOVER_RATE);
        let gst_total =
            round_paise((brokerage + exchange_turnover_charge + sebi_turnover_charge) * GST_RATE);
        let total = round_paise(
            transaction_tax
                + exchange_turnover_charge
                + sebi_turnover_charge
                + brokerage
                + stamp_duty
                + gst_total,
        );

        Self {
            total,
            transaction_tax,
            transaction_tax_type: STT.to_owned(),
            exchange_turnover_charge,
            sebi_turnover_charge,
            brokerage,
            stamp_duty,
            gst: KiteGstBreakdown {
                igst: gst_total,
                cgst: Decimal::ZERO,
                sgst: Decimal::ZERO,
                total: gst_total,
            },
        }
    }
}

impl KiteFeeModel {
    /// Creates a fee model using the given product for equity fills.
    pub fn new(equity_product: KiteProduct) -> Result<Self, KiteChargesError> {
        match equity_product {
            KiteProduct::CashAndCarry | KiteProduct::Intraday => Ok(Self {
                equity_product,
                commissions: KiteCommissionTracker::default(),
            }),
            product => Err(KiteChargesError::UnsupportedProduct(product)),
        }
    }

    /// Calculates incremental commission from primitive fill values.
    ///
    /// This entry point supports integrations which cannot supply Nautilus Rust order and
    /// instrument objects directly. Native Nautilus callers use [`FeeModel::get_commission`].
    pub fn commission_for_fill(
        &self,
        client_order_id: ClientOrderId,
        instrument_type: KiteInstrumentType,
        venue: &str,
        transaction_type: KiteTransactionType,
        fill_turnover: Decimal,
        is_complete: bool,
    ) -> Result<Money, KiteFeeModelError> {
        self.commissions.commission_for_fill(
            client_order_id,
            fill_turnover,
            is_complete,
            |turnover| match instrument_type {
                KiteInstrumentType::Equity => {
                    equity_order_charges(venue, self.equity_product, transaction_type, turnover)
                }
                KiteInstrumentType::Future
                | KiteInstrumentType::CallOption
                | KiteInstrumentType::PutOption => {
                    derivative_order_charges(instrument_type, venue, transaction_type, turnover)
                }
            },
        )
    }
}

impl FeeModel for KiteFeeModel {
    fn get_commission(
        &self,
        order: &OrderAny,
        fill_quantity: Quantity,
        fill_px: Price,
        instrument: &InstrumentAny,
    ) -> anyhow::Result<Money> {
        let transaction_type = order.order_side().into();
        Ok(self.commissions.commission_for_fill(
            order.client_order_id(),
            fill_px.as_decimal() * fill_quantity.as_decimal(),
            order.filled_qty().as_decimal() + fill_quantity.as_decimal()
                >= order.quantity().as_decimal(),
            |turnover| order_charges(instrument, self.equity_product, transaction_type, turnover),
        )?)
    }
}

impl KiteCommissionTracker {
    fn commission_for_fill(
        &self,
        client_order_id: ClientOrderId,
        fill_turnover: Decimal,
        is_complete: bool,
        charges: impl Fn(Decimal) -> Result<KiteChargeBreakdown, KiteChargesError>,
    ) -> Result<Money, KiteFeeModelError> {
        let mut turnover_by_order = self
            .turnover_by_order
            .lock()
            .expect("commission tracker mutex poisoned");
        let turnover_before = turnover_by_order
            .get(&client_order_id)
            .copied()
            .unwrap_or_default();
        let commission = commission_delta(turnover_before, fill_turnover, charges)?;
        if is_complete {
            turnover_by_order.remove(&client_order_id);
        } else {
            turnover_by_order.insert(client_order_id, turnover_before + fill_turnover);
        }
        Ok(commission)
    }
}

pub(crate) fn fill_commission(
    instrument: &InstrumentAny,
    product: KiteProduct,
    transaction_type: KiteTransactionType,
    turnover_before: Decimal,
    fill_turnover: Decimal,
) -> Result<Money, KiteFeeModelError> {
    validate_product(instrument, product)?;
    commission_delta(turnover_before, fill_turnover, |turnover| {
        order_charges(instrument, product, transaction_type, turnover)
    })
}

fn commission_delta(
    turnover_before: Decimal,
    fill_turnover: Decimal,
    charges: impl Fn(Decimal) -> Result<KiteChargeBreakdown, KiteChargesError>,
) -> Result<Money, KiteFeeModelError> {
    let before = charges(turnover_before)?.total;
    let after = charges(turnover_before + fill_turnover)?.total;
    Ok(Money::from_decimal(after - before, Currency::INR())?)
}

/// Calculates charges for one cumulative NSE/BSE equity turnover using Zerodha's
/// calculator as verified on 2026-10-23:
/// <https://zerodha.com/brokerage-calculator>.
fn equity_order_charges(
    venue: &str,
    product: KiteProduct,
    transaction_type: KiteTransactionType,
    turnover: Decimal,
) -> Result<KiteChargeBreakdown, KiteChargesError> {
    if turnover.is_sign_negative() {
        return Err(KiteChargesError::NegativeTurnover);
    }

    let (brokerage, transaction_tax, stamp_duty) = match (product, transaction_type) {
        (KiteProduct::CashAndCarry, KiteTransactionType::Buy) => (
            Decimal::ZERO,
            round_paise_then_rupee(turnover * DELIVERY_STT_RATE),
            round_paise_then_rupee(turnover * DELIVERY_STAMP_DUTY_RATE),
        ),
        (KiteProduct::CashAndCarry, KiteTransactionType::Sell) => (
            Decimal::ZERO,
            round_paise_then_rupee(turnover * DELIVERY_STT_RATE),
            Decimal::ZERO,
        ),
        (KiteProduct::Intraday, KiteTransactionType::Buy) => (
            capped_brokerage(turnover),
            Decimal::ZERO,
            round_paise_then_rupee(turnover * INTRADAY_STAMP_DUTY_RATE),
        ),
        (KiteProduct::Intraday, KiteTransactionType::Sell) => (
            capped_brokerage(turnover),
            round_paise_then_rupee(turnover * INTRADAY_STT_RATE),
            Decimal::ZERO,
        ),
        (product, _) => return Err(KiteChargesError::UnsupportedProduct(product)),
    };
    let exchange_turnover_charge = match venue {
        VENUE_NSE => round_paise(
            round_paise(turnover * NSE_EQUITY_TRANSACTION_RATE)
                + round_paise(turnover * NSE_IPFT_RATE),
        ),
        VENUE_BSE => round_paise(turnover * BSE_EQUITY_TRANSACTION_RATE),
        _ => return Err(KiteChargesError::UnsupportedInstrument),
    };
    Ok(KiteChargeBreakdown::new(
        turnover,
        brokerage,
        transaction_tax,
        exchange_turnover_charge,
        stamp_duty,
    ))
}

fn order_charges(
    instrument: &InstrumentAny,
    product: KiteProduct,
    transaction_type: KiteTransactionType,
    turnover: Decimal,
) -> Result<KiteChargeBreakdown, KiteChargesError> {
    match instrument {
        InstrumentAny::Equity(_) => equity_order_charges(
            instrument.id().venue.as_str(),
            product,
            transaction_type,
            turnover,
        ),
        InstrumentAny::FuturesContract(_) => derivative_order_charges(
            KiteInstrumentType::Future,
            instrument.id().venue.as_str(),
            transaction_type,
            turnover,
        ),
        InstrumentAny::OptionContract(option) => {
            let instrument_type = match option.option_kind {
                OptionKind::Call => KiteInstrumentType::CallOption,
                OptionKind::Put => KiteInstrumentType::PutOption,
            };
            derivative_order_charges(
                instrument_type,
                instrument.id().venue.as_str(),
                transaction_type,
                turnover,
            )
        }
        _ => Err(KiteChargesError::UnsupportedInstrument),
    }
}

fn validate_product(
    instrument: &InstrumentAny,
    product: KiteProduct,
) -> Result<(), KiteChargesError> {
    match (instrument, product) {
        (InstrumentAny::Equity(_), KiteProduct::CashAndCarry | KiteProduct::Intraday)
        | (
            InstrumentAny::FuturesContract(_) | InstrumentAny::OptionContract(_),
            KiteProduct::Intraday | KiteProduct::Normal,
        ) => Ok(()),
        (
            InstrumentAny::Equity(_)
            | InstrumentAny::FuturesContract(_)
            | InstrumentAny::OptionContract(_),
            product,
        ) => Err(KiteChargesError::UnsupportedProduct(product)),
        _ => Err(KiteChargesError::UnsupportedInstrument),
    }
}

fn capped_brokerage(turnover: Decimal) -> Decimal {
    round_paise((turnover * BROKERAGE_RATE).min(BROKERAGE_CAP))
}

fn flat_brokerage(turnover: Decimal) -> Decimal {
    if turnover.is_zero() {
        Decimal::ZERO
    } else {
        BROKERAGE_CAP
    }
}

fn round_paise(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

fn round_paise_then_rupee(value: Decimal) -> Decimal {
    round_rupee(round_paise(value))
}

fn round_rupee(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
}

#[cfg(test)]
mod tests {
    use nautilus_execution::models::fee::FeeModel;
    use nautilus_model::enums::{OrderSide, OrderType};
    use nautilus_model::identifiers::{ClientOrderId, InstrumentId};
    use nautilus_model::instruments::stubs::futures_contract_es;
    use nautilus_model::instruments::{Instrument, InstrumentAny};
    use nautilus_model::orders::OrderTestBuilder;
    use nautilus_model::orders::stubs::TestOrderStubs;
    use nautilus_model::types::{Currency, Price, Quantity};
    use rust_decimal::{Decimal, dec};

    use crate::common::consts::{VENUE_BFO, VENUE_BSE, VENUE_NFO, VENUE_NSE};
    use crate::execution::charges::{
        KiteChargeBreakdown, KiteChargesError, KiteCommissionTracker, KiteFeeModel,
        KiteFeeModelError, KiteGstBreakdown, derivative_order_charges, equity_order_charges,
        fill_commission,
    };
    use crate::execution::models::{KiteProduct, KiteTransactionType};
    use crate::http::models::KiteInstrumentType;

    #[test]
    fn calculates_nse_equity_charges() {
        let turnover = dec!(100_000);
        let delivery_buy = equity_order_charges(
            VENUE_NSE,
            KiteProduct::CashAndCarry,
            KiteTransactionType::Buy,
            turnover,
        )
        .unwrap();
        let intraday_sell = equity_order_charges(
            VENUE_NSE,
            KiteProduct::Intraday,
            KiteTransactionType::Sell,
            turnover,
        )
        .unwrap();
        let delivery_sell = equity_order_charges(
            VENUE_NSE,
            KiteProduct::CashAndCarry,
            KiteTransactionType::Sell,
            turnover,
        )
        .unwrap();
        let intraday_buy = equity_order_charges(
            VENUE_NSE,
            KiteProduct::Intraday,
            KiteTransactionType::Buy,
            turnover,
        )
        .unwrap();

        assert_eq!(
            delivery_buy,
            KiteChargeBreakdown {
                total: dec!(118.74),
                transaction_tax: dec!(100),
                transaction_tax_type: "stt".to_owned(),
                exchange_turnover_charge: dec!(3.07),
                sebi_turnover_charge: dec!(0.10),
                brokerage: Decimal::ZERO,
                stamp_duty: dec!(15),
                gst: KiteGstBreakdown {
                    igst: dec!(0.57),
                    cgst: Decimal::ZERO,
                    sgst: Decimal::ZERO,
                    total: dec!(0.57),
                },
            }
        );
        assert_eq!(
            delivery_sell,
            KiteChargeBreakdown {
                total: dec!(103.74),
                transaction_tax: dec!(100),
                transaction_tax_type: "stt".to_owned(),
                exchange_turnover_charge: dec!(3.07),
                sebi_turnover_charge: dec!(0.10),
                brokerage: Decimal::ZERO,
                stamp_duty: Decimal::ZERO,
                gst: KiteGstBreakdown {
                    igst: dec!(0.57),
                    cgst: Decimal::ZERO,
                    sgst: Decimal::ZERO,
                    total: dec!(0.57),
                },
            }
        );
        assert_eq!(
            intraday_buy,
            KiteChargeBreakdown {
                total: dec!(30.34),
                transaction_tax: Decimal::ZERO,
                transaction_tax_type: "stt".to_owned(),
                exchange_turnover_charge: dec!(3.07),
                sebi_turnover_charge: dec!(0.10),
                brokerage: dec!(20),
                stamp_duty: dec!(3),
                gst: KiteGstBreakdown {
                    igst: dec!(4.17),
                    cgst: Decimal::ZERO,
                    sgst: Decimal::ZERO,
                    total: dec!(4.17),
                },
            }
        );
        assert_eq!(
            intraday_sell,
            KiteChargeBreakdown {
                total: dec!(52.34),
                transaction_tax: dec!(25),
                transaction_tax_type: "stt".to_owned(),
                exchange_turnover_charge: dec!(3.07),
                sebi_turnover_charge: dec!(0.10),
                brokerage: dec!(20),
                stamp_duty: Decimal::ZERO,
                gst: KiteGstBreakdown {
                    igst: dec!(4.17),
                    cgst: Decimal::ZERO,
                    sgst: Decimal::ZERO,
                    total: dec!(4.17),
                },
            }
        );
        assert_eq!(
            equity_order_charges(
                VENUE_NSE,
                KiteProduct::CashAndCarry,
                KiteTransactionType::Buy,
                Decimal::ZERO,
            )
            .unwrap()
            .total,
            Decimal::ZERO
        );
        assert!(matches!(
            equity_order_charges(
                VENUE_NSE,
                KiteProduct::CashAndCarry,
                KiteTransactionType::Buy,
                Decimal::NEGATIVE_ONE,
            ),
            Err(KiteChargesError::NegativeTurnover)
        ));
        assert!(matches!(
            equity_order_charges(
                VENUE_NSE,
                KiteProduct::Normal,
                KiteTransactionType::Buy,
                turnover,
            ),
            Err(KiteChargesError::UnsupportedProduct(KiteProduct::Normal))
        ));
    }

    #[test]
    fn calculates_bse_equity_charges() {
        assert_eq!(
            equity_order_charges(
                VENUE_BSE,
                KiteProduct::CashAndCarry,
                KiteTransactionType::Buy,
                dec!(100_000),
            )
            .unwrap(),
            KiteChargeBreakdown {
                total: dec!(119.54),
                transaction_tax: dec!(100),
                transaction_tax_type: "stt".to_owned(),
                exchange_turnover_charge: dec!(3.75),
                sebi_turnover_charge: dec!(0.10),
                brokerage: Decimal::ZERO,
                stamp_duty: dec!(15),
                gst: KiteGstBreakdown {
                    igst: dec!(0.69),
                    cgst: Decimal::ZERO,
                    sgst: Decimal::ZERO,
                    total: dec!(0.69),
                },
            }
        );
    }

    #[test]
    fn calculates_nfo_derivative_charges() {
        let mut future = futures_contract_es(None, None);
        future.id = InstrumentId::from(format!("NIFTY26AUGFUT.{VENUE_NFO}"));
        let future = InstrumentAny::FuturesContract(future);
        let future_buy = derivative_order_charges(
            KiteInstrumentType::Future,
            VENUE_NFO,
            KiteTransactionType::Buy,
            dec!(100_000),
        )
        .unwrap();
        let future_sell = derivative_order_charges(
            KiteInstrumentType::Future,
            VENUE_NFO,
            KiteTransactionType::Sell,
            dec!(100_000),
        )
        .unwrap();

        assert_eq!(
            future_buy,
            KiteChargeBreakdown {
                total: dec!(27.88),
                transaction_tax: Decimal::ZERO,
                transaction_tax_type: "stt".to_owned(),
                exchange_turnover_charge: dec!(1.83),
                sebi_turnover_charge: dec!(0.10),
                brokerage: dec!(20),
                stamp_duty: dec!(2),
                gst: KiteGstBreakdown {
                    igst: dec!(3.95),
                    cgst: Decimal::ZERO,
                    sgst: Decimal::ZERO,
                    total: dec!(3.95),
                },
            }
        );
        assert_eq!(
            future_sell,
            KiteChargeBreakdown {
                total: dec!(75.88),
                transaction_tax: dec!(50),
                transaction_tax_type: "stt".to_owned(),
                exchange_turnover_charge: dec!(1.83),
                sebi_turnover_charge: dec!(0.10),
                brokerage: dec!(20),
                stamp_duty: Decimal::ZERO,
                gst: KiteGstBreakdown {
                    igst: dec!(3.95),
                    cgst: Decimal::ZERO,
                    sgst: Decimal::ZERO,
                    total: dec!(3.95),
                },
            }
        );
        assert_eq!(
            KiteFeeModel::new(KiteProduct::CashAndCarry)
                .unwrap()
                .get_commission(
                    &TestOrderStubs::make_accepted_order(
                        &OrderTestBuilder::new(OrderType::Market)
                            .instrument_id(future.id())
                            .side(OrderSide::Sell)
                            .quantity(Quantity::from(100))
                            .build(),
                    ),
                    Quantity::from(100),
                    Price::from("1000"),
                    &future,
                )
                .unwrap()
                .as_decimal(),
            future_sell.total,
        );
        assert!(matches!(
            fill_commission(
                &future,
                KiteProduct::CashAndCarry,
                KiteTransactionType::Sell,
                Decimal::ZERO,
                dec!(100_000),
            ),
            Err(KiteFeeModelError::Charges(
                KiteChargesError::UnsupportedProduct(KiteProduct::CashAndCarry)
            ))
        ));

        let option_buy = derivative_order_charges(
            KiteInstrumentType::CallOption,
            VENUE_NFO,
            KiteTransactionType::Buy,
            dec!(100_000),
        )
        .unwrap();
        let option_sell = derivative_order_charges(
            KiteInstrumentType::CallOption,
            VENUE_NFO,
            KiteTransactionType::Sell,
            dec!(100_000),
        )
        .unwrap();

        assert_eq!(
            option_buy,
            KiteChargeBreakdown {
                total: dec!(68.64),
                transaction_tax: Decimal::ZERO,
                transaction_tax_type: "stt".to_owned(),
                exchange_turnover_charge: dec!(35.53),
                sebi_turnover_charge: dec!(0.10),
                brokerage: dec!(20),
                stamp_duty: dec!(3),
                gst: KiteGstBreakdown {
                    igst: dec!(10.01),
                    cgst: Decimal::ZERO,
                    sgst: Decimal::ZERO,
                    total: dec!(10.01),
                },
            }
        );
        assert_eq!(
            option_sell,
            KiteChargeBreakdown {
                total: dec!(215.64),
                transaction_tax: dec!(150),
                transaction_tax_type: "stt".to_owned(),
                exchange_turnover_charge: dec!(35.53),
                sebi_turnover_charge: dec!(0.10),
                brokerage: dec!(20),
                stamp_duty: Decimal::ZERO,
                gst: KiteGstBreakdown {
                    igst: dec!(10.01),
                    cgst: Decimal::ZERO,
                    sgst: Decimal::ZERO,
                    total: dec!(10.01),
                },
            }
        );
        assert_eq!(
            KiteFeeModel::new(KiteProduct::CashAndCarry)
                .unwrap()
                .commission_for_fill(
                    ClientOrderId::from("O-OPTION"),
                    KiteInstrumentType::CallOption,
                    VENUE_NFO,
                    KiteTransactionType::Sell,
                    dec!(100_000),
                    true,
                )
                .unwrap()
                .as_decimal(),
            option_sell.total,
        );
    }

    #[test]
    fn calculates_bfo_option_charges() {
        assert_eq!(
            derivative_order_charges(
                KiteInstrumentType::CallOption,
                VENUE_BFO,
                KiteTransactionType::Buy,
                dec!(100_000),
            )
            .unwrap(),
            KiteChargeBreakdown {
                total: dec!(65.07),
                transaction_tax: Decimal::ZERO,
                transaction_tax_type: "stt".to_owned(),
                exchange_turnover_charge: dec!(32.50),
                sebi_turnover_charge: dec!(0.10),
                brokerage: dec!(20),
                stamp_duty: dec!(3),
                gst: KiteGstBreakdown {
                    igst: dec!(9.47),
                    cgst: Decimal::ZERO,
                    sgst: Decimal::ZERO,
                    total: dec!(9.47),
                },
            }
        );
    }

    #[test]
    fn charges_partial_fills_against_cumulative_order_turnover() {
        let tracker = KiteCommissionTracker::default();
        let full_fill_tracker = KiteCommissionTracker::default();
        let client_order_id = ClientOrderId::from("O-001");
        let charges = |turnover| {
            equity_order_charges(
                VENUE_NSE,
                KiteProduct::Intraday,
                KiteTransactionType::Buy,
                turnover,
            )
        };

        let first = tracker
            .commission_for_fill(client_order_id, dec!(50_000), false, charges)
            .unwrap();
        let second = tracker
            .commission_for_fill(client_order_id, dec!(50_000), true, charges)
            .unwrap();
        let full = full_fill_tracker
            .commission_for_fill(client_order_id, dec!(100_000), true, charges)
            .unwrap();

        assert_eq!(first.as_decimal(), dec!(21.58));
        assert_eq!(second.as_decimal(), dec!(8.76));
        assert_eq!(first.as_decimal() + second.as_decimal(), full.as_decimal());
        assert_eq!(first.currency, Currency::INR());
        assert_eq!(second.currency, Currency::INR());
    }
}
