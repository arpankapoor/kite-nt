# kite-nt

> [!IMPORTANT]
> Independent project; not affiliated with, endorsed by, or supported by
> Nautech Systems Pty Ltd or its NautilusTrader project, nor by Zerodha Broking
> Ltd.

`kite-nt` is a [Kite Connect](https://zerodha.com/products/api/) adapter for
[NautilusTrader](https://nautilustrader.io/). It implements live data and
execution clients backed by shared Kite Connect HTTP and WebSocket clients.

## Capabilities

The data adapter provides instruments, historical bars, live quotes, order-book
depth, index prices, and open interest. The execution adapter provides account
state, reconciliation, order management, and WebSocket order updates.

It also includes Kite HTTP and WebSocket clients, pluggable instrument snapshot
storage, Kite order models, and Zerodha commission calculation.

## Getting started

See the [crate documentation](https://docs.rs/kite-nt) for authentication,
configuration, and a complete setup example. Authentication follows Kite
Connect's [documented login flow](https://kite.trade/docs/connect/v3/user/).
