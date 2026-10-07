//! エラーを HTTP に変換する。どのエラーを何番にするかの対応表はここだけにある。

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::domain::DomainError;
use crate::service::ServiceError;

/// エラーレスポンスの本文。`{"code": "...", "message": "..."}` の形。
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
}

/// ハンドラが返すエラー。ステータスコードと本文の組。
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: ErrorBody,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            body: ErrorBody {
                code,
                message: message.into(),
            },
        }
    }
}

// `IntoResponse` を実装すると、ハンドラの戻り値としてそのまま返せるようになる。
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

impl From<ServiceError> for ApiError {
    fn from(error: ServiceError) -> Self {
        // メッセージは match で値を取り出す前に作っておく(match で中身をムーブするため)。
        let message = error.to_string();
        match error {
            ServiceError::Domain(domain_error) => from_domain_error(&domain_error, message),
            ServiceError::StockNotFound { .. } => {
                ApiError::new(StatusCode::NOT_FOUND, "stock_not_found", message)
            }
            ServiceError::OrderNotFound(_) => {
                ApiError::new(StatusCode::NOT_FOUND, "order_not_found", message)
            }
            // 注文の中の SKU が未知なのは、URL のリソースが無い(404)のではなく
            // 送られてきた内容が不正という扱いなので 422 にする。
            ServiceError::UnknownSkus(_) => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "unknown_sku", message)
            }
            ServiceError::Repository(_) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
            }
        }
    }
}

// ハンドラの中で `Sku::new(...)?` のようにドメインのエラーを直接 `?` できるようにする。
impl From<DomainError> for ApiError {
    fn from(error: DomainError) -> Self {
        ApiError::from(ServiceError::from(error))
    }
}

fn from_domain_error(error: &DomainError, message: String) -> ApiError {
    use DomainError::*;

    let (status, code) = match error {
        EmptySku
        | EmptyWarehouseId
        | EmptyTrackingNumber
        | ZeroQuantity { .. }
        | EmptyOrderLines
        | QuantityOverflow { .. } => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_input"),
        InvalidTransition { .. } | ShipmentAlreadyShipped { .. } => {
            (StatusCode::CONFLICT, "invalid_transition")
        }
        ShipmentNotFound { .. } => (StatusCode::NOT_FOUND, "shipment_not_found"),
        AdjustmentBelowReserved { .. } => (StatusCode::CONFLICT, "adjustment_below_reserved"),
        // 内部不整合は利用者のせいではないので 500。
        ReservedOverflow { .. }
        | ReservedUnderflow { .. }
        | StockMissing { .. }
        | PlanMismatch { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
    };
    ApiError::new(status, code, message)
}

/// JSON が壊れている・型が合わないなど、axum が本文を読めなかったとき。
///
/// そのままだと axum はプレーンテキストでエラーを返すので、
/// 他のエラーと同じ JSON 形式にそろえるために変換する。
/// ステータスは axum の判断(構文エラーは 400、型の不一致は 422 など)をそのまま使う。
impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        ApiError::new(rejection.status(), "invalid_json", rejection.body_text())
    }
}

/// パスの値が読めなかったとき(例: `/orders/abc`)。
impl From<PathRejection> for ApiError {
    fn from(rejection: PathRejection) -> Self {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_path",
            rejection.body_text(),
        )
    }
}
