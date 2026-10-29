use nautilus_network::http::Url;

const API_ROOT: &str = "https://api.kite.trade";
const LOGIN_ROOT: &str = "https://kite.zerodha.com";
const WEBSOCKET_ROOT: &str = "wss://ws.kite.trade";

pub(crate) fn api_url(path: &str) -> Url {
    join_url(API_ROOT, path)
}

pub(crate) fn join_login_url(path: &str) -> Url {
    join_url(LOGIN_ROOT, path)
}

pub(crate) fn websocket_url() -> Url {
    parse_url(WEBSOCKET_ROOT)
}

fn join_url(root: &str, path: &str) -> Url {
    parse_url(root)
        .join(path)
        .expect("Kite URL path should be valid")
}

fn parse_url(url: &str) -> Url {
    Url::parse(url).expect("Kite URL should be valid")
}
