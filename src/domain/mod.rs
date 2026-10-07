//! ドメイン層。業務ルールだけを表現する純粋なロジック。
//!
//! serde や axum には依存させない。HTTP や JSON はドメインの関心ではなく、
//! それらに引きずられると「業務ルールのテストに Web サーバが必要」になってしまうため。

mod allocation;
mod error;
mod order;
mod stock;
mod types;

// `pub use` で再公開しておくと、利用側は `domain::Order` のように
// 内部のファイル構成を知らずに書ける。ファイルを分割し直しても利用側が壊れない。
pub use allocation::{allocate, cancel, ship};
pub use error::{DomainError, Shortage};
pub use order::{Order, OrderAction, OrderLine, OrderStatus};
pub use stock::Stock;
pub use types::{OrderId, Quantity, Sku};
