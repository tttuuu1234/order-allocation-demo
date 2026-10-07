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

use std::sync::{Mutex, MutexGuard};

use crate::domain::{
    self, AdjustmentReason, AllocationOutcome, AllocationStrategy, DomainError, LedgerEntry, Order,
    OrderId, OrderLine, OrderStatus, Quantity, ShipmentNo, Sku, Stock, StockMovement,
    TrackingNumber, WarehouseId,
};
use crate::repository::{InventoryRepository, OrderRepository};

mod error;

pub use error::ServiceError;

/// 入荷・棚卸調整の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryChange {
    /// 変更後の在庫
    pub stock: Stock,
    /// 台帳に記録した行
    pub entry: LedgerEntry,
    /// この変更で引当できた入荷待ちの注文
    pub reallocated: Vec<OrderId>,
}

/// 引当の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocationResult {
    pub order: Order,
    pub outcome: AllocationOutcome,
}

/// キャンセルの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelResult {
    pub order: Order,
    /// 解放された在庫で引当できた入荷待ちの注文
    pub reallocated: Vec<OrderId>,
}

/// Mutex で守る中身。在庫と受注の Repository を 1 つにまとめている。
///
/// 2 つを別々の Mutex にしないのは、引当のように在庫と受注を同時に更新する処理で
/// 「在庫は更新されたが受注はまだ」という途中状態を他のリクエストに見せないため。
struct Repositories<I, O> {
    inventory: I,
    orders: O,
}

/// ユースケース層の本体。
///
/// `<I: InventoryRepository, O: OrderRepository>` はジェネリクス。
/// 「I は InventoryRepository を実装した何らかの型」という意味で、具体的な型は使う側が決める。
/// コンパイル時に具体型ごとのコードが生成されるので、実行時のコストは無い。
pub struct Service<I: InventoryRepository, O: OrderRepository> {
    // DB ならトランザクション + 行ロックにあたる部分。
    // ここでは全データを 1 つの Mutex で守っているので、更新系の処理は完全に直列になる。
    // 単純で確実だが、同時実行性は低い(README のトレードオフ参照)。
    //
    // `tokio::sync::Mutex` ではなく `std::sync::Mutex` を使っているのは、
    // ロック中に `.await` しない(すべてメモリ上の同期処理)ため。その場合は std の方が軽い。
    inner: Mutex<Repositories<I, O>>,

    /// 引当戦略。
    ///
    /// Repository はジェネリクスにしたが、戦略は `Box<dyn ...>`(トレイトオブジェクト)にした。
    /// ジェネリクスにすると型引数が 3 つに増えて読みにくくなるのと、
    /// 戦略は 1 リクエストに数回呼ぶだけなので、動的ディスパッチの小さなコストが問題にならないため。
    /// `Box` はヒープに置いた値を所有するポインタで、サイズが実行時に決まる値を持つのに使う。
    strategy: Box<dyn AllocationStrategy>,
}

impl<I: InventoryRepository, O: OrderRepository> Service<I, O> {
    /// `impl AllocationStrategy + 'static` は「AllocationStrategy を実装した何らかの型の値」。
    /// `'static` は「一時的な参照を含まない(ずっと持っていても安全な)値」という制約で、
    /// Box に入れてサービスが持ち続けるために必要。
    pub fn new(inventory: I, orders: O, strategy: impl AllocationStrategy + 'static) -> Self {
        Service {
            inner: Mutex::new(Repositories { inventory, orders }),
            strategy: Box::new(strategy),
        }
    }

    /// ロックを取る。DB なら `BEGIN TRANSACTION` にあたる。
    ///
    /// 戻り値の `MutexGuard` はスコープを抜けると自動でロックを解放する(RAII)。
    /// Swift の `defer { unlock() }` を書き忘れる心配が無い。
    fn lock(&self) -> MutexGuard<'_, Repositories<I, O>> {
        // ロック中に別スレッドが panic すると Mutex は「poisoned(汚染)」状態になる。
        // このサービスは保存を最後にまとめて行うので、panic しても保存済みデータは
        // 途中状態にならない。そのため汚染は無視して中身を使い続ける。
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ---------------- 在庫 ----------------

    /// 入荷。その倉庫に在庫レコードが無い SKU なら新規作成する。
    /// 入荷待ちの注文があれば、続けて再引当する。
    pub fn receive(
        &self,
        warehouse: WarehouseId,
        sku: Sku,
        quantity: Quantity,
    ) -> Result<InventoryChange, ServiceError> {
        // `|stock| stock.receive(quantity)` はクロージャ(無名関数)。
        // Swift の `{ stock in stock.receive(quantity) }` にあたる。
        self.change_inventory(warehouse, sku, true, |stock| stock.receive(quantity))
    }

    /// 棚卸調整。在庫レコードが無ければエラー(数え直す対象が無いので)。
    pub fn adjust(
        &self,
        warehouse: WarehouseId,
        sku: Sku,
        delta: i64,
        reason: AdjustmentReason,
    ) -> Result<InventoryChange, ServiceError> {
        self.change_inventory(warehouse, sku, false, |stock| stock.adjust(delta, reason))
    }

    pub fn list_stocks(&self) -> Result<Vec<Stock>, ServiceError> {
        // 読み取りでもロックを取るのは、更新の途中状態を読まないため。
        Ok(self.lock().inventory.find_all()?)
    }

    /// その SKU の、全倉庫の在庫。
    pub fn stocks_of(&self, sku: &Sku) -> Result<Vec<Stock>, ServiceError> {
        let stocks = self.lock().inventory.find_by_sku(sku)?;
        if stocks.is_empty() {
            return Err(ServiceError::StockNotFound {
                sku: sku.clone(),
                warehouse: None,
            });
        }
        Ok(stocks)
    }

    /// その SKU の受払台帳。
    pub fn movements_of(&self, sku: &Sku) -> Result<Vec<LedgerEntry>, ServiceError> {
        let repos = self.lock();
        if repos.inventory.find_by_sku(sku)?.is_empty() {
            return Err(ServiceError::StockNotFound {
                sku: sku.clone(),
                warehouse: None,
            });
        }
        Ok(repos.inventory.movements_for(sku)?)
    }

    // ---------------- 受注 ----------------

    /// 受注の登録。この時点では在庫は確保しない(Pending)。
    pub fn create_order(&self, lines: Vec<OrderLine>) -> Result<Order, ServiceError> {
        let mut repos = self.lock();

        // ID を先に払い出してから検証する。検証に失敗するとこの ID は欠番になる。
        // DB の AUTOINCREMENT でも、ロールバックされた INSERT の番号は戻らないのと同じ。
        let id = repos.orders.next_id()?;
        let order = Order::new(id, lines)?;

        // どの倉庫にも在庫レコードが無い SKU を全部集めてから返す。
        // 在庫不足と同じく、一括で直せるようにするため。
        let mut unknown: Vec<Sku> = Vec::new();
        for line in order.lines() {
            if repos.inventory.find_by_sku(&line.sku)?.is_empty() {
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
        // `ok_or` で「None なら NotFound エラー」に変換し、`?` 無しでそのまま返している。
        self.lock()
            .orders
            .find(id)?
            .ok_or(ServiceError::OrderNotFound(id))
    }

    /// 引当。足りなければ入荷待ちにする(在庫には触れない)。
    pub fn allocate(&self, id: OrderId) -> Result<AllocationResult, ServiceError> {
        // --- BEGIN TRANSACTION ---
        let mut repos = self.lock();

        // 読み込み。DB なら `SELECT ... FOR UPDATE` で行ロックを取る箇所。
        let mut order = repos.find_order(id)?;
        // `std::slice::from_ref(&order)` は、1 つの値を「要素 1 個のスライス」として借りる。
        // 複製せずに `&[Order]` を受け取る関数へ渡せる。
        let mut stocks = repos.load_stocks(&skus_of(std::slice::from_ref(&order)))?;

        // コピーに対してドメインで変更。失敗したら `?` でここから return し、何も保存しない。
        let outcome = domain::allocate(&mut order, &mut stocks, self.strategy.as_ref())?;

        // 成功時のみ保存。入荷待ちになった場合も、注文の状態が変わるので保存する。
        // インメモリ実装の save は失敗しないが、DB なら途中で失敗しうる。
        // その場合はトランザクションのロールバックで、ここまでの save を取り消すことになる。
        repos.save_stocks(stocks)?;
        repos.orders.save(order.clone())?;
        Ok(AllocationResult { order, outcome })
        // --- COMMIT(ここで repos がスコープを抜け、ロックが解放される)---
    }

    /// 出荷を 1 件出す。
    pub fn ship(
        &self,
        id: OrderId,
        shipment_no: ShipmentNo,
        tracking_number: TrackingNumber,
    ) -> Result<Order, ServiceError> {
        let mut repos = self.lock();

        let mut order = repos.find_order(id)?;
        let mut stocks = repos.load_stocks(&skus_of(std::slice::from_ref(&order)))?;

        let movements = domain::ship(&mut order, shipment_no, tracking_number, &mut stocks)?;

        repos.save_stocks(stocks)?;
        repos.record_movements(movements)?;
        repos.orders.save(order.clone())?;
        Ok(order)
    }

    /// キャンセル。引当済みなら引当を解放し、その在庫で入荷待ちの注文を再引当する。
    pub fn cancel(&self, id: OrderId) -> Result<CancelResult, ServiceError> {
        let mut repos = self.lock();

        let mut order = repos.find_order(id)?;
        let order_skus = skus_of(std::slice::from_ref(&order));
        // キャンセルする注文自身が入荷待ちの場合もあるので、再引当の対象からは外す。
        let mut backorders: Vec<Order> = repos
            .backorders_containing(&order_skus)?
            .into_iter()
            .filter(|o| o.id() != id)
            .collect();
        let mut skus = order_skus;
        skus.extend(skus_of(&backorders));
        let mut stocks = repos.load_stocks(&skus)?;

        domain::cancel(&mut order, &mut stocks)?;
        let reallocated =
            domain::reallocate_backorders(&mut backorders, &mut stocks, self.strategy.as_ref())?;

        repos.save_stocks(stocks)?;
        repos.orders.save(order.clone())?;
        repos.save_reallocated(backorders, &reallocated)?;
        Ok(CancelResult { order, reallocated })
    }

    // ---------------- 内部 ----------------

    /// 入荷と棚卸調整の共通処理。
    ///
    /// `change: impl FnOnce(&mut Stock) -> ...` は「在庫を受け取って変更するクロージャ」を
    /// 引数に取る書き方。`FnOnce` は「1 回だけ呼ばれる」関数を表す。
    /// 入荷と棚卸で違うのは「在庫に何をするか」だけなので、そこを外から渡している。
    fn change_inventory(
        &self,
        warehouse: WarehouseId,
        sku: Sku,
        create_if_missing: bool,
        change: impl FnOnce(&mut Stock) -> Result<StockMovement, DomainError>,
    ) -> Result<InventoryChange, ServiceError> {
        let mut repos = self.lock();

        // 再引当の候補と、その注文たちが必要とする全 SKU の在庫を読み込んでおく。
        // 在庫を変えた後の再引当も、同じトランザクションの中で済ませるため。
        let mut backorders = repos.backorders_containing(std::slice::from_ref(&sku))?;
        let mut skus = skus_of(&backorders);
        skus.push(sku.clone());
        let mut stocks = repos.load_stocks(&skus)?;

        // 対象の在庫が何番目にあるか。無ければ新規作成するか、エラーにする。
        let index = match stocks.iter().position(|s| s.is(&warehouse, &sku)) {
            Some(index) => index,
            None if create_if_missing => {
                stocks.push(Stock::new(warehouse, sku));
                stocks.len() - 1
            }
            None => {
                return Err(ServiceError::StockNotFound {
                    sku,
                    warehouse: Some(warehouse),
                });
            }
        };

        let movement = change(&mut stocks[index])?;
        let reallocated =
            domain::reallocate_backorders(&mut backorders, &mut stocks, self.strategy.as_ref())?;

        let stock = stocks[index].clone();
        repos.save_stocks(stocks)?;
        let entry = repos.inventory.append_movement(movement)?;
        repos.save_reallocated(backorders, &reallocated)?;
        Ok(InventoryChange {
            stock,
            entry,
            reallocated,
        })
    }
}

/// 読み込み・保存の小さな部品。ユースケースの本筋を読みやすくするために切り出している。
impl<I: InventoryRepository, O: OrderRepository> Repositories<I, O> {
    fn find_order(&self, id: OrderId) -> Result<Order, ServiceError> {
        self.orders.find(id)?.ok_or(ServiceError::OrderNotFound(id))
    }

    /// 指定した SKU すべての、全倉庫の在庫を読み込む。
    ///
    /// SKU が重複していても 1 回しか読まない。同じ在庫のコピーが 2 つあると、
    /// 片方だけ変更して保存したあと、もう片方(古いまま)で上書きしてしまうため。
    fn load_stocks(&self, skus: &[Sku]) -> Result<Vec<Stock>, ServiceError> {
        let mut loaded: Vec<&Sku> = Vec::new();
        let mut stocks = Vec::new();
        for sku in skus {
            if loaded.contains(&sku) {
                continue;
            }
            loaded.push(sku);
            stocks.extend(self.inventory.find_by_sku(sku)?);
        }
        Ok(stocks)
    }

    /// 指定した SKU のどれかを含む、入荷待ちの注文(先着順)。
    fn backorders_containing(&self, skus: &[Sku]) -> Result<Vec<Order>, ServiceError> {
        Ok(self
            .orders
            .find_by_status(OrderStatus::Backordered)?
            .into_iter()
            .filter(|order| skus.iter().any(|sku| order.contains_sku(sku)))
            .collect())
    }

    fn save_stocks(&mut self, stocks: Vec<Stock>) -> Result<(), ServiceError> {
        for stock in stocks {
            self.inventory.save(stock)?;
        }
        Ok(())
    }

    fn record_movements(&mut self, movements: Vec<StockMovement>) -> Result<(), ServiceError> {
        for movement in movements {
            self.inventory.append_movement(movement)?;
        }
        Ok(())
    }

    /// 再引当の候補のうち、実際に引当できた注文だけを保存する。
    /// 待ちのままの注文は何も変わっていないので書かない。
    fn save_reallocated(
        &mut self,
        candidates: Vec<Order>,
        reallocated: &[OrderId],
    ) -> Result<(), ServiceError> {
        for order in candidates {
            if reallocated.contains(&order.id()) {
                self.orders.save(order)?;
            }
        }
        Ok(())
    }
}

/// 注文たちに含まれる SKU を、重複なく集める。
fn skus_of(orders: &[Order]) -> Vec<Sku> {
    let mut skus: Vec<Sku> = Vec::new();
    for order in orders {
        for line in order.lines() {
            if !skus.contains(&line.sku) {
                skus.push(line.sku.clone());
            }
        }
    }
    skus
}

#[cfg(test)]
mod tests;
