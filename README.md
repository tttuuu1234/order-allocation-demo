# order-allocation-demo

受発注・在庫引当サービスのデモ実装です(Rust / axum)。

動くことよりも **読んで学べること** を優先して書いています。
コメントは「何をするか」ではなく「なぜそうしたか」を中心にしています。
Rust 特有の書き方(`?`、`impl From`、所有権の移動、`Option` / `Result` など)には、初めて出てくる箇所に一言説明を添えています。

- 対象: Web API(バックエンド)のみ。画面や認証はありません
- 永続化: インメモリ(再起動で消えます)。Repository は trait にしてあるので、後から SQLite に差し替えられます

---

## 1. 起動方法と curl シナリオ

### 起動

```sh
cargo run
# listening on http://127.0.0.1:3000
```

起動時に、次のデモ用在庫が入ります(`src/main.rs` の `seed`)。

| SKU    | on_hand |
|--------|---------|
| APPLE  | 10      |
| BANANA | 5       |
| ORANGE | 2       |

テストと静的チェック:

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

### API 一覧

| メソッド | パス                      | 内容                          |
|----------|---------------------------|-------------------------------|
| POST     | `/stocks/{sku}/receipts`  | 入荷(未登録 SKU は新規作成) |
| GET      | `/stocks`                 | 在庫一覧                      |
| GET      | `/stocks/{sku}`           | 在庫 1 件                     |
| POST     | `/orders`                 | 受注登録(Pending)            |
| GET      | `/orders`                 | 受注一覧                      |
| GET      | `/orders/{id}`            | 受注 1 件                     |
| POST     | `/orders/{id}/allocate`   | 引当                          |
| POST     | `/orders/{id}/ship`       | 出荷                          |
| POST     | `/orders/{id}/cancel`     | キャンセル                    |

> 依頼文では `:sku` と書いていましたが、axum 0.8 からパス変数の書き方が `{sku}` に変わったため、コード上は `{sku}` です。URL としての使い方は同じです。

エラーはすべて次の形式で返ります。

```json
{"code": "invalid_transition", "message": "order 1 cannot cancel from Shipped"}
```

| code                 | HTTP      | 発生条件                                        |
|----------------------|-----------|-------------------------------------------------|
| `insufficient_stock` | 409       | 引当時の在庫不足(`shortages` に全明細の不足)  |
| `invalid_transition` | 409       | 不正な状態遷移                                  |
| `order_not_found`    | 404       | 注文が無い                                      |
| `stock_not_found`    | 404       | 在庫が無い(`GET /stocks/{sku}`)               |
| `unknown_sku`        | 422       | 注文に未登録の SKU が含まれる                   |
| `invalid_input`      | 422       | 数量 0、明細が空など                            |
| `invalid_json`       | 400 / 422 | JSON の構文エラー(400)、型の不一致(422)     |
| `invalid_path`       | 400       | `/orders/abc` のように ID が数値でない          |
| `internal_error`     | 500       | 内部不整合(本来起きない)                      |

### シナリオ A: 入荷 → 注文 → 引当 → 出荷

サーバを起動した直後の状態から、上から順に実行してください(注文 ID は連番なので、順番が変わると ID もずれます)。

```sh
# 入荷: APPLE を 5 個追加(10 → 15)
curl -s -X POST localhost:3000/stocks/APPLE/receipts \
  -H 'content-type: application/json' -d '{"quantity": 5}'
# {"sku":"APPLE","on_hand":15,"reserved":0,"available":15}

# 注文: APPLE の明細が 2 行あるが、1 行(3+1=4)に合算される
curl -s -X POST localhost:3000/orders \
  -H 'content-type: application/json' \
  -d '{"lines":[{"sku":"APPLE","quantity":3},{"sku":"BANANA","quantity":2},{"sku":"APPLE","quantity":1}]}'
# {"id":1,"status":"Pending","lines":[{"sku":"APPLE","quantity":4},{"sku":"BANANA","quantity":2}]}

# 引当: reserved が増え、available が減る
curl -s -X POST localhost:3000/orders/1/allocate
# {"id":1,"status":"Allocated",...}
curl -s localhost:3000/stocks/APPLE
# {"sku":"APPLE","on_hand":15,"reserved":4,"available":11}

# 出荷: on_hand と reserved の両方が減る(available は変わらない)
curl -s -X POST localhost:3000/orders/1/ship
# {"id":1,"status":"Shipped",...}
curl -s localhost:3000/stocks/APPLE
# {"sku":"APPLE","on_hand":11,"reserved":0,"available":11}
```

### シナリオ B: 在庫不足(all-or-nothing)

```sh
# APPLE は足りるが、BANANA(残り 3)と ORANGE(残り 2)が足りない注文
curl -s -X POST localhost:3000/orders \
  -H 'content-type: application/json' \
  -d '{"lines":[{"sku":"APPLE","quantity":1},{"sku":"BANANA","quantity":10},{"sku":"ORANGE","quantity":5}]}'
# {"id":2,"status":"Pending",...}

curl -s -w ' [%{http_code}]\n' -X POST localhost:3000/orders/2/allocate
# {"code":"insufficient_stock",
#  "message":"insufficient stock: BANANA (requested 10, available 3), ORANGE (requested 5, available 2)",
#  "shortages":[{"sku":"BANANA","requested":10,"available":3,"missing":7},
#               {"sku":"ORANGE","requested":5,"available":2,"missing":3}]} [409]

# 足りていた APPLE も引き当てられておらず、注文も Pending のまま
curl -s localhost:3000/stocks/APPLE
# {"sku":"APPLE","on_hand":11,"reserved":0,"available":11}
curl -s localhost:3000/orders/2
# {"id":2,"status":"Pending",...}
```

### シナリオ C: キャンセル

```sh
# 引当済みの注文をキャンセルすると、引当が解放される
curl -s -X POST localhost:3000/orders \
  -H 'content-type: application/json' -d '{"lines":[{"sku":"ORANGE","quantity":2}]}'
# {"id":3,"status":"Pending",...}
curl -s -X POST localhost:3000/orders/3/allocate
curl -s localhost:3000/stocks/ORANGE
# {"sku":"ORANGE","on_hand":2,"reserved":2,"available":0}
curl -s -X POST localhost:3000/orders/3/cancel
# {"id":3,"status":"Cancelled",...}
curl -s localhost:3000/stocks/ORANGE
# {"sku":"ORANGE","on_hand":2,"reserved":0,"available":2}

# Pending の注文もキャンセルできる(在庫には触れない)
curl -s -X POST localhost:3000/orders/2/cancel
# {"id":2,"status":"Cancelled",...}

# 出荷済みはキャンセルできない
curl -s -w ' [%{http_code}]\n' -X POST localhost:3000/orders/1/cancel
# {"code":"invalid_transition","message":"order 1 cannot cancel from Shipped"} [409]
```

### その他のエラー例

```sh
# 未知の SKU を含む注文 → 422(未知の SKU はまとめて返る)
curl -s -X POST localhost:3000/orders -H 'content-type: application/json' \
  -d '{"lines":[{"sku":"GRAPE","quantity":1}]}'
# {"code":"unknown_sku","message":"unknown SKU: GRAPE"}

# 数量 0 → 422
curl -s -X POST localhost:3000/orders -H 'content-type: application/json' \
  -d '{"lines":[{"sku":"APPLE","quantity":0}]}'
# {"code":"invalid_input","message":"quantity for APPLE must be greater than 0"}

# 壊れた JSON → 400
curl -s -X POST localhost:3000/orders -H 'content-type: application/json' -d '{"lines":['
# {"code":"invalid_json","message":"Failed to parse the request body as JSON: ..."}

# 存在しない注文 → 404
curl -s localhost:3000/orders/999
# {"code":"order_not_found","message":"order 999 not found"}
```

---

## 2. アーキテクチャ

```text
            HTTP (JSON)
                │
┌───────────────▼─────────────────┐
│ api.rs                          │  ハンドラ / DTO / エラー → HTTP ステータス変換
│   ApiError, *Request, *Response │  (HTTP と JSON の都合はここで止める)
└───────────────┬─────────────────┘
                │ ドメイン型 (Sku, Quantity, OrderLine …)
┌───────────────▼─────────────────┐
│ service.rs                      │  ユースケース。ロック → 読み込み →
│   Service<S, O>, ServiceError   │  コピーをドメインで変更 → 成功時のみ保存
└──────┬──────────────────┬───────┘
       │ 呼ぶ              │ trait 経由で読み書き
┌──────▼───────────┐ ┌────▼─────────────────────────────┐
│ domain/          │ │ repository.rs                    │
│  types.rs        │ │  trait StockRepository           │
│  stock.rs        │ │  trait OrderRepository           │
│  order.rs        │ │  InMemoryStockRepository  ┐      │
│  allocation.rs   │ │  InMemoryOrderRepository  ┴ BTreeMap
│  error.rs        │ └──────────────────────────────────┘
└──────────────────┘
  ↑ 他の層に依存しない(serde / axum を知らない)

main.rs: 具体的な Repository を選び、Service と Router を組み立てて起動する
```

依存の向きは常に「上から下」です。`domain` はどの層にも依存しません。

| 層 | ファイル | 責務 | 持たないもの |
|----|----------|------|--------------|
| ドメイン | `src/domain/` | 業務ルール。数量の計算、状態遷移、all-or-nothing の引当判定 | HTTP、JSON、保存、ロック |
| Repository | `src/repository.rs` | 保存と取り出し。読み取りは常に「コピー」を返す | 業務ルール |
| サービス | `src/service.rs` | ユースケースの手順。排他制御と、成功時のみ保存する原子性 | HTTP、JSON |
| API | `src/api.rs` | JSON ⇔ ドメイン型の変換、エラー → ステータスコード | 業務ルール |
| 起動 | `src/main.rs` | 依存の組み立て、シードデータ | ロジック |

`src/lib.rs` はモジュールの宣言だけです。ライブラリにしてあるのは、テストから各層を直接使えるようにするためです。

---

## 3. おすすめの読む順番

1. **`src/domain/types.rs`**: newtype(`Sku`, `Quantity`, `OrderId`)。`struct`、`derive`、`Result`、`Option` の基本がここに出てきます
2. **`src/domain/error.rs`**: 自前のエラー型。Rust のエラーは「ただの enum + トレイト実装」だと分かります
3. **`src/domain/stock.rs`**: `&self` と `&mut self`、`?` 演算子。不変条件 `reserved <= on_hand` の守り方
4. **`src/domain/order.rs`**: 状態遷移表(`match (状態, 操作)`)と、同一 SKU の合算。所有権のムーブ
5. **`src/domain/allocation.rs`**: 引当・出荷・キャンセル。all-or-nothing の実装の核心です
6. **`src/repository.rs`**: trait による抽象化(Swift の protocol / Kotlin の interface)
7. **`src/service.rs`**: ジェネリクス、`Mutex`、`impl From` と `?` によるエラー変換。「読み込み → コピーを変更 → 成功時のみ保存」の流れ
8. **`src/api.rs`**: axum のハンドラ、DTO、エラーの HTTP 化
9. **`src/main.rs`**: 全体の組み立て
10. **`src/service/tests.rs`**: ユースケースの仕様一覧として読めます

各ファイル末尾の `#[cfg(test)] mod tests` も、仕様書の代わりとして読めます(テスト名は日本語です)。

---

## 4. 設計上の判断とトレードオフ

| 判断 | 理由 | 諦めたこと |
|------|------|------------|
| **在庫と注文を 1 つの `Mutex` で守る** | 引当では在庫と注文を同時に更新します。ロックを 1 つにすれば、途中状態が他から見えることも、デッドロックも起きません | 同時実行性。無関係な SKU どうしの注文も直列に処理されます。DB なら行ロックで SKU ごとに並行処理できます |
| **注文 ID は検証の前に払い出す** | DB の AUTOINCREMENT やシーケンスと同じ振る舞いにそろえるため | 検証に失敗すると ID が**欠番**になります(テスト `検証に失敗した注文の_id_は欠番になる`) |
| **Repository は値(コピー)を返し、成功時のみ `save`** | 失敗したら保存しないだけで原子性が保てます。トランザクションのロールバックと同じ効果です | 読み込みのたびに clone するコスト。大量のデータには向きません |
| **ドメイン関数自体も「失敗時に引数を変えない」** | サービス層のコピーに頼らなくても、ドメイン単体で正しさをテストできるようにするため | 在庫の配列をもう一度コピーするので、二重の守りになり冗長です |
| **注文時に同一 SKU を合算** | 1 SKU = 1 明細になり、引当で在庫と明細を素直に突き合わせられます | 利用者が送った明細の行構成(例: 行ごとの備考)は保持できません |
| **未知の SKU は注文時にエラー(422)** | 存在しない商品の注文を Pending で溜めないため | 「先に注文を受けて後から商品登録」という運用はできません |
| **引当は注文登録と分離(Pending → allocate)** | 在庫が無くても注文自体は受け付け、入荷後に引き当てられます | 引当は明示的に呼ぶ必要があります。自動引当はしていません |
| **Repository の戻り値を `Result` にする** | インメモリでは失敗しませんが、DB に差し替えたときにシグネチャを変えずに済みます | 今は常に `Ok` なので、冗長に見えます |
| **ハンドラは具体型 `AppService` に固定** | ジェネリックなハンドラは型注釈が増えて読みにくくなるため | 差し替え時は型エイリアスを 1 行変える必要があります |
| **エラー型・ID 採番を自前実装** | 外部依存を最小にし、仕組みを見えるようにするため | `thiserror` などを使った場合より記述量が増えます |
| **`std::sync::Mutex` を async ハンドラから使う** | ロック中に `.await` しない短い処理なので、tokio の Mutex より軽いです | ロック中の処理が重くなると、ランタイムのスレッドを塞ぎます |
| **Mutex の poison(panic 後の汚染)を無視** | 保存を最後にまとめているので、panic しても途中状態は保存されません | panic の原因が別にあっても、気づきにくくなります |

---

## 5. 発展課題の候補

### SQLite への差し替え
`StockRepository` / `OrderRepository` を実装した `SqliteStockRepository` などを作ります(例: `rusqlite` や `sqlx`)。考えどころは次のとおりです。
- 今は在庫と注文が別々の Repository です。1 つのトランザクションで両方を更新するには、接続(トランザクション)を共有する仕組みが要ります。Unit of Work パターンや、Repository をまとめる trait を検討します
- `Mutex` の代わりに、`BEGIN IMMEDIATE` や行ロックで排他します
- `next_id` は AUTOINCREMENT に置き換えます

### 冪等キー(二重注文の防止)
`POST /orders` で `Idempotency-Key` ヘッダを受け付け、同じキーなら前回の結果を返します。キーと結果の保存先、保持期間、「同じキーなのに本文が違う」場合の扱いを考えます。

### 部分引当
足りる分だけ引き当て、残りを後から引き当てます。明細ごとに `allocated_quantity` を持たせることになり、状態にも `PartiallyAllocated` が増えます。all-or-nothing と比べると、今の設計の単純さがよく分かります。

### 引当の有効期限
`Allocated` のまま一定時間出荷されなければ、自動で引当を解放します。期限切れの検出方法(定期ジョブ、または読み込み時の遅延判定)と、同時に出荷 API が呼ばれた場合の競合を考えます。

### そのほか
- API 層の結合テスト(`tower::ServiceExt::oneshot` でルータを直接呼ぶ)
- SKU ごとのロックで並行性を上げる
- 入荷・引当・出荷の履歴(在庫の移動記録)を残す
