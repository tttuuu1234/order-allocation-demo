//! 在庫。1 件 = 「ある倉庫にある、ある SKU」。
//!
//! 不変条件: `reserved <= on_hand`。
//! つまり `available = on_hand - reserved` は決してマイナスにならない。
//! この条件を守るため、フィールドは非公開にしてメソッド経由でしか変更できないようにする。
//!
//! on_hand を変えるメソッド(入荷・出荷・棚卸調整)は、受払台帳に載せる
//! `StockMovement` を戻り値で返す。台帳への記録漏れを、呼び出し側の注意力ではなく
//! 「戻り値を使わないと警告が出る」という型の仕組みで防ぐため。

use super::{
    AdjustmentReason, DomainError, MovementReason, Quantity, Sku, StockMovement, WarehouseId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stock {
    warehouse: WarehouseId,
    sku: Sku,
    /// 実在庫(倉庫に物理的にある数)
    on_hand: Quantity,
    /// 引当済み(注文に確保されているが、まだ出荷していない数)
    reserved: Quantity,
}

impl Stock {
    /// 在庫 0 の新しい在庫レコードを作る。入荷時に未登録なら使う。
    pub fn new(warehouse: WarehouseId, sku: Sku) -> Self {
        Stock {
            warehouse,
            sku,
            on_hand: Quantity::ZERO,
            reserved: Quantity::ZERO,
        }
    }

    // ---- 読み取り ----
    // `&self` は「自分を読み取り専用で借りる」という意味。
    // 呼び出し後も呼び出し元が Stock を使い続けられる。

    pub fn warehouse(&self) -> &WarehouseId {
        &self.warehouse
    }

    pub fn sku(&self) -> &Sku {
        &self.sku
    }

    pub fn on_hand(&self) -> Quantity {
        self.on_hand
    }

    pub fn reserved(&self) -> Quantity {
        self.reserved
    }

    /// 引当可能数。保存せず毎回計算するのは、on_hand / reserved と食い違う余地をなくすため。
    pub fn available(&self) -> Quantity {
        // 不変条件 reserved <= on_hand があるので負にならないが、
        // 万一に備えて saturating_sub(0 で止まる減算)にしておく。
        self.on_hand.saturating_sub(self.reserved)
    }

    /// 倉庫と SKU が一致するか。在庫の一覧から目的の 1 件を探すときに使う。
    pub fn is(&self, warehouse: &WarehouseId, sku: &Sku) -> bool {
        &self.warehouse == warehouse && &self.sku == sku
    }

    // ---- 変更 ----
    // `&mut self` は「自分を書き換え可能として借りる」。
    // Rust では同時に 1 か所しか `&mut` で借りられないので、データ競合がコンパイル時に防がれる。

    /// 入荷。実在庫を増やす。
    ///
    /// `#[must_use]` を付けると、戻り値を捨てたときにコンパイラが警告する。
    /// 台帳への記録漏れを防ぐため。
    #[must_use = "受払台帳に記録すること"]
    pub fn receive(&mut self, quantity: Quantity) -> Result<StockMovement, DomainError> {
        if quantity.is_zero() {
            return Err(DomainError::ZeroQuantity {
                sku: self.sku.clone(),
            });
        }
        // `ok_or_else` は Option を Result に変換する。None ならクロージャでエラーを作る。
        // 末尾の `?` は「Err ならその場で return し、Ok なら中身を取り出す」演算子。
        // Swift の `try` に近いが、関数の戻り値も Result である必要がある。
        self.on_hand =
            self.on_hand
                .checked_add(quantity)
                .ok_or_else(|| DomainError::QuantityOverflow {
                    sku: self.sku.clone(),
                })?;
        Ok(self.movement(i64::from(quantity.value()), MovementReason::Receipt))
    }

    /// 棚卸調整。帳簿の数を実際の数に合わせる。
    ///
    /// 引当済みの数より少なくはできない。
    /// 引き当てた注文が出荷できなくなるので、先にその注文をどうするか(キャンセルなど)を
    /// 人が判断すべきだから。自動で引当を外すと、誰の注文が欠けるかが勝手に決まってしまう。
    #[must_use = "受払台帳に記録すること"]
    pub fn adjust(
        &mut self,
        delta: i64,
        reason: AdjustmentReason,
    ) -> Result<StockMovement, DomainError> {
        if delta == 0 {
            return Err(DomainError::ZeroQuantity {
                sku: self.sku.clone(),
            });
        }
        // i64 で計算してから u32 に戻す。`u32::try_from` は範囲外なら Err を返す変換。
        let new_on_hand = i64::from(self.on_hand.value()) + delta;
        let new_on_hand = u32::try_from(new_on_hand)
            .map(Quantity::new)
            .map_err(|_| self.adjustment_error())?;
        if new_on_hand < self.reserved {
            return Err(self.adjustment_error());
        }
        self.on_hand = new_on_hand;
        Ok(self.movement(delta, MovementReason::Adjustment(reason)))
    }

    /// 引当。引当可能数が足りなければ何も変えずにエラー。
    pub fn reserve(&mut self, quantity: Quantity) -> Result<(), DomainError> {
        if self.available() < quantity {
            return Err(DomainError::ReservedOverflow {
                warehouse: self.warehouse.clone(),
                sku: self.sku.clone(),
            });
        }
        // 上で available >= quantity を確認済みなので、
        // reserved + quantity <= on_hand <= u32::MAX となり、あふれない。
        self.reserved =
            self.reserved
                .checked_add(quantity)
                .ok_or_else(|| DomainError::QuantityOverflow {
                    sku: self.sku.clone(),
                })?;
        Ok(())
    }

    /// 引当の解放(キャンセル時)。
    pub fn release(&mut self, quantity: Quantity) -> Result<(), DomainError> {
        self.reserved = self.subtract_reserved(quantity)?;
        Ok(())
    }

    /// 出荷。倉庫から物が出ていくので、実在庫と引当済みの両方を減らす。
    /// 両方減らすので available は変わらない。
    #[must_use = "受払台帳に記録すること"]
    pub fn ship(
        &mut self,
        quantity: Quantity,
        reason: MovementReason,
    ) -> Result<StockMovement, DomainError> {
        // 先に両方の計算を済ませてから代入する。
        // 片方だけ代入した後にもう片方で失敗すると、中途半端な状態が残るため。
        let new_reserved = self.subtract_reserved(quantity)?;
        let new_on_hand = self
            .on_hand
            .checked_sub(quantity)
            .ok_or_else(|| self.underflow_error())?;
        self.reserved = new_reserved;
        self.on_hand = new_on_hand;
        Ok(self.movement(-i64::from(quantity.value()), reason))
    }

    fn subtract_reserved(&self, quantity: Quantity) -> Result<Quantity, DomainError> {
        self.reserved
            .checked_sub(quantity)
            .ok_or_else(|| self.underflow_error())
    }

    /// 変更後の状態から台帳の 1 行を作る。必ず on_hand を更新した「後」に呼ぶ。
    fn movement(&self, delta: i64, reason: MovementReason) -> StockMovement {
        StockMovement {
            warehouse: self.warehouse.clone(),
            sku: self.sku.clone(),
            delta,
            balance_after: self.on_hand,
            reason,
        }
    }

    fn underflow_error(&self) -> DomainError {
        DomainError::ReservedUnderflow {
            warehouse: self.warehouse.clone(),
            sku: self.sku.clone(),
        }
    }

    fn adjustment_error(&self) -> DomainError {
        DomainError::AdjustmentBelowReserved {
            warehouse: self.warehouse.clone(),
            sku: self.sku.clone(),
            on_hand: self.on_hand,
            reserved: self.reserved,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stock(on_hand: u32, reserved: u32) -> Stock {
        let mut stock = Stock::new(
            WarehouseId::new("TOKYO").unwrap(),
            Sku::new("APPLE").unwrap(),
        );
        if on_hand > 0 {
            // テストでは台帳の行は使わないので `let _ =` で明示的に捨てる。
            let _ = stock.receive(Quantity::new(on_hand)).unwrap();
        }
        if reserved > 0 {
            stock.reserve(Quantity::new(reserved)).unwrap();
        }
        stock
    }

    #[test]
    fn available_は実在庫から引当済みを引いた数() {
        let stock = stock(10, 3);
        assert_eq!(stock.available(), Quantity::new(7));
    }

    #[test]
    fn 入荷で実在庫が増え_台帳の行が返る() {
        let mut stock = stock(10, 0);
        let movement = stock.receive(Quantity::new(5)).unwrap();
        assert_eq!(stock.on_hand(), Quantity::new(15));
        assert_eq!(movement.delta, 5);
        assert_eq!(movement.balance_after, Quantity::new(15));
        assert_eq!(movement.reason, MovementReason::Receipt);
    }

    #[test]
    fn 数量0の入荷はエラー() {
        let mut stock = stock(10, 0);
        let result = stock.receive(Quantity::ZERO);
        // `matches!` はパターンに一致するかを bool で返すマクロ。中身の値までは問わないときに便利。
        assert!(matches!(result, Err(DomainError::ZeroQuantity { .. })));
    }

    #[test]
    fn 引当可能数を超える引当は失敗し在庫は変わらない() {
        let mut stock = stock(5, 3);
        let before = stock.clone();
        assert!(stock.reserve(Quantity::new(3)).is_err());
        assert_eq!(stock, before);
    }

    #[test]
    fn 出荷で実在庫と引当済みが両方減り_マイナスの行が返る() {
        let mut stock = stock(10, 4);
        let movement = stock
            .ship(Quantity::new(4), MovementReason::Receipt)
            .unwrap();
        assert_eq!(stock.on_hand(), Quantity::new(6));
        assert_eq!(stock.reserved(), Quantity::ZERO);
        assert_eq!(movement.delta, -4);
        assert_eq!(movement.balance_after, Quantity::new(6));
    }

    #[test]
    fn 棚卸調整で増やすことも減らすこともできる() {
        let mut stock = stock(10, 0);
        let lost = stock.adjust(-3, AdjustmentReason::Lost).unwrap();
        assert_eq!(stock.on_hand(), Quantity::new(7));
        assert_eq!(lost.balance_after, Quantity::new(7));

        let _ = stock.adjust(2, AdjustmentReason::Found).unwrap();
        assert_eq!(stock.on_hand(), Quantity::new(9));
    }

    #[test]
    fn 引当済みを下回る棚卸調整はエラーで在庫は変わらない() {
        let mut stock = stock(10, 8);
        let before = stock.clone();
        let result = stock.adjust(-3, AdjustmentReason::Damaged);
        assert!(matches!(
            result,
            Err(DomainError::AdjustmentBelowReserved { .. })
        ));
        assert_eq!(stock, before);
    }

    #[test]
    fn 実在庫をマイナスにする棚卸調整はエラー() {
        let mut stock = stock(2, 0);
        assert!(stock.adjust(-3, AdjustmentReason::Lost).is_err());
    }

    #[test]
    fn 引当済みを超える解放は内部不整合エラー() {
        let mut stock = stock(10, 2);
        let result = stock.release(Quantity::new(3));
        assert!(matches!(result, Err(DomainError::ReservedUnderflow { .. })));
        assert_eq!(stock.reserved(), Quantity::new(2));
    }
}
