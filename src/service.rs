//! ユースケース層。
//!
//! 1 つの公開メソッドが 1 つのユースケース(API 1 本)に対応する。
//! どのメソッドも次の流れで書いている。
//!
//! 1. ロックを取る(= DB ならトランザクション開始)
//! 2. Repository から読み込む(手元にはコピーが来る)
//! 3. コピーに対してドメインのロジックで変更する
//! 4. 成功したときだけ Repository に保存する(= コミット)
//!
//! 3 で失敗すると `?` で 4 を通らずに return するので、保存済みのデータは一切変わらない。
//! これが「全部成功するか、何も起きないか」の原子性を担保している。

use std::fmt;
use std::sync::{Mutex, MutexGuard};

use crate::domain::{self, DomainError, Order, OrderId, OrderLine, Quantity, Sku, Stock};
use crate::repository::{OrderRepository, RepositoryError, StockRepository};

/// サービス層のエラー。ドメインのエラーに、「見つからない」と永続化の失敗を足したもの。
///
/// 「見つからない」をドメインではなくここに置いたのは、
/// 「ID で探して無かった」というのは保存先の事情であって業務ルールではないため。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    Domain(DomainError),
    StockNotFound(Sku),
    OrderNotFound(OrderId),
    /// 注文に、在庫レコードの無い SKU が含まれていた。まとめて返す。
    UnknownSkus(Vec<Sku>),
    Repository(RepositoryError),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceError::Domain(e) => write!(f, "{e}"),
            ServiceError::StockNotFound(sku) => write!(f, "stock {sku} not found"),
            ServiceError::OrderNotFound(id) => write!(f, "order {id} not found"),
            ServiceError::UnknownSkus(skus) => {
                let names: Vec<&str> = skus.iter().map(Sku::as_str).collect();
                write!(f, "unknown SKU: {}", names.join(", "))
            }
            ServiceError::Repository(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ServiceError {}

// `impl From<A> for B` を書くと、`?` 演算子が A のエラーを自動で B に変換してくれる。
// これにより、戻り値が `Result<_, ServiceError>` の関数の中で、
// `Result<_, DomainError>` を返す関数に `?` をそのまま付けられる。
impl From<DomainError> for ServiceError {
    fn from(e: DomainError) -> Self {
        ServiceError::Domain(e)
    }
}

impl From<RepositoryError> for ServiceError {
    fn from(e: RepositoryError) -> Self {
        ServiceError::Repository(e)
    }
}

/// Mutex で守る中身。在庫と受注の Repository を 1 つにまとめている。
///
/// 2 つを別々の Mutex にしないのは、引当のように在庫と受注を同時に更新する処理で
/// 「在庫は更新されたが受注はまだ」という途中状態を他のリクエストに見せないため。
struct Repositories<S, O> {
    stocks: S,
    orders: O,
}

/// ユースケース層の本体。
///
/// `<S: StockRepository, O: OrderRepository>` はジェネリクス。
/// 「S は StockRepository を実装した何らかの型」という意味で、具体的な型は使う側が決める。
/// Swift の `struct Service<S: StockRepository, O: OrderRepository>` とほぼ同じ書き方。
/// コンパイル時に具体型ごとのコードが生成されるので、実行時のコストは無い。
pub struct Service<S: StockRepository, O: OrderRepository> {
    // DB ならトランザクション + 行ロックにあたる部分。
    // ここでは全データを 1 つの Mutex で守っているので、更新系の処理は完全に直列になる。
    // 単純で確実だが、同時実行性は低い(README のトレードオフ参照)。
    //
    // `tokio::sync::Mutex` ではなく `std::sync::Mutex` を使っているのは、
    // ロック中に `.await` しない(すべてメモリ上の同期処理)ため。その場合は std の方が軽い。
    inner: Mutex<Repositories<S, O>>,
}

impl<S: StockRepository, O: OrderRepository> Service<S, O> {
    pub fn new(stocks: S, orders: O) -> Self {
        Service {
            inner: Mutex::new(Repositories { stocks, orders }),
        }
    }

    /// ロックを取る。DB なら `BEGIN TRANSACTION` にあたる。
    ///
    /// 戻り値の `MutexGuard` はスコープを抜けると自動でロックを解放する(RAII)。
    /// Swift の `defer { unlock() }` を書き忘れる心配が無い。
    fn lock(&self) -> MutexGuard<'_, Repositories<S, O>> {
        // ロック中に別スレッドが panic すると Mutex は「poisoned(汚染)」状態になる。
        // このサービスは保存を最後にまとめて行うので、panic しても保存済みデータは
        // 途中状態にならない。そのため汚染は無視して中身を使い続ける。
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ---------------- 在庫 ----------------

    /// 入荷。在庫レコードが無い SKU なら新規作成する。
    pub fn receive(&self, sku: Sku, quantity: Quantity) -> Result<Stock, ServiceError> {
        let mut repos = self.lock();

        // `unwrap_or_else` は Option が None のときだけクロージャを実行して値を作る。
        // ここでは「見つからなければ在庫 0 で新規作成」を表している。
        // `sku.clone()` が必要なのは、Stock::new が SKU の所有権を取るため。
        let mut stock = repos
            .stocks
            .find(&sku)?
            .unwrap_or_else(|| Stock::new(sku.clone()));
        stock.receive(quantity)?;

        repos.stocks.save(stock.clone())?;
        Ok(stock)
    }

    pub fn list_stocks(&self) -> Result<Vec<Stock>, ServiceError> {
        // 読み取りでもロックを取るのは、更新の途中状態を読まないため。
        Ok(self.lock().stocks.find_all()?)
    }

    pub fn get_stock(&self, sku: &Sku) -> Result<Stock, ServiceError> {
        // `ok_or_else` で「None なら NotFound エラー」に変換し、`?` 無しでそのまま返している。
        self.lock()
            .stocks
            .find(sku)?
            .ok_or_else(|| ServiceError::StockNotFound(sku.clone()))
    }

    // ---------------- 受注 ----------------

    /// 受注の登録。この時点では在庫は確保しない(Pending)。
    pub fn create_order(&self, lines: Vec<OrderLine>) -> Result<Order, ServiceError> {
        let mut repos = self.lock();

        // ID を先に払い出してから検証する。検証に失敗するとこの ID は欠番になる。
        // DB の AUTOINCREMENT でも、ロールバックされた INSERT の番号は戻らないのと同じ。
        // 欠番を避けたいなら検証を先にすればよいが、ここでは DB と同じ振る舞いを優先した。
        let id = repos.orders.next_id()?;
        let order = Order::new(id, lines)?;

        // 未知の SKU を全部集めてから返す。在庫不足と同じく、一括で直せるようにするため。
        let mut unknown: Vec<Sku> = Vec::new();
        for line in order.lines() {
            if repos.stocks.find(&line.sku)?.is_none() {
                unknown.push(line.sku.clone());
            }
        }
        if !unknown.is_empty() {
            return Err(ServiceError::UnknownSkus(unknown));
        }

        repos.orders.save(order.clone())?;
        Ok(order)
    }

    pub fn list_orders(&self) -> Result<Vec<Order>, ServiceError> {
        Ok(self.lock().orders.find_all()?)
    }

    pub fn get_order(&self, id: OrderId) -> Result<Order, ServiceError> {
        self.lock()
            .orders
            .find(id)?
            .ok_or(ServiceError::OrderNotFound(id))
    }

    /// 引当。全明細まとめて確保するか、何もしないか。
    pub fn allocate(&self, id: OrderId) -> Result<Order, ServiceError> {
        // 3 つのユースケースは「どのドメイン関数を呼ぶか」だけが違うので、
        // 関数そのものを引数として渡して共通化している。
        self.update_order_and_stocks(id, domain::allocate)
    }

    /// 出荷。
    pub fn ship(&self, id: OrderId) -> Result<Order, ServiceError> {
        self.update_order_and_stocks(id, domain::ship)
    }

    /// キャンセル。引当済みなら引当を解放する。
    pub fn cancel(&self, id: OrderId) -> Result<Order, ServiceError> {
        self.update_order_and_stocks(id, domain::cancel)
    }

    /// 受注と、その明細の在庫をまとめて更新する共通処理。
    ///
    /// `operation: fn(...)` は「この形の関数を受け取る」という引数。
    /// Swift の `(inout Order, inout [Stock]) throws -> Void` を受け取るのに近い。
    fn update_order_and_stocks(
        &self,
        id: OrderId,
        operation: fn(&mut Order, &mut [Stock]) -> Result<(), DomainError>,
    ) -> Result<Order, ServiceError> {
        // --- BEGIN TRANSACTION ---
        let mut repos = self.lock();

        // 読み込み。DB なら `SELECT ... FOR UPDATE` で行ロックを取る箇所。
        let mut order = repos
            .orders
            .find(id)?
            .ok_or(ServiceError::OrderNotFound(id))?;
        let mut stocks: Vec<Stock> = Vec::new();
        for line in order.lines() {
            let stock = repos
                .stocks
                .find(&line.sku)?
                .ok_or_else(|| ServiceError::StockNotFound(line.sku.clone()))?;
            stocks.push(stock);
        }

        // コピーに対してドメインで変更。失敗したら `?` でここから return し、何も保存しない。
        operation(&mut order, &mut stocks)?;

        // 成功時のみ保存。
        // インメモリ実装の save は失敗しないが、DB なら途中で失敗しうる。
        // その場合はトランザクションのロールバックで、ここまでの save を取り消すことになる。
        for stock in stocks {
            repos.stocks.save(stock)?;
        }
        repos.orders.save(order.clone())?;
        Ok(order)
        // --- COMMIT(ここで repos がスコープを抜け、ロックが解放される)---
    }
}

#[cfg(test)]
mod tests;
