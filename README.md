# order-allocation-demo

受発注・在庫引当サービスを Rust(axum)で実装したデモです。
「動けばよい」より **「読んで業務設計と Rust を学べる」** ことを優先して書いています。
コメントは日本語で、Rust 特有の書き方には初出の箇所に一言添えています。

- Web API のみ(画面は無し)。認証やマルチテナントは対象外
- 永続化はインメモリ。Repository を trait で抽象化しているので SQLite などに差し替えられます
- 依存クレートは axum / tokio / serde / serde_json だけ。エラー型は自前、ID は連番

実務の配送ドメインに近づけるため、次の 4 つを入れています。

1. **在庫の受払台帳**:在庫数を上書きするだけでなく、入荷・出荷・棚卸調整を 1 行ずつ記録する
2. **注文と出荷の分離**:1 つの注文が倉庫ごとの出荷に分かれ、出荷ごとに送り状番号を持って 1 件ずつ出荷できる
3. **複数倉庫と引当戦略**:在庫は「倉庫 × SKU」で持つ。どの倉庫から出すかは trait で差し替えられる
4. **バックオーダー**:在庫不足なら入荷待ちにし、入荷・棚卸での増加・キャンセルで在庫が空いたら先着順に自動で再引当する

---

## 1. 起動方法と curl シナリオ

```sh
cargo run            # http://127.0.0.1:3000 で起動
cargo test           # ユニットテスト
cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

起動時に次の在庫が入ります(`src/main.rs` の `seed`)。引当戦略は「東京優先、なるべく 1 倉庫から」です。
以下のシナリオは、この初期状態から **上から順に** 実行する前提です(サーバを再起動するとデータは初期状態に戻ります)。

| 倉庫  | APPLE | BANANA | ORANGE |
|-------|-------|--------|--------|
| TOKYO | 10    | 5      | -      |
| OSAKA | 5     | -      | 2      |

### API 一覧

| メソッド | パス | 内容 |
|---|---|---|
| GET  | `/stocks` | 全倉庫・全 SKU の在庫 |
| GET  | `/stocks/{sku}` | 1 SKU の全倉庫合計と倉庫別の内訳 |
| GET  | `/stocks/{sku}/movements` | 1 SKU の受払台帳 |
| POST | `/warehouses/{warehouse}/stocks/{sku}/receipts` | 入荷 `{"quantity": 5}` |
| POST | `/warehouses/{warehouse}/stocks/{sku}/adjustments` | 棚卸調整 `{"delta": -1, "reason": "Damaged"}` |
| POST | `/orders` | 受注 `{"lines": [{"sku": "APPLE", "quantity": 3}]}` |
| GET  | `/orders`、`/orders/{id}` | 受注の照会(出荷を含む) |
| POST | `/orders/{id}/allocate` | 引当(足りなければ入荷待ち) |
| POST | `/orders/{id}/shipments/{no}/ship` | 出荷を 1 件出す `{"tracking_number": "YMT-0001"}` |
| POST | `/orders/{id}/cancel` | キャンセル |

棚卸の理由(`reason`)は `Damaged`(破損)・`Lost`(紛失)・`Found`(発見)・`CountCorrection`(数え直し)のいずれかです。

### シナリオ A: 注文 → 引当(倉庫をまたいで分割)→ 出荷を 1 件ずつ

```sh
# BANANA は TOKYO にしか無く、ORANGE は OSAKA にしか無い。BANANA の 2 行は 1 行に合算される
curl -s -X POST localhost:3000/orders -H 'content-type: application/json' \
  -d '{"lines":[{"sku":"BANANA","quantity":2},{"sku":"ORANGE","quantity":1},{"sku":"BANANA","quantity":1}]}'
# => 201 {"id":1,"status":"Pending","lines":[{"sku":"BANANA","quantity":3},{"sku":"ORANGE","quantity":1}],"shipments":[]}

# 引当: 1 倉庫ではそろわないので、出荷が TOKYO と OSAKA の 2 つに分かれる
curl -s -X POST localhost:3000/orders/1/allocate
# => 200 {"order":{"id":1,"status":"Allocated",...,"shipments":[
#      {"no":1,"warehouse":"TOKYO","status":"AwaitingShipment","lines":[{"sku":"BANANA","quantity":3}]},
#      {"no":2,"warehouse":"OSAKA","status":"AwaitingShipment","lines":[{"sku":"ORANGE","quantity":1}]}]}}

# 1 件目を出荷すると「一部出荷済み」
curl -s -X POST localhost:3000/orders/1/shipments/1/ship \
  -H 'content-type: application/json' -d '{"tracking_number":"YMT-0001"}'
# => 200 {"id":1,"status":"PartiallyShipped",...
#         {"no":1,...,"status":"Shipped","tracking_number":"YMT-0001",...}

# 2 件目も出荷すると「出荷済み」
curl -s -X POST localhost:3000/orders/1/shipments/2/ship \
  -H 'content-type: application/json' -d '{"tracking_number":"SGW-0001"}'
# => 200 {"id":1,"status":"Shipped",...}

curl -s localhost:3000/stocks/BANANA
# => {"sku":"BANANA","on_hand":2,"reserved":0,"available":2,
#     "warehouses":[{"warehouse":"TOKYO","sku":"BANANA","on_hand":2,"reserved":0,"available":2}]}
```

### シナリオ B: 在庫不足 → 入荷待ち → 入荷で自動引当

```sh
# ORANGE は残り 1 個なので 3 個は足りない
curl -s -X POST localhost:3000/orders -H 'content-type: application/json' \
  -d '{"lines":[{"sku":"ORANGE","quantity":3}]}'
# => 201 {"id":2,"status":"Pending",...}

# 失敗ではなく「入荷待ち」になる(在庫は 1 個も押さえない)。不足の内訳が付く
curl -s -X POST localhost:3000/orders/2/allocate
# => 200 {"order":{"id":2,"status":"Backordered",...,"shipments":[]},
#         "shortages":[{"sku":"ORANGE","requested":3,"available":1,"missing":2}]}

# 2 個入荷すると、そのまま入荷待ちの注文 2 が引き当てられる
curl -s -X POST localhost:3000/warehouses/OSAKA/stocks/ORANGE/receipts \
  -H 'content-type: application/json' -d '{"quantity":2}'
# => 201 {"stock":{"warehouse":"OSAKA","sku":"ORANGE","on_hand":3,"reserved":3,"available":0},
#         "ledger_entry":{"seq":7,...,"delta":2,"balance_after":3,"reason":"Receipt"},
#         "reallocated_orders":[2]}

curl -s localhost:3000/orders/2
# => {"id":2,"status":"Allocated",...}
```

### シナリオ C: キャンセルで空いた在庫が入荷待ちに回る

```sh
# APPLE 12 個は 1 倉庫ではそろわないので TOKYO 10 + OSAKA 2 に分かれる
curl -s -X POST localhost:3000/orders -H 'content-type: application/json' \
  -d '{"lines":[{"sku":"APPLE","quantity":12}]}'                       # => id 3
curl -s -X POST localhost:3000/orders/3/allocate                      # => Allocated

# 残りは 3 個なので、5 個の注文は入荷待ち
curl -s -X POST localhost:3000/orders -H 'content-type: application/json' \
  -d '{"lines":[{"sku":"APPLE","quantity":5}]}'                        # => id 4
curl -s -X POST localhost:3000/orders/4/allocate
# => 200 {"order":{"id":4,"status":"Backordered",...},"shortages":[{"sku":"APPLE","requested":5,"available":3,"missing":2}]}

# 注文 3 をキャンセルすると引当が解放され、その在庫で注文 4 が引き当てられる
curl -s -X POST localhost:3000/orders/3/cancel
# => 200 {"order":{"id":3,"status":"Cancelled",...(出荷も Cancelled)},"reallocated_orders":[4]}
```

### シナリオ D: 棚卸調整と受払台帳

```sh
curl -s -X POST localhost:3000/warehouses/TOKYO/stocks/BANANA/adjustments \
  -H 'content-type: application/json' -d '{"delta":-1,"reason":"Damaged"}'
# => 201 {"stock":{...,"on_hand":1,...},"ledger_entry":{...,"delta":-1,"reason":"Adjustment","adjustment_reason":"Damaged"},...}

# 引当済み(注文 4 が TOKYO の APPLE を 5 個押さえている)を下回る調整はできない
curl -s -X POST localhost:3000/warehouses/TOKYO/stocks/APPLE/adjustments \
  -H 'content-type: application/json' -d '{"delta":-9,"reason":"Lost"}'
# => 409 {"code":"adjustment_below_reserved","message":"cannot adjust APPLE at TOKYO: on_hand 10 would fall below reserved 5"}

# 受払台帳。残高(balance_after)を上から追うと、今の在庫数と一致する
curl -s localhost:3000/stocks/BANANA/movements
# => [{"seq":2,"warehouse":"TOKYO","sku":"BANANA","delta":5,"balance_after":5,"reason":"Receipt"},
#     {"seq":5,...,"delta":-3,"balance_after":2,"reason":"Shipment","order_id":1,"shipment_no":1},
#     {"seq":8,...,"delta":-1,"balance_after":1,"reason":"Adjustment","adjustment_reason":"Damaged"}]
```

### 不正な操作

```sh
curl -s -X POST localhost:3000/orders/1/cancel
# => 409 {"code":"invalid_transition","message":"order 1 cannot cancel from Shipped"}
curl -s -X POST localhost:3000/orders/4/shipments/9/ship -H 'content-type: application/json' -d '{"tracking_number":"X"}'
# => 404 {"code":"shipment_not_found","message":"order 4 has no shipment 9"}
```

### エラー一覧

すべて `{"code": "...", "message": "..."}` の形です。

| 状況 | status | code |
|---|---|---|
| 不正な状態遷移(出荷前の出荷、出荷済みのキャンセル、同じ出荷の二重出荷など) | 409 | `invalid_transition` |
| 引当済みを下回る棚卸調整 | 409 | `adjustment_below_reserved` |
| 注文・在庫・出荷が無い | 404 | `order_not_found` / `stock_not_found` / `shipment_not_found` |
| 注文に未知の SKU | 422 | `unknown_sku` |
| 数量 0・明細が空・送り状番号が空など | 422 | `invalid_input` |
| JSON の構文エラー | 400 | `invalid_json` |
| JSON の型不一致(数量に `-1`、未知の棚卸理由など) | 422 | `invalid_json` |
| パスが不正(`/orders/abc`) | 400 | `invalid_path` |
| 内部不整合・保存失敗 | 500 | `internal_error` |

在庫不足はエラーではありません。引当は 200 で入荷待ち(`Backordered`)になり、`shortages` が付きます。

---

## 2. アーキテクチャと各層の責務

```text
          HTTP (JSON)
              │
┌─────────────▼──────────────┐
│ api/                       │  mod.rs: ルーティング  dto.rs: JSON の形
│                            │  handlers.rs: 各エンドポイント  error.rs: エラー → HTTP
└─────────────┬──────────────┘
              │ Sku, WarehouseId, Quantity, OrderLine …
┌─────────────▼──────────────┐
│ service.rs                 │  ユースケース。Mutex で排他し、
│  Service<I, O>             │  「読み込み → コピーを変更 → 成功時のみ保存」
│  + Box<dyn AllocationStrategy>
└───────┬─────────────┬──────┘
        │ 呼ぶ         │ trait 経由で読み書き
┌───────▼───────┐ ┌───▼───────────────────────────┐
│ domain/       │ │ repository.rs                 │
│  types        │ │  trait InventoryRepository    │
│  stock        │ │    (在庫 + 受払台帳)          │
│  ledger       │ │  trait OrderRepository        │
│  order        │ │    (受注 + 出荷)              │
│  shipment     │ │  InMemory*Repository (実装)   │
│  strategy     │ └───────────────────────────────┘
│  allocation   │
│  error        │
└───────────────┘
  ↑ 業務ルールだけ。serde / axum に依存しない

main.rs: 具体的な Repository と引当戦略を選んで Service を組み立て、サーバを起動。シードデータ投入
```

依存の向きは常に「外側 → 内側」です。`domain` は他のどのモジュールも知りません。

### ドメインモデル

```text
Order(集約ルート)
 ├─ lines: [OrderLine { sku, quantity }]         … 何がいくつ欲しいか(SKU ごとに合算済み)
 ├─ status: Pending / Backordered / Allocated / PartiallyShipped / Shipped / Cancelled
 └─ shipments: [Shipment]                        … どこからどう出すか(引当で作られる)
      ├─ no, warehouse
      ├─ lines: [ShipmentLine { sku, quantity }]
      └─ status: AwaitingShipment / Shipped { tracking_number } / Cancelled

Stock(倉庫 × SKU)
 └─ on_hand, reserved  (available = on_hand - reserved)
      ↑ on_hand が変わるたびに StockMovement を 1 行、受払台帳(LedgerEntry)に追記

reserved の中身 = 各注文の「AwaitingShipment の出荷」の数量の合計
```

注文の状態遷移(`src/domain/order.rs` の冒頭に図があります):

```text
Pending ─┬─ allocate(足りる)──▶ Allocated ─ship─▶ PartiallyShipped ─ship─▶ Shipped
         └─ allocate(不足)──▶ Backordered ─(再引当)─▶ Allocated
Pending / Backordered / Allocated ─cancel─▶ Cancelled
```

| 層 | 責務 | やらないこと |
|---|---|---|
| `domain/types.rs` | `Sku` / `WarehouseId` / `Quantity` / `OrderId` の newtype。取り違え防止と値の検証 | — |
| `domain/stock.rs` | 在庫 1 件の不変条件(`reserved <= on_hand`)と、入荷・棚卸・引当・解放・出荷。on_hand を変えたら台帳の行を返す | 受注の知識 |
| `domain/ledger.rs` | 受払台帳の行(`StockMovement`)と、動いた理由 | 保存 |
| `domain/order.rs` | 受注の生成(合算・検証)、状態遷移表、出荷の作成と出荷済みへの更新 | 在庫の知識 |
| `domain/shipment.rs` | 出荷と、その状態・送り状番号 | — |
| `domain/strategy.rs` | 「どの倉庫からいくつ出すか」の計画(trait と 2 つの実装) | 在庫の更新 |
| `domain/allocation.rs` | 受注と在庫をまたぐ操作(引当・出荷・キャンセル・再引当)。all-or-nothing を保証 | 保存 |
| `domain/error.rs` | 業務ルール違反の enum | HTTP ステータス |
| `repository.rs` | 永続化の trait とインメモリ実装 | 業務ルール |
| `service.rs` | ユースケースの手順、排他(Mutex)、「見つからない」の判定、再引当の対象選び | 業務ルールそのもの、HTTP |
| `api/` | ルーティング、JSON ⇄ ドメイン型、エラーの HTTP 化 | 業務ルール |
| `main.rs` | 組み立て(どの実装・どの戦略を使うか)とシード | — |

---

## 3. おすすめの読む順番

1. **`src/domain/types.rs`**:newtype、`Result` / `Option`、`derive`、テストの書き方など、Rust の基本がまとまっています
2. **`src/domain/stock.rs`**:`&self` と `&mut self`、`?` 演算子、`#[must_use]`。在庫の不変条件と、台帳の行を返す設計
3. **`src/domain/ledger.rs`**:受払台帳とは何か。データを持つ enum(`MovementReason`)
4. **`src/domain/shipment.rs` → `src/domain/order.rs`**:集約の考え方、状態遷移表(`ensure_can`)、所有権のムーブ。テストは `src/domain/order/tests.rs`
5. **`src/domain/strategy.rs`**:trait と、その実装 2 つ。イテレータ(`filter_map`、`fold`、`all`)
6. **`src/domain/allocation.rs`**:この教材の山場です。「コピーで計算 → 全部成功したら書き戻す」で all-or-nothing を実現しています。`&dyn Trait` もここで出てきます
7. **`src/domain/error.rs`**:自前のエラー型(`enum` + `Display` + `Error`)
8. **`src/repository.rs`**:trait による抽象化(Swift の protocol、Kotlin の interface にあたります)
9. **`src/service.rs`**:ジェネリクスと `Box<dyn Trait>` の使い分け、クロージャ(`impl FnOnce`)、Mutex = トランザクション。`src/service/error.rs` では `impl From` と `?` によるエラー変換を扱います
10. **`src/service/tests.rs`**:サービス層の振る舞いを、テストを仕様書として読む
11. **`src/api/`**:`mod.rs` → `handlers.rs` → `dto.rs` → `error.rs` の順。axum のハンドラ、`IntoResponse`
12. **`src/main.rs`**:全体の組み立て

**テスト(各ファイルの `mod tests` や `tests.rs`)を先に読む**のもおすすめです。テスト名がそのまま業務ルールの一覧になっています。

---

## 4. 設計上の判断とトレードオフ

### 全体

| 判断 | 理由 | 代わりに諦めたこと |
|---|---|---|
| **在庫と受注を 1 つの Mutex でまとめて守る** | 引当や再引当は在庫と受注を同時に更新します。ロックが 1 つなら途中状態が見えず、デッドロックも起きません | 更新がすべて直列になり、同時実行性が低くなります。DB に移すなら、トランザクションと行ロック(`SELECT ... FOR UPDATE`)で SKU 単位の排他にできます |
| **「読み込み → コピーを変更 → 成功時のみ保存」** | 失敗したら `?` で保存に到達しないので、ロールバック処理を書かなくても原子性が保てます | 毎回コピーするコストがかかります(このデモの規模なら無視できます) |
| **ドメイン関数自体も失敗時に引数を変更しない** | サービス層のコピーとあわせて二重の守りになります。ドメイン単体で all-or-nothing をテストできます | 「コピーして書き戻す」コードが allocation.rs の各関数に出てきます |
| **注文 ID は検証の前に払い出す** | DB の AUTOINCREMENT やシーケンスと同じ振る舞いにそろえるためです | **検証に失敗した注文の ID は欠番になります** |
| **同一 SKU は注文作成時に合算** | 1 SKU = 1 明細にしておくと、引当の計画と明細を突き合わせやすくなります | 利用者が送った明細の行構成は保持されません |
| **NotFound はドメインではなくサービスのエラー** | 「ID で探して無かった」は保存先の事情で、業務ルールではないためです。ただし出荷番号の未存在は Order 集約の中の話なのでドメインにあります | エラー型が 2 段(`DomainError` → `ServiceError`)になります |
| **Repository の戻り値は `Result`** | インメモリでは失敗しませんが、DB では失敗しえます | インメモリ実装では常に `Ok(...)` を返す冗長なコードになります |
| **ドメイン型に serde を付けず DTO を分ける** | JSON の形(API の契約)と内部表現を独立に変えられるようにするためです | 変換コード(`dto.rs`)が増えます |

### 受払台帳

| 判断 | 理由 | 代わりに諦めたこと |
|---|---|---|
| **在庫数(スナップショット)と台帳の両方を持つ** | 台帳だけにして毎回合計する(イベントソーシング)と、照会のたびに全履歴を読むことになります。両方を同じトランザクションで書き、テストで「台帳の合計 = 在庫数」を確かめています | 2 つが食い違う可能性はゼロではありません(必ず同じロックの中で書くことで防いでいます) |
| **台帳に載せるのは on_hand の増減だけ** | 引当は「まだ物が動いていない予約」なので受払ではありません。予約は各注文の出荷が持っています | 「いつ誰が引き当てたか」の履歴は残りません |
| **Stock のメソッドが台帳の行を戻り値で返す(`#[must_use]`)** | 記録漏れを、人の注意ではなくコンパイラの警告で防ぐためです | 呼び出し側が受け取って保存する手間が増えます |
| **台帳に時刻を持たせない(連番のみ)** | 時刻を扱うには時計の注入(テストで固定するため)が必要で、教材の焦点がぼやけるためです | 「いつ」は連番の順序でしか分かりません(発展課題を参照) |

### 注文と出荷

| 判断 | 理由 | 代わりに諦めたこと |
|---|---|---|
| **出荷を Order 集約の子にする** | 「全出荷が終わったら注文も出荷済み」という整合性を、Order 1 つの中で守れます。保存も 1 回で済みます | 実務で出荷を倉庫側の別システム(WMS)が持つ構成なら、出荷は独立した集約にして、イベントで注文側に通知する形になります |
| **出荷は「倉庫ごとに 1 件」を引当時に作る** | 倉庫が違えば荷物も送り状も別になるためです | 同じ倉庫から 2 回に分けて出す(先に揃った分だけ出す)ことはできません |
| **一部でも出荷したらキャンセル不可** | 物はもう倉庫を出ているので、実務では「キャンセル」ではなく「返品」という別の業務です | 残りの出荷だけを取り消す「残キャンセル」は未対応です |

### 複数倉庫と引当

| 判断 | 理由 | 代わりに諦めたこと |
|---|---|---|
| **引当戦略を trait にし、計画と実行を分ける** | どの倉庫から出すかは会社の方針次第です。戦略は計画を返すだけなので、在庫を更新せずにテストできます | 戦略が間違った計画を返す可能性があるため、`Order::allocate` で計画と明細の数量を突き合わせています |
| **戦略は `Box<dyn ...>`、Repository はジェネリクス** | 型引数を増やさず読みやすくするためです。どちらの書き方もあることを見せる意図もあります | 動的ディスパッチの小さなコストがかかります |
| **注文単位の all-or-nothing は維持(部分引当はしない)** | 「全部そろってから出す」方が、分割出荷や出荷漏れの管理が単純になります | 一部だけ先に送ることはできません |

### バックオーダー

| 判断 | 理由 | 代わりに諦めたこと |
|---|---|---|
| **在庫不足はエラーではなく「入荷待ち」** | 在庫切れは日常的に起こることで、注文を断るより待たせるのが普通です。HTTP も 200 で返します | 「入荷待ちにせず断る」はできません |
| **入荷・棚卸での増加・キャンセルと同じトランザクションで再引当する** | 在庫が空いた瞬間に、後から来た手動の引当に取られないようにするためです | 入荷待ちが大量にあると、入荷 API が遅くなります(実務ではキューで非同期にすることが多いです) |
| **再引当は先着順(注文 ID 順)。足りない注文は飛ばす** | 単純で公平に見えるためです | **追い越しが起きます**:先の大きな注文が待っている間に、後ろの小さな注文が在庫を取ることがあります。また、新しい注文を手動で引当すると、入荷待ちの注文より先に在庫を取れます |

---

## 5. 発展課題の候補

- **SQLite への差し替え**
  `rusqlite` などで `InventoryRepository` / `OrderRepository` を実装します。
  一番の学びどころは、在庫・台帳・受注を 1 つのトランザクションでまとめて保存することです。
  今の trait は「1 件ずつ save」なので、トランザクションをどう表現するかを設計する必要があります
  (Unit of Work パターン、`fn transaction(&mut self, f: impl FnOnce(&mut Tx) -> Result<..>)` など)。
  Order と出荷は、`orders` / `shipments` / `shipment_lines` テーブルに分けて保存することになります。
- **冪等キー(二重注文・二重出荷の防止)**
  `Idempotency-Key` ヘッダを受け取り、同じキーなら前回のレスポンスを返します。
  モバイルアプリで通信が不安定なときの再送を想像すると、必要性がよく分かります。
- **部分引当**
  足りる分だけ確保して先に送り、残りを入荷待ちにします。
  明細ごとに「引当済み数」を持つ必要があり、出荷の作り方も変わります。
- **引当の有効期限**
  一定時間出荷されなかった引当を自動で解放し、入荷待ちに回します。
  時刻の注入(`Clock` trait)と、定期実行(tokio のタスク)の設計が題材になります。台帳への時刻の追加もここで一緒にできます。
- **返品**
  出荷済みの注文から戻ってきた物を検品し、良品なら在庫に戻します(台帳に `Return` の行が増えます)。
  不良品を別の在庫区分で持つと、「引当できる在庫」の定義が変わります。
- **出荷後の状態**
  ピッキング → 梱包 → 出荷 → 配達完了・配達不能。運送会社からの通知(Webhook)を受ける API になります。
- **再引当の優先順位**
  先着順ではなく、出荷予定日・顧客ランク・注文の大きさで並べ替えます。並べ替えのルールも trait にできます。
- そのほか
  - SKU 単位のロックに分けて同時実行性を上げる
  - 一覧 API のページングと絞り込み(状態・期間)
  - `axum` のハンドラに対する結合テスト(`tower::ServiceExt::oneshot`)
