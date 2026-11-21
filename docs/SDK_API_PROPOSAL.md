# SDK 化に向けた公開 API の整理案

EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.

状態: **承認済み・実装中**（2026-11-21 に所有者が §6 の 4 点をいずれも提案どおりに決定。DECISIONS D-039）。進み具合は §7 の表に記録する。
対象は `crates/sync-core`（同期コア）と `crates/lab-storage`（SQLite ストア）。commit `b539311` 時点の内容に基づく。

## 1. この SDK が約束する範囲

**やること**: BIP352 の受信側の同期と検証。ブロック範囲のスキャン、永続同期（再開・reorg）、2 つのデータ源の突き合わせ、
そして「どの証拠に基づく検出か」の区別。

**やらないこと（この整理の後も変わらない）**: 送金・署名、残高、鍵の保管、mainnet。
暗号の計算は上流 `bdk_sp` に任せる（CLAUDE.md 規約 1・2・5・6）。

**安定性の約束**: バージョンは `0.x` のまま。破壊的変更はあり得るが、変更は公開 API の一覧（§5.4）に現れる形で行う。

## 2. 現状の棚卸し

`sync-core` の公開項目は約 110。モジュールごとの数と、リポジトリ内での使われ方は次のとおり。

| モジュール | 公開項目 | CLI が使う | ストアが使う | テスト専用（testkit / tests だけが使う） |
|---|---|---|---|---|
| `network` | 11 | `LabNetwork`, `ChainIdentity` | `ChainIdentity`（間接） | `NetworkError`, `ChainIdentity::verified` |
| `protocol` | 28 | `ReceiverKeys`, `ReceiveScanner::new` | — | `scan_tx`, `scan_indexed_tx`, `eligibility`, `public_tweak_data*`, `with_exact_labels`, `ScanMatch`, `TxScanOutcome`, `SkipReason`, `ProtocolError` |
| `engine` | 6 | `scan_range`, `ScanReport`, `ScanError` | — | `SCAN_CHUNK_BLOCKS`, `MAX_INDEXED_OUTPUTS_PER_BLOCK`。`ScanCounters` は誰も使っていない |
| `models` | 4 | すべて | すべて | — |
| `source`（トレイトと型） | 12 | `ScanSource`, `SourceError`, `SourceStats` | — | `Evidence`, `BlockEvidence`, `ScanBatch`, `IndexedTx`, `IndexedOutput`, `TxWithPrevouts`, `SourceCapabilities` |
| `source::bitcoin_core` | 4 | `CoreRestSource` | — | — |
| `source::blindbit` | 4 | `BlindbitHttpSource` | — | `header_source` |
| `source::fixture` | 11 | `FixtureSource` | — | `FixtureFile` ほかファイル形式の型 |
| `source::local_index` | 6 | `LocalTweakIndex`, `IndexPolicy` | — | `inner` |
| `store` | 11 | `SyncStore`, `StoreMeta`, `SCANNER_VERSION` | `check_append`, `sort_detections` ほか | `MemoryStore` とその故障注入フック |
| `sync` | 8 | すべて（`ReorgReport` は型名では参照なし） | — | — |
| `verify` | 5 | `cross_check`, `VerificationReport` | — | `Explanation`, `Side` |

`lab-storage` の公開項目は 5（`SqliteStore::{create, open, in_memory}`、`SCHEMA_VERSION`）で、整理の必要はほぼ無い。

## 3. 現状の問題点

1. **入口が分からない。** 8 つのモジュールがすべて同じ重みで公開されており、「組み込む人がまず使うもの」と「内部の部品」の区別が無い。クレートの先頭にある説明は実験の経緯で、使い方の例が無い。
2. **テストのための公開が混ざっている。** `MemoryStore::fail_appends_after` / `stop_failing`（故障注入）、`ReceiveScanner::with_exact_labels`、`ScanCounters`、各種の定数など。
3. **実験用の名前が残っている。** `LabNetwork`、`LabAddress`、`EXPERIMENTAL_BANNER`。意味（mainnet を表現できない型）は SDK でも残すが、名前は実験の名残り。
4. **テストデータ用のアダプタが本体にある。** `source::fixture`（検証なしの prevout を持つオフライン形式）と、`IndexPolicy::IgnoreSegwitVersionRule`（BIP に準拠しない索引の再現）は、実験と診断のためのもの。
5. **型が進化に耐えない。** レポート構造体はフィールドが公開で `#[non_exhaustive]` が無く、フィールドを 1 つ足すだけで破壊的変更になる。エラー enum も同様（付いているのは `Evidence` だけ）。
6. **秘密に触れる API の位置づけが曖昧。** `ScanMatch::sensitive_output_tweak` は公開だが、使っているのはテストだけ。一方で、将来のウォレットが出力を使う（支出する）には出力ごとの tweak が必要になる。**SDK がそれをどう渡すかが未設計**（§6 の問い 1）。
7. **鍵の入口が 1 つしかない。** `ReceiverKeys::new(scan 秘密鍵, spend 公開鍵)`。CLI は公開テスト鍵を埋め込んでいる。SDK としては十分に小さいが、鍵をどこから持ってくるか（ファイル、環境、OS のキーストア）は SDK の外と決めておく必要がある。

## 4. 提案する公開 API

3 つの層に分ける。**層 A だけで普通の組み込みは完結する**ことを目標にする。

### 層 A: 入口（クレートの直下から使えるようにする）

```rust
use silent_sync::{ReceiverKeys, ReceiveScanner, Network, SyncOptions};
use silent_sync::source::CoreRestSource;          // feature "core-rest"
use silent_sync_sqlite::SqliteStore;              // 別クレート

let keys = ReceiverKeys::new(scan_secret_key, spend_public_key);
let scanner = ReceiveScanner::new(&keys, &[/* 追加のラベル */])?;
let source = CoreRestSource::connect("http://127.0.0.1:38332")?;
let mut store = SqliteStore::open(path)?;          // 初回は create + initialize
let report = silent_sync::sync(&source, &scanner, &keys.wallet_id(), &mut store, &SyncOptions::default())?;
for output in &report.new_detections { /* txid, vout, 金額, ラベル, 証拠の種類 */ }
```

| 分類 | 項目 |
|---|---|
| 鍵とスキャナ | `ReceiverKeys`, `ReceiveScanner::{new, labels}`, アドレス型（現 `LabAddress`） |
| ネットワーク | 現 `LabNetwork`（mainnet を持たない）, `ChainIdentity` |
| 実行 | `scan_range`, `sync`, `sync_with_progress`, `cross_check`, `new_store_meta` |
| 設定と結果 | `SyncOptions`, `ScanReport`, `SyncReport`, `ReorgReport`, `StopReason`, `VerificationReport`, `Difference`, `Explanation`, `Side` |
| データ | `DetectedOutput`, `BlockRef`, `EvidenceKind`, `MatchProvenance` |
| エラー | `ScanError`, `SyncError`, `SourceError`, `StoreError`, `ProtocolError`, `NetworkError` |
| データ源 | `CoreRestSource`（feature `core-rest`）, `BlindbitHttpSource`（feature `blindbit-http`） |
| ストア | `SyncStore`, `StoreMeta`, `MemoryStore`（故障注入フックを除く）, `SqliteStore`（別クレート） |

### 層 B: 拡張点（自分でデータ源やストアを実装する人向け。モジュールの下に残す）

| 用途 | 項目 |
|---|---|
| データ源を実装する | `ScanSource`, `SourceCapabilities`, `SourceStats`, `ScanBatch`, `BlockEvidence`, `Evidence`, `TxWithPrevouts`, `IndexedTx`, `IndexedOutput` |
| ストアを実装する | `SyncStore`, `check_append`, `sort_detections`, `MAX_STORABLE_SATS`, `SCANNER_VERSION` |

層 B は「実装する側が守るべき契約」を伴う（例: `append` は `check_append` を満たすこと、検出は `sort_detections` の順で返すこと）。契約は型のドキュメントに書き、`lab-testkit` の同期スイートを「自作ストアの適合テスト」として使えることを案内する。

### 層 C: 公開をやめる・移す

| 項目 | 提案 | 理由 |
|---|---|---|
| `MemoryStore::{fail_appends_after, stop_failing}` | `lab-testkit` のラッパーに移す | 故障注入はテストの道具 |
| `ReceiveScanner::with_exact_labels` | `#[doc(hidden)]` か testkit 側へ | 公式ベクトルの検証専用 |
| `ReceiveScanner::{scan_tx, scan_indexed_tx, eligibility, public_tweak_data*}`, `TxScanOutcome`, `SkipReason`, `ScanMatch` | 新しい feature `low-level`（既定で無効）の下に置く | tx 単位の部品。通常は `scan_range` / `sync` 経由で使う。testkit と `local_index` は内部で使うので機能は残す |
| `ScanCounters`, `SCAN_CHUNK_BLOCKS`, 各 `*_BACKEND_ID` 定数 | 非公開（必要な値はメソッドで返す） | 内部の詳細 |
| `source::fixture` | feature `fixture`（既定で無効）。CLI は有効にする | prevout を検証できないオフライン形式。組み込み先に既定で入れるものではない |
| `LocalTweakIndex`, `IndexPolicy` | feature `local-index`（既定で無効）。CLI は有効にする | 突き合わせ・診断用。`IgnoreSegwitVersionRule` は準拠しない索引の再現 |
| `coinbase_height` | 非公開 | 内部の補助関数 |
| `EXPERIMENTAL_BANNER` | CLI に移す | 表示は CLI の責務。クレートの説明には注意書きを残す |
| `pub use bdk_sp::bitcoin` | **残す**（`silent_sync::bitcoin`） | 公開 API が `Txid` / `BlockHash` などを使うので、利用側がバージョンを合わせられるようにする |

### 名前の変更（任意。§6 の問い 3）

| 現在 | 案 | 備考 |
|---|---|---|
| クレート `sync-core` | `silent-sync` | バイナリ名と揃う。crates.io に出す場合は空き状況の確認が要る（**未確認**） |
| クレート `lab-storage` | `silent-sync-sqlite` | |
| `LabNetwork` | `Network` | mainnet を持たないことは型の説明に明記 |
| `LabAddress` | `SilentPaymentAddress` | |

名前を変えると差分は大きくなるが、機械的な置換で済む。いま変えるのが一番安い。

## 5. 併せて行うこと

1. **進化に耐える型にする。** レポート構造体・エラー enum・`SyncOptions` に `#[non_exhaustive]` を付ける（`SyncOptions` は `Default` と組み合わせて使う形にする）。
2. **ドキュメント。** クレート先頭に「何をする／しない」「最小の使用例（fixture でコンパイル・実行されるドキュメントテスト）」「証拠の種類の違い」を書く。層 A・B に `#![warn(missing_docs)]`。
3. **使用例。** `examples/` に 3 本: 自分のノードから同期、インデックスと突き合わせ、自作ストアの実装（最小）。
4. **公開 API の一覧をリポジトリに固定する。** 公開項目の一覧をファイルに書き出し、テストで実際の公開範囲と突き合わせる。API の変更が、必ず差分として見えるようにするため（方法は実装時に決める。nightly に依存しない方法を優先）。
5. **変えないもの。** 検査の内容、fail-closed の挙動、エラーの分類、ストアの形式。公開範囲と名前だけを動かし、既存のテスト・無効化チェック・故障注入はそのまま通す。

## 6. 所有者に決めていただいたこと

**決定（2026-11-21）**: 1 → (a) 渡さない。2 → 既定で無効。3 → 名前を変える。4 → feature の下に残す。以下は決定時の説明。

1. **出力ごとの tweak を SDK から渡すか。**【人間レビュー対象: 秘密情報の扱い】
   将来のウォレットが受け取った出力を使うには、出力ごとの tweak（秘密）が必要になる。いまは検出結果に含めていない（規約 3）。選択肢:
   - (a) **渡さない**（現状維持）。必要になったら、該当 tx を再スキャンして取り出す API を後から足す。
   - (b) 専用の型で渡す（`Debug` / シリアライズ不可、ストアには保存しない）。
   - 提案は **(a)**。SDK の範囲を「受信の検出」に保ち、秘密を扱う面を増やさない。ウォレット化を決めた時点で (b) を設計する。
2. **`fixture` と `local-index` を既定で無効にしてよいか。** 提案は「無効」（組み込み先には、実ノードと Oracle のデータ源だけを見せる）。CLI は両方を有効にするので、いまのコマンドはそのまま動く。
3. **名前を変えるか**（§4 の表）。提案は「変える」。crates.io への公開は別の判断で、この整理では `publish = false` のままにする。
4. **tx 単位の API（`low-level`）を残すか。** 提案は「feature の下に残す」。消すと、自前の取得経路を持つ利用者が使えなくなる。

## 7. 進め方（承認後）

小さな単位に分け、それぞれ fmt / clippy / 全テスト / 実ノード E2E を通してから次へ進む。

| 順 | 内容 | 影響 | 状態 |
|---|---|---|---|
| 1 | 層 C の非公開化と feature 分け（`fixture`, `local-index`, `low-level`）、テスト用フックの移動 | 公開範囲のみ。挙動は不変 | **済**。提案との差: (1) 出力ごとの tweak は `low-level` でも渡さず、適合テスト専用の feature `secret-output-tweaks` を別に設けた（既定では `ScanMatch` が tweak を保持しない）。(2) `SCAN_CHUNK_BLOCKS` と `MAX_INDEXED_OUTPUTS_PER_BLOCK` は観測できる挙動の上限なので公開のまま |
| 2 | 名前の変更（クレート `silent-sync` / `silent-sync-sqlite`、型 `Network` / `SilentPaymentAddress`） | 機械的な置換 | **済**。ディレクトリも `crates/silent-sync`・`crates/silent-sync-sqlite` に変更。ストアに記録する識別子 `SCANNER_VERSION` の接頭辞 `sync-core/` は、既存のストアを無効にしないため変えていない |
| 3 | 層 A の再エクスポート、`#[non_exhaustive]`、ドキュメントと使用例 | 追加のみ | 未着手 |
| 4 | 公開 API 一覧の固定とテスト | 追加のみ | 未着手 |
| 5 | 敵対的レビュー（別エージェント）、無効化チェック、PROGRESS / DECISIONS の更新 | — | 未着手 |

鍵の受け渡しの設計（鍵をどこから読むか）と、ヘッダ鎖の永続化などの未実装項目（O-006、O-013、O-016、O-017）は、この整理の後に別の提案として扱う。
