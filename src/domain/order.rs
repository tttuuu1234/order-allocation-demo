//! 受注と、その状態遷移。
//!
//! 状態遷移のルールは `next_status` の 1 か所に集めている。
//! 遷移表がコードに 1 つだけあれば、ルールの確認も変更もそこを見るだけで済むため。
//!
//! ```text
//!   Pending ──allocate──▶ Allocated ──ship──▶ Shipped
//!      │                     │
//!      └──cancel──▶ Cancelled ◀──cancel──┘
//! ```

use std::fmt;

use super::{DomainError, OrderId, Quantity, Sku};

/// 受注の状態。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderStatus {
    /// 受け付けたが、まだ在庫を確保していない
    Pending,
    /// 在庫を確保済み(出荷待ち)
    Allocated,
    /// 出荷済み(終端)
    Shipped,
    /// キャンセル済み(終端)
    Cancelled,
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            OrderStatus::Pending => "Pending",
            OrderStatus::Allocated => "Allocated",
            OrderStatus::Shipped => "Shipped",
            OrderStatus::Cancelled => "Cancelled",
        };
        write!(f, "{name}")
    }
}

/// 受注に対する操作。エラーメッセージで「何をしようとして失敗したか」を伝えるのにも使う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderAction {
    Allocate,
    Ship,
    Cancel,
}

impl fmt::Display for OrderAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            OrderAction::Allocate => "allocate",
            OrderAction::Ship => "ship",
            OrderAction::Cancel => "cancel",
        };
        write!(f, "{name}")
    }
}

/// 受注明細。1 つの SKU とその数量。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderLine {
    pub sku: Sku,
    pub quantity: Quantity,
}

/// 受注。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    id: OrderId,
    lines: Vec<OrderLine>,
    status: OrderStatus,
}

impl Order {
    /// 新しい受注を `Pending` で作る。
    ///
    /// - 明細が空ならエラー
    /// - 数量 0 の明細があればエラー
    /// - 同じ SKU の明細は 1 行に合算する
    ///
    /// 合算するのは、同じ SKU が 2 行あると引当時に
    /// 「1 行目は確保できたが 2 行目で足りない」という判定が複雑になるため。
    /// 1 SKU = 1 行に正規化しておけば、在庫 1 件と明細 1 件を素直に突き合わせられる。
    ///
    /// 引数の `lines: Vec<OrderLine>` は参照(`&`)ではなく値で受け取っている。
    /// これは「所有権を受け取る」という意味で、呼び出し元はこの後 `lines` を使えなくなる(ムーブ)。
    /// 中身を作り直して Order に持たせるので、借りるより受け取る方が素直。
    pub fn new(id: OrderId, lines: Vec<OrderLine>) -> Result<Self, DomainError> {
        if lines.is_empty() {
            return Err(DomainError::EmptyOrderLines);
        }

        // HashMap ではなく Vec で合算しているのは、利用者が送った明細の順序を保つため。
        // 明細数は多くても数十程度の想定なので、線形探索でも問題にならない。
        let mut merged: Vec<OrderLine> = Vec::new();
        // `for line in lines` で Vec の所有権ごと 1 つずつ取り出す(`lines` はここで消費される)。
        for line in lines {
            if line.quantity.is_zero() {
                return Err(DomainError::ZeroQuantity { sku: line.sku });
            }
            // `iter_mut().find(...)` は条件に合う最初の要素への可変参照を Option で返す。
            match merged.iter_mut().find(|existing| existing.sku == line.sku) {
                Some(existing) => {
                    existing.quantity =
                        existing
                            .quantity
                            .checked_add(line.quantity)
                            .ok_or_else(|| DomainError::QuantityOverflow {
                                sku: line.sku.clone(),
                            })?;
                }
                None => merged.push(line),
            }
        }

        Ok(Order {
            id,
            lines: merged,
            status: OrderStatus::Pending,
        })
    }

    pub fn id(&self) -> OrderId {
        self.id
    }

    /// 明細をスライス(`&[T]`)で返す。
    /// `&Vec<T>` より `&[T]` の方が「読むだけ」の意図が明確で、内部表現にも縛られない。
    pub fn lines(&self) -> &[OrderLine] {
        &self.lines
    }

    pub fn status(&self) -> OrderStatus {
        self.status
    }

    /// 状態遷移表。現在の状態と操作から、遷移先を決める。
    ///
    /// 状態を変えずに「遷移できるか」だけを知りたい場面(引当の事前チェック)があるので、
    /// 判定と変更を分けている。
    pub fn next_status(&self, action: OrderAction) -> Result<OrderStatus, DomainError> {
        // タプル `(状態, 操作)` に対して match すると、遷移表をそのまま書き下せる。
        // `|` は「または」。`_` は「それ以外すべて」。
        match (self.status, action) {
            (OrderStatus::Pending, OrderAction::Allocate) => Ok(OrderStatus::Allocated),
            (OrderStatus::Allocated, OrderAction::Ship) => Ok(OrderStatus::Shipped),
            (OrderStatus::Pending | OrderStatus::Allocated, OrderAction::Cancel) => {
                Ok(OrderStatus::Cancelled)
            }
            (from, action) => Err(DomainError::InvalidTransition {
                order_id: self.id,
                from,
                action,
            }),
        }
    }

    /// 引当済みにする。在庫の確保は `domain::allocate` が担う。
    pub fn allocate(&mut self) -> Result<(), DomainError> {
        self.transition(OrderAction::Allocate)
    }

    /// 出荷済みにする。在庫の減算は `domain::ship` が担う。
    pub fn ship(&mut self) -> Result<(), DomainError> {
        self.transition(OrderAction::Ship)
    }

    /// キャンセル済みにする。引当の解放は `domain::cancel` が担う。
    pub fn cancel(&mut self) -> Result<(), DomainError> {
        self.transition(OrderAction::Cancel)
    }

    fn transition(&mut self, action: OrderAction) -> Result<(), DomainError> {
        self.status = self.next_status(action)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(sku: &str, quantity: u32) -> OrderLine {
        OrderLine {
            sku: Sku::new(sku).unwrap(),
            quantity: Quantity::new(quantity),
        }
    }

    fn pending_order() -> Order {
        Order::new(OrderId::new(1), vec![line("APPLE", 1)]).unwrap()
    }

    #[test]
    fn 同一_sku_の明細は合算され順序は保たれる() {
        let order = Order::new(
            OrderId::new(1),
            vec![line("APPLE", 2), line("BANANA", 1), line("APPLE", 3)],
        )
        .unwrap();

        assert_eq!(order.lines(), &[line("APPLE", 5), line("BANANA", 1)]);
    }

    #[test]
    fn 明細が空の注文はエラー() {
        let result = Order::new(OrderId::new(1), vec![]);
        assert_eq!(result, Err(DomainError::EmptyOrderLines));
    }

    #[test]
    fn 数量0の明細はエラー() {
        let result = Order::new(OrderId::new(1), vec![line("APPLE", 1), line("BANANA", 0)]);
        assert!(
            matches!(result, Err(DomainError::ZeroQuantity { sku }) if sku.as_str() == "BANANA")
        );
    }

    #[test]
    fn 新しい注文は_pending() {
        assert_eq!(pending_order().status(), OrderStatus::Pending);
    }

    #[test]
    fn pending_から引当_出荷と進める() {
        let mut order = pending_order();
        order.allocate().unwrap();
        assert_eq!(order.status(), OrderStatus::Allocated);
        order.ship().unwrap();
        assert_eq!(order.status(), OrderStatus::Shipped);
    }

    #[test]
    fn pending_も_allocated_もキャンセルできる() {
        let mut pending = pending_order();
        pending.cancel().unwrap();
        assert_eq!(pending.status(), OrderStatus::Cancelled);

        let mut allocated = pending_order();
        allocated.allocate().unwrap();
        allocated.cancel().unwrap();
        assert_eq!(allocated.status(), OrderStatus::Cancelled);
    }

    #[test]
    fn pending_のまま出荷はできない() {
        let mut order = pending_order();
        let result = order.ship();
        assert_eq!(
            result,
            Err(DomainError::InvalidTransition {
                order_id: OrderId::new(1),
                from: OrderStatus::Pending,
                action: OrderAction::Ship,
            })
        );
        // 失敗した遷移で状態が変わっていないこと
        assert_eq!(order.status(), OrderStatus::Pending);
    }

    #[test]
    fn shipped_はキャンセルできない() {
        let mut order = pending_order();
        order.allocate().unwrap();
        order.ship().unwrap();
        let result = order.cancel();
        assert!(matches!(
            result,
            Err(DomainError::InvalidTransition {
                from: OrderStatus::Shipped,
                action: OrderAction::Cancel,
                ..
            })
        ));
        assert_eq!(order.status(), OrderStatus::Shipped);
    }

    #[test]
    fn 二重引当はできない() {
        let mut order = pending_order();
        order.allocate().unwrap();
        assert!(order.allocate().is_err());
    }

    #[test]
    fn cancelled_からはどこにも遷移できない() {
        for action in [
            OrderAction::Allocate,
            OrderAction::Ship,
            OrderAction::Cancel,
        ] {
            let mut order = pending_order();
            order.cancel().unwrap();
            assert!(order.next_status(action).is_err(), "{action} should fail");
        }
    }
}
