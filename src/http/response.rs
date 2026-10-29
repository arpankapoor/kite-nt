use nautilus_network::http::HttpResponse;
use serde::de::DeserializeOwned;

use crate::http::error::KiteHttpError;
use crate::http::models::KiteResponse;

pub(crate) fn decode<T>(response: HttpResponse) -> Result<T, KiteHttpError>
where
    T: DeserializeOwned,
{
    match serde_json::from_slice::<KiteResponse<T>>(&response.body)? {
        KiteResponse::Success { data } => Ok(data),
        KiteResponse::Error {
            message,
            error_type,
            ..
        } => Err(KiteHttpError::Api {
            status: response.status.as_u16(),
            error_type,
            message,
        }),
    }
}

pub(crate) fn ensure_success(response: HttpResponse) -> Result<HttpResponse, KiteHttpError> {
    if response.status.is_success() {
        return Ok(response);
    }

    let status = response.status.as_u16();
    if let Ok(KiteResponse::Error {
        message,
        error_type,
        ..
    }) = serde_json::from_slice::<KiteResponse<serde_json::Value>>(&response.body)
    {
        return Err(KiteHttpError::Api {
            status,
            error_type,
            message,
        });
    }

    Err(KiteHttpError::HttpStatus(status))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use nautilus_network::http::{HttpResponse, HttpStatus};

    use crate::http::error::{KiteException, KiteHttpError};
    use crate::http::response::ensure_success;

    #[test]
    fn converts_unsuccessful_json_response_to_api_error() {
        let response = response(
            500,
            r#"{"status":"error","message":"Error message","error_type":"GeneralException"}"#,
        );

        let error = ensure_success(response).unwrap_err();

        assert!(matches!(
            error,
            KiteHttpError::Api {
                status: 500,
                error_type: KiteException::General,
                message,
            } if message == "Error message"
        ));
    }

    #[test]
    fn converts_unsuccessful_non_json_response_to_status_error() {
        let response = response(502, "Bad gateway");

        let error = ensure_success(response).unwrap_err();

        assert!(matches!(error, KiteHttpError::HttpStatus(502)));
    }

    fn response(status: u16, body: &'static str) -> HttpResponse {
        HttpResponse {
            status: HttpStatus::try_from(status).unwrap(),
            headers: HashMap::new(),
            body: body.into(),
        }
    }
}
