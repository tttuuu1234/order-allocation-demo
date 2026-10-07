# order-allocation-demo

受発注・在庫引当サービスを Rust(axum)で実装したデモです。
「動けばよい」より **「読んで業務設計と Rust を学べる」** ことを優先して書いています。
コメントは日本語で、Rust 特有の書き方には初出の箇所に一言添えています。

- Web API のみ(画面は無し)。認証やマルチテナントは対象外
- 永続化はインメモリ。Repository を trait で抽象化しているので SQLite などに差し替えられます
- 依存クレートは axum / tokio / serde / serde_json だけ。エラー型は自前、ID は連番

---

## 1. 起動方法と curl シナリオ

```sh
cargo run            # http://127.0.0.1:3000 で起動
cargo test           # ユニットテスト
cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

起動時に次の在庫が入ります(`src/main.rs` の `seed`)。以下のシナリオはこの数量が前提です。
サーバを起動し直すとデータは初期状態に戻ります。

| SKU    | on_hand |
|--------|---------|
| APPLE  | 10      |
| BANANA | 5       |
| ORANGE | 2       |

### シナリオ A: 入荷 → 注文 → 引当 → 出荷

```sh
# 入荷(未登録の SKU は新規作成される)
curl -s -X POST localhost:3000/stocks/GRAPE/receipts \
  -H 'content-type: application/json' -d '{"quantity":4}'
# => 201 {"sku":"GRAPE","on_hand":4,"reserved":0,"available":4}

curl -s localhost:3000/stocks

# 注文(APPLE が 2 行あるが、1 行に合算される)
curl -s -X POST localhost:3000/orders -H 'content-type: application/json' \
  -d '{"lines":[{"sku":"APPLE","quantity":2},{"sku":"BANANA","quantity":1},{"sku":"APPLE","quantity":1}]}'
# => 201 {"id":1,"status":"Pending","lines":[{"sku":"APPLE","quantity":3},{"sku":"BANANA","quantity":1}]}

# 引当: reserved が増え、available が減る
curl -s -X POST localhost:3000/orders/1/allocate
# => 200 {"id":1,"status":"Allocated",...}
curl -s localhost:3000/stocks/APPLE
# => {"sku":"APPLE","on_hand":10,"reserved":3,"available":7}

# 出荷: on_hand と reserved が両方減る(available は変わらない)
curl -s -X POST localhost:3000/orders/1/ship
# => 200 {"id":1,"status":"Shipped",...}
curl -s localhost:3000/stocks/APPLE
# => {"sku":"APPLE","on_hand":7,"reserved":0,"available":7}
```

### シナリオ B: 在庫不足(all-or-nothing)

```sh
# APPLE は足りるが BANANA と ORANGE が足りない注文
curl -s -X POST localhost:3000/orders -H 'content-type: application/json' \
  -d '{"lines":[{"sku":"APPLE","quantity":3},{"sku":"BANANA","quantity":9},{"sku":"ORANGE","quantity":5}]}'
# => 201 {"id":2,"status":"Pending",...}

curl -s -X POST localhost:3000/orders/2/allocate
# => 409
# {"code":"insufficient_stock",
#  "message":"insufficient stock: BANANA (requested 9, available 4), ORANGE (requested 5, available 2)",
#  "shortages":[{"sku":"BANANA","requested":9,"available":4,"missing":5},
#               {"sku":"ORANGE","requested":5,"available":2,"missing":3}]}

# 足りていた APPLE も含め、在庫は何も確保されず、注文も Pending のまま
curl -s localhost:3000/stocks/APPLE    # reserved は 0 のまま
curl -s localhost:3000/orders/2        # status は Pending
```

### シナリオ C: キャンセル

```sh
curl -s -X POST localhost:3000/orders -H 'content-type: application/json' \
  -d '{"lines":[{"sku":"ORANGE","quantity":2}]}'
# => 201 {"id":3,...}
curl -s -X POST localhost:3000/orders/3/allocate     # ORANGE の available が 0 になる
curl -s -X POST localhost:3000/orders/3/cancel
# => 200 {"id":3,"status":"Cancelled",...}
curl -s localhost:3000/stocks/ORANGE
# => {"sku":"ORANGE","on_hand":2,"reserved":0,"available":2}  引当が解放された

# 不正な状態遷移は 409
curl -s -X POST localhost:3000/orders/1/cancel
# => 409 {"code":"invalid_transition","message":"order 1 cannot cancel from Shipped"}
curl -s -X POST localhost:3000/orders/2/ship
# => 409 {"code":"invalid_transition","message":"order 2 cannot ship from Pending"}
```

### エラー一覧

すべて `{"code": "...", "message": "..."}` の形です(在庫不足のみ `shortages` が付きます)。

| 状況 | status | code |
|---|---|---|
| 在庫不足 | 409 | `insufficient_stock` |
| 不正な状態遷移 | 409 | `invalid_transition` |
| 注文が無い / 在庫が無い | 404 | `order_not_found` / `stock_not_found` |
| 注文に未知の SKU | 422 | `unknown_sku` |
| 数量 0・明細が空など、業務ルール上の入力不正 | 422 | `invalid_input` |
| JSON の構文エラー | 400 | `invalid_json` |
| JSON の型不一致(数量に `-1` など) | 422 | `invalid_json` |
| パスが不正(`/orders/abc`) | 400 | `invalid_path` |
| 内部不整合・保存失敗 | 500 | `internal_error` |

※ ルーティングは axum 0.8 の書き方なので、コード上のパスは `/stocks/{sku}` です(0.7 までは `:sku`)。

---

## 2. アーキテクチャと各層の責務

```text
          HTTP (JSON)
              │
┌─────────────▼──────────────┐
│ api.rs                     │  DTO ⇄ ドメイン型の変換、エラー → HTTP ステータス
└─────────────┬──────────────┘
              │ Sku, Quantity, OrderLine …
┌─────────────▼──────────────┐
│ service.rs                 │  ユースケース。Mutex で排他し、
│  Service<S, O>             │  「読み込み → コピーを変更 → 成功時のみ保存」
└───────┬─────────────┬──────┘
        │ 呼ぶ         │ trait 経由で読み書き
┌───────▼──────┐ ┌────▼────────────────────────┐
│ domain/      │ │ repository.rs               │
│  types       │ │  trait StockRepository      │
│  stock       │ │  trait OrderRepository      │
│  order       │ │  InMemory*Repository (実装) │
│  allocation  │ └─────────────────────────────┘
│  error       │
└──────────────┘
  ↑ 業務ルールだけ。serde / axum に依存しない

main.rs: 具体的な Repository を選んで Service を組み立て、サーバを起動。シードデータ投入
```

依存の向きは常に「外側 → 内側」です。`domain` は他のどのモジュールも知りません。

| 層 | 責務 | やらないこと |
|---|---|---|
| `domain/types.rs` | `Sku` / `Quantity` / `OrderId` の newtype。取り違え防止と値の検証 | — |
| `domain/stock.rs` | 在庫 1 件の不変条件(`reserved <= on_hand`)と、入荷・引当・解放・出荷 | 受注の知識 |
| `domain/order.rs` | 受注の生成(合算・検証)と状態遷移表 | 在庫の知識 |
| `domain/allocation.rs` | 受注と在庫をまたぐ操作(引当・出荷・キャンセル)。all-or-nothing を保証 | 保存 |
| `domain/error.rs` | 業務ルール違反の enum | HTTP ステータス |
| `repository.rs` | 永続化の trait とインメモリ実装 | 業務ルール |
| `service.rs` | ユースケースの手順、排他(Mutex)、「見つからない」の判定 | 業務ルールそのもの、HTTP |
| `api.rs` | ルーティング、JSON ⇄ ドメイン型、エラーの HTTP 化 | 業務ルール |
| `main.rs` | 組み立て(どの実装を使うか)とシード | — |

---

## 3. おすすめの読む順番

1. **`src/domain/types.rs`** — newtype、`Result` / `Option`、`derive`、テストの書き方など、Rust の基本がまとまっています
2. **`src/domain/stock.rs`** — `&self` と `&mut self`、`?` 演算子。在庫の不変条件
3. **`src/domain/order.rs`** — 状態遷移表(`match (状態, 操作)`)、所有権のムーブ、同一 SKU の合算
4. **`src/domain/allocation.rs`** — この教材の山場です。all-or-nothing を「先に全部調べる → コピーで変更 → 書き戻す」で実現しています
5. **`src/domain/error.rs`** — 自前のエラー型(`enum` + `Display` + `Error`)
6. **`src/repository.rs`** — trait による抽象化(Swift の protocol、Kotlin の interface にあたります)
7. **`src/service.rs`** — ジェネリクス、`impl From` と `?` によるエラー変換、Mutex = トランザクション
8. **`src/service/tests.rs`** — サービス層の振る舞いを、テストを仕様書として読む
9. **`src/api.rs`** — axum のハンドラ、DTO、`IntoResponse`
10. **`src/main.rs`** — 全体の組み立て

**ドメインのテスト(各ファイル末尾の `mod tests`)を先に読む**のもおすすめです。テスト名がそのまま業務ルールの一覧になっています。

---

## 4. 設計上の判断とトレードオフ

| 判断 | 理由 | 代わりに諦めたこと |
|---|---|---|
| **在庫と受注を 1 つの Mutex でまとめて守る** | 引当は在庫と受注を同時に更新します。ロックが 1 つなら途中状態が見えず、デッドロックも起きません | 更新がすべて直列になり、同時実行性が低くなります。無関係な SKU の引当同士も待ち合います。DB に移すなら、トランザクションと行ロック(`SELECT ... FOR UPDATE`)で SKU 単位の排他にできます |
| **「読み込み → コピーを変更 → 成功時のみ保存」** | 失敗したら `?` で保存に到達しないので、ロールバック処理を書かなくても原子性が保てます | 毎回コピーするコストがかかります(このデモの規模なら無視できます) |
| **ドメイン関数自体も失敗時に引数を変更しない** | サービス層のコピーとあわせて二重の守りになります。ドメイン単体で all-or-nothing をテストできます | 同じ「コピーして書き戻す」コードが allocation.rs に 3 回出てきます |
| **注文 ID は検証の前に払い出す** | DB の AUTOINCREMENT やシーケンスと同じ振る舞いにそろえるためです | **検証に失敗した注文の ID は欠番になります**(テスト `検証に失敗した注文の_id_は欠番になる`) |
| **同一 SKU は注文作成時に合算** | 1 SKU = 1 明細にしておくと、引当で在庫と明細を 1 対 1 で突き合わせられます | 利用者が送った明細の行構成(行ごとの備考など)は保持されません |
| **在庫不足・未知 SKU はまとめて返す** | 1 件ずつエラーを直す往復をなくすためです | 判定を最後まで続ける分、コードが少し長くなります |
| **注文作成時には在庫を確保しない(Pending)** | 「受注」と「引当」を別の業務イベントとして分けるためです | 作成直後に在庫があっても、引当までに他の注文に取られることがあります |
| **NotFound はドメインではなくサービスのエラー** | 「ID で探して無かった」は保存先の事情で、業務ルールではないためです | エラー型が 2 段(`DomainError` → `ServiceError`)になります |
| **Repository の戻り値は `Result`** | インメモリでは失敗しませんが、DB では失敗しえます。trait のシグネチャを今から DB 向けにしておくためです | インメモリ実装では常に `Ok(...)` を返す冗長なコードになります |
| **`std::sync::Mutex` + 同期の Repository** | ロック中に `.await` しないなら std の Mutex の方が軽くて単純です | 非同期 DB ドライバ(sqlx など)を使うときは、trait を async にするか `spawn_blocking` が必要になります |
| **API 層は具体型(`AppService` 型エイリアス)で書く** | ハンドラをジェネリクスにすると読みにくくなるためです | 差し替え時に型エイリアスの 1 行を書き換える必要があります |
| **ドメイン型に serde を付けず DTO を分ける** | JSON の形(API の契約)と内部表現を独立に変えられるようにするためです | 変換コード(`impl From<&Order> for OrderResponse` など)が増えます |
| **エラー型は自前実装(thiserror 不使用)** | エラーが「ただの enum + トレイト実装」であることを見えるようにするためです | `Display` の手書きが多くなります |

---

## 5. 発展課題の候補

- **SQLite への差し替え**
  `rusqlite` などで `StockRepository` / `OrderRepository` を実装します。
  注意点は、在庫と受注を 1 つのトランザクションでまとめて保存することです。
  今の trait は「1 件ずつ save」なので、トランザクションをどう表現するか(Unit of Work パターン、
  `fn transaction(&mut self, f: impl FnOnce(&mut Tx) -> Result<..>)` など)を設計するのが一番の学びどころです。
- **冪等キー(二重注文の防止)**
  `Idempotency-Key` ヘッダを受け取り、同じキーなら前回のレスポンスを返します。
  モバイルアプリで通信が不安定なときの再送を想像すると、必要性がよく分かります。
- **部分引当**
  足りる分だけ確保し、残りをバックオーダーとして持ちます。
  明細ごとに `allocated_quantity` を持たせることになり、状態遷移(`PartiallyAllocated`)も増えます。
- **引当の有効期限**
  一定時間出荷されなかった引当を自動解放します。時刻をどう注入するか(テストで時刻を固定するために
  `Clock` trait を作るなど)と、定期実行(tokio のタスク)の設計が題材になります。
- そのほか
  - SKU 単位のロックに分けて同時実行性を上げる
  - 在庫の増減履歴(入出庫ログ)を残す
  - `axum` のハンドラに対する結合テスト(`tower::ServiceExt::oneshot`)
