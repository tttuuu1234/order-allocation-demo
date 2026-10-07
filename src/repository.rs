//! 永続化の抽象(trait)と、そのインメモリ実装。
//!
//! サービス層はこの trait にだけ依存する。
//! SQLite に差し替えるときは、この trait を実装した型を新しく作り、
//! main.rs(と api の型エイリアス)で渡す型を変えるだけで済むようにしている。
//! Swift の protocol / Kotlin の interface を使った依存性逆転と同じ考え方。

use std::collections::BTreeMap;
use std::fmt;

use crate::domain::{
    LedgerEntry, Order, OrderId, OrderStatus, Sku, Stock, StockMovement, WarehouseId,
};

/// 永続化の失敗。
///
/// インメモリ実装では起きないが、DB なら接続断などで必ず起こりうる。
/// 今のうちに trait の戻り値を Result にしておくことで、
/// SQLite 実装に差し替えてもサービス層のシグネチャを変えずに済む。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryError(pub String);

impl fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "repository error: {}", self.0)
    }
}

impl std::error::Error for RepositoryError {}

/// 在庫(スナップショット)と受払台帳の保存先。
///
/// 在庫数と台帳を 1 つの trait にしているのは、両者を必ず同じトランザクションで
/// 書くべきものだから(DB なら同じデータベースの 2 つのテーブル)。
///
/// `Send` を要求しているのは、サービスを tokio の複数スレッドから共有するため。
/// (`Send` は「別スレッドへ渡しても安全」を表すマーカートレイト)
///
/// 読み取りが「参照(`&Stock`)」ではなく「値(`Stock`)」を返すのは、
/// 呼び出し側がコピーを自由に変更し、成功したときだけ `save` で書き戻すため。
/// DB から読んだ行がアプリ側のコピーであるのと同じ形になる。
pub trait InventoryRepository: Send {
    fn find(&self, warehouse: &WarehouseId, sku: &Sku) -> Result<Option<Stock>, RepositoryError>;
    /// その SKU の、全倉庫の在庫。
    fn find_by_sku(&self, sku: &Sku) -> Result<Vec<Stock>, RepositoryError>;
    fn find_all(&self) -> Result<Vec<Stock>, RepositoryError>;
    /// 新規なら追加、既存なら上書き(upsert)。
    fn save(&mut self, stock: Stock) -> Result<(), RepositoryError>;

    /// 台帳に 1 行追記する。連番は保存先が振る。台帳は追記のみで、更新・削除の口は用意しない。
    fn append_movement(&mut self, movement: StockMovement) -> Result<LedgerEntry, RepositoryError>;
    /// その SKU の台帳を、記録した順に返す。
    fn movements_for(&self, sku: &Sku) -> Result<Vec<LedgerEntry>, RepositoryError>;
}

/// 受注の保存先。出荷(Shipment)は Order の一部として一緒に保存される。
pub trait OrderRepository: Send {
    /// 次の注文 ID を払い出す。
    ///
    /// 払い出した ID は、その後の保存が行われなくても再利用しない。
    /// DB のシーケンス(AUTOINCREMENT)と同じ振る舞いにそろえておくため。
    fn next_id(&mut self) -> Result<OrderId, RepositoryError>;
    fn find(&self, id: OrderId) -> Result<Option<Order>, RepositoryError>;
    fn find_all(&self) -> Result<Vec<Order>, RepositoryError>;
    /// 指定した状態の注文を ID 順(= 先着順)で返す。入荷待ちの再引当で使う。
    fn find_by_status(&self, status: OrderStatus) -> Result<Vec<Order>, RepositoryError>;
    fn save(&mut self, order: Order) -> Result<(), RepositoryError>;
}

// ---- インメモリ実装 ----
//
// BTreeMap(キー順に並ぶ辞書)を使うのは、一覧 API の並び順を
// 倉庫・SKU 順や ID 順で安定させるため。HashMap だと実行ごとに順序が変わりうる。

/// 在庫と台帳のインメモリ実装。
///
/// `#[derive(Default)]` で、空の状態を `InMemoryInventoryRepository::default()` で作れる。
#[derive(Debug, Default)]
pub struct InMemoryInventoryRepository {
    /// キーは (倉庫, SKU) のタプル。DB なら複合主キーにあたる。
    stocks: BTreeMap<(WarehouseId, Sku), Stock>,
    ledger: Vec<LedgerEntry>,
}

// `impl トレイト for 型` で、その型にトレイトを実装する。
// Swift の `extension Type: Protocol` にあたる。
impl InventoryRepository for InMemoryInventoryRepository {
    fn find(&self, warehouse: &WarehouseId, sku: &Sku) -> Result<Option<Stock>, RepositoryError> {
        // `get` は参照(Option<&Stock>)を返すので、`.cloned()` で値のコピーにする。
        // 呼び出し側が変更しても、保存するまでここの中身には影響しない。
        let key = (warehouse.clone(), sku.clone());
        Ok(self.stocks.get(&key).cloned())
    }

    fn find_by_sku(&self, sku: &Sku) -> Result<Vec<Stock>, RepositoryError> {
        Ok(self
            .stocks
            .values()
            .filter(|stock| stock.sku() == sku)
            .cloned()
            .collect())
    }

    fn find_all(&self) -> Result<Vec<Stock>, RepositoryError> {
        Ok(self.stocks.values().cloned().collect())
    }

    fn save(&mut self, stock: Stock) -> Result<(), RepositoryError> {
        // `stock` の所有権はここで Map に移る(ムーブ)。キーとして別に持つ分だけ複製する。
        let key = (stock.warehouse().clone(), stock.sku().clone());
        self.stocks.insert(key, stock);
        Ok(())
    }

    fn append_movement(&mut self, movement: StockMovement) -> Result<LedgerEntry, RepositoryError> {
        let entry = LedgerEntry {
            seq: self.ledger.len() as u64 + 1,
            movement,
        };
        self.ledger.push(entry.clone());
        Ok(entry)
    }

    fn movements_for(&self, sku: &Sku) -> Result<Vec<LedgerEntry>, RepositoryError> {
        Ok(self
            .ledger
            .iter()
            .filter(|entry| &entry.movement.sku == sku)
            .cloned()
            .collect())
    }
}

/// 受注のインメモリ実装。
#[derive(Debug, Default)]
pub struct InMemoryOrderRepository {
    orders: BTreeMap<OrderId, Order>,
    /// 最後に払い出した ID。0 は「まだ 1 つも払い出していない」を表す。
    last_id: u64,
}

impl OrderRepository for InMemoryOrderRepository {
    fn next_id(&mut self) -> Result<OrderId, RepositoryError> {
        self.last_id += 1;
        Ok(OrderId::new(self.last_id))
    }

    fn find(&self, id: OrderId) -> Result<Option<Order>, RepositoryError> {
        Ok(self.orders.get(&id).cloned())
    }

    fn find_all(&self) -> Result<Vec<Order>, RepositoryError> {
        Ok(self.orders.values().cloned().collect())
    }

    fn find_by_status(&self, status: OrderStatus) -> Result<Vec<Order>, RepositoryError> {
        // BTreeMap はキー(ID)順に並んでいるので、そのまま先着順になる。
        Ok(self
            .orders
            .values()
            .filter(|order| order.status() == status)
            .cloned()
            .collect())
    }

    fn save(&mut self, order: Order) -> Result<(), RepositoryError> {
        self.orders.insert(order.id(), order);
        Ok(())
    }
}
