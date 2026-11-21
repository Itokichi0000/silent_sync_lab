# RESEARCH — 上流調査結果（Phase 0）

調査日: 2026-11-21。記載内容は**実際にクローンしたコード・実行したコマンド**に基づく。
実コードで確認できなかった事項は `UNVERIFIED` と明記する。

## 1. 確認したリポジトリとコミット

| 対象 | 確認した commit | 日付 | 取得方法 |
|---|---|---|---|
| bitcoin/bips（BIP352 正本・公式ベクトル） | `fdcd8024595bc1e97db73883e716dbb91627c371`（HEAD） | 2026-11-20 | `git clone --depth 1`（sparse: `bip-0352*`） |
| bitcoindevkit/bdk-sp | `bb373e566fa2a9ee037cb15de2cbe6e679aac7b0`（master HEAD） | 2026-10-27 | `git clone`（全履歴） |
| setavenger/blindbit-oracle | `f9ab7fd8bc35467c492324f743ba1b69049a2bb6`（HEAD） | 2026-09-23 | `git clone --depth 200` |
| silent-payments/BIP0352-index-server-specification | `dc705a055e72024e332e232cf038c96afd42d50e`（HEAD） | 2026-05-11 | `git clone --depth 50` |

## 2. BIP352 正本

確認したファイル: `bip-0352.mediawiki`, `bip-0352/reference.py`, `bip-0352/send_and_receive_test_vectors.json`

- **現行バージョンは 1.1.2（2026-10-25）**。設計書 §3.1 の「v1.1.1」より1つ新しい。
  - 1.1.2: ベクトル「Single recipient: taproot key-path spend with annex」を追加
  - 1.1.1 (2026-04-16): 「Input keys intermediate sum is zero but final sum is non-zero」を追加
  - 1.1.0 (2026-03-02): `K_max` 導入
- `K_max = 2323`（mediawiki L307, L348、`reference.py` L33）。受信側の規則は「`k == K_max` ならスキャンを**止める**」。エラーではなく、そこまでの 2323 件は検出結果になる。公式ベクトル #27 は `n_outputs: 2323` を期待する。
- スキャン対象 tx の条件（L191–196）: コンセンサス有効／Taproot 出力が1つ以上／共有秘密導出に使える入力が1つ以上／**SegWit version > 1 の出力を使っていない**。
- アドレスの HRP は `sp`（mainnet）と `tsp`（testnets）のみ（L207）。**regtest 用の HRP は BIP352 に定義がない**。
- 公式ベクトル: 29 件、SHA-256 `23cc7c3b…706c`。`fixtures/bip352/` に無改変で固定（`fixtures/bip352/SOURCE.md`）。
- ライセンス: BSD-2-Clause。

## 3. bdk-sp

確認したパス: `README.md`, `Cargo.toml`, `Cargo.lock`, `silentpayments/{Cargo.toml,CHANGELOG.md}`,
`silentpayments/src/{lib.rs,hashes.rs,receive/mod.rs,receive/scan.rs,receive/error.rs,encoding/mod.rs,send/mod.rs,send/bip352.rs}`,
`silentpayments/examples/send_and_scan.rs`, `silentpayments/tests/functional_tests/bip352_vectors/{receive.rs,send.rs}`,
`silentpayments/tests/functional_tests/serialization.rs`, `oracles/src/tweaks/blindbit.rs`, `indexer/src/lib.rs`（公開 API 一覧のみ）, `justfile`, `.github/workflows/*.yml`

### 3.1 構成・タグ・ライセンス

- ワークスペース: `silentpayments`（crate 名 `bdk_sp` 0.2.0）, `dleq`, `indexer`, `wallet`, `oracles`, `cli/v1`, `cli/v2`, `fuzz`。
- タグ: `v0.1.0`（`79cfaf1`, 2025-08-28）, `bdk_sp-0.2.0`（`3d8aa58`, 2026-07-23）。crates.io の最新も `bdk_sp 0.2.0`。
- **`K_MAX` の実装はタグ `bdk_sp-0.2.0` の後**（PR #75, commit `1598b06` / `d257a22`）。crates.io 版には無い。
- ライセンス: リポジトリの `LICENSE` は MIT、`silentpayments/Cargo.toml` は `MIT OR Apache-2.0`。
- `bdk_sp` の依存は `bitcoin ^0.32.6` と任意の `serde` のみ。MSRV 1.85.0。
- README: "Work in progress. Not recommended for use with bitcoin mainnet."

### 3.2 受信側 API（本プロジェクトが使うもの）

| API | 役割 |
|---|---|
| `receive::scan::Scanner::new(scan_sk, spend_pk, label_lookup)` | スキャナ。ラベルは `BTreeMap<PublicKey, (Scalar, u32)>` |
| `Scanner::scan_tx(&tx, &prevouts) -> Result<Vec<SpOut>, SpReceiveError>` | 1 tx のスキャン（共有秘密の導出 + マッチ） |
| `Scanner::get_spks_from_tweak(&tweak, k) -> Vec<ScriptBuf>` | 公開 tweak データから `k` 番目の候補 script（ラベル分 + 無ラベル） |
| `receive::compute_tweak_data(&tx, &prevouts)` | 公開 tweak データ `input_hash·A`（秘密鍵不要） |
| `receive::extract_pubkey(txin, &spk)` / `tag_txin` | 入力種別判定と公開鍵抽出 |
| `receive::K_MAX` | 2323 |
| `encoding::SilentPaymentCode::{new_v0, get_label, add_label}` | アドレスとラベル |
| `send::{create_silentpayment_partial_secret, create_silentpayment_scriptpubkeys}` | 送信側（**fixture 生成でのみ使用**） |

- HRP: `sp`（Bitcoin）, `tsp`（Testnet/Testnet4/Signet）, **`sprt`（Regtest、bdk-sp 独自）**。
- `SpOut` は出力ごとの tweak（`SecretKey`）を含む。

### 3.3 BIP352 正本との差異（実測）

| # | 差異 | 根拠 | 本プロジェクトの対応 |
|---|---|---|---|
| U-1 | **K_max 超過時にエラーを返し検出 0 件**。BIP は最初の 2323 件を返して停止 | `receive/mod.rs` `scan_txouts`: `matched_tweaks > K_MAX` で `Err(ScanLimitExceeded)`。公式ベクトル #27 を直接かけると `Err`。上流テスト `check_cases` は `Err` を「期待出力が空」とみなす分岐で通過しており、`n_outputs` を検査していない | D-005（BIP 準拠に補正、ユーザー承認済み） |
| U-2 | **「SegWit v>1 を使う tx はスキャンしない」規則が無い** | `silentpayments/src`, `indexer/src` に witness version の判定なし。該当入力を無視してスキャンを続行する | D-006（sync-core で補う、ユーザー承認済み） |
| U-3 | `compute_tweak_data` が `tx.input.iter().zip(prevouts)` で、**prevout が不足しても黙って切り詰める** | `receive/mod.rs` | D-007（件数不一致をエラーにする） |
| U-4 | 「適格入力なし」と「鍵の和が無限遠点」が同じ `Secp256k1Error(InvalidPublicKeySum)` になる。どちらも BIP 上は「スキップ」でありエラーではない | `PublicKey::combine_keys` の挙動 | 事前の適格性判定で区別（`SkipReason`） |
| U-5 | 不正な witness で panic し得る（`control_block[1..33]` のスライス）。コンセンサス有効な tx では起きないが、未検証データでは起きる | `lib.rs` `tag_txin`。テスト `malformed_taproot_witness_is_contained_as_an_error` で再現 | `catch_unwind` でエラー化（D-008） |
| U-6 | 同梱ベクトルが古い（28 件、v1.1.1 相当）。v1.1.2 の annex ベクトルを含まない | `tests/functional_tests/send_and_receive_test_vectors.json`（SHA-256 `f5f9ed4a…`） | 正本の 29 件で検証。annex ベクトルも通過を確認 |

| U-7 | 同じ `k` に 2 つの出力がマッチし得るとき（無ラベルの `P_k` とラベル付きの `P_k + L` が同居）、**どちらを報告するかが出力の並びに依存**する。`reference.py` は tx 内で最初の出力を取る | `find_spout_for_tweak` の `swap_remove` が残り出力の順序を崩す。独立レビューで再現 | 未対応。準拠した送信者はこの形を作らないが、バックエンド間比較で差分として現れ得る（DECISIONS O-011） |
| U-8 | P2WPKH の witness 内公開鍵と prevout の hash160 の一致を確認しない。witness が空の SegWit 入力は「適格でない入力」として無視する | `extract_pubkey` / `tag_txin`。コンセンサス有効な tx では問題にならない | 後者は D-017 でエラー化。前者は未対応（DECISIONS O-010） |

U-1, U-2, U-5 は上流への報告候補（未報告。DECISIONS O-002）。

### 3.5 rust-bitcoin 0.32.7 の注意点（実測）

| 項目 | 挙動 | 対応 |
|---|---|---|
| `Block::check_witness_commitment` | **全入力の witness が空なら、coinbase に commitment があっても `true` を返す。** witness を剥がしたブロックは txid・マークルルート・ブロックハッシュ・PoW が変わらないため、これだけでは検出できない | アダプタで厳密に再計算（D-013）。スキャナ側でも witness 欠落をエラー化（D-017） |
| `Target::from_compact` | オーバーフローする `bits`（例 `0x227fffff`）を拒否せず丸める。Bitcoin Core は拒否する | `bits` の正規性とネットワーク上限を検査（D-013） |
| `Block::bip34_block_height` | 高さ 1〜16（`OP_1`〜`OP_16`）を読めない | 自前の `coinbase_height`（D-016） |

### 3.4 上流テスト・サンプルの再現（Gate 0）

| コマンド | 結果 | ログ |
|---|---|---|
| `cargo test --no-fail-fast --all-features`（1回目） | **失敗**（ビルドスクリプトの TLS エラー） | `docs/logs/phase0_upstream_cargo_test_attempt1_tls_failure.log` |
| 同上（2回目、下記の回避後） | **成功**: 112 + 53 + 2 + 26 + 9 passed、3 ignored | `docs/logs/phase0_upstream_cargo_test.log` |
| `cargo test --no-fail-fast --all-features -- --ignored slow::` | **成功**: 1 passed（K_max ベクトル、124 秒） | `docs/logs/phase0_upstream_cargo_test_slow.log` |
| `cargo run --example send_and_scan` | **成功**: 送信 → スキャン → ラベル 7 で 0.0001 BTC 検出 | `docs/logs/phase0_upstream_example_send_and_scan.log` |

1回目の失敗原因と対処:
- 原因: 依存 `bitcoind 0.36.1` / `electrsd 0.28.0`（`bdk_testenv` の `download` feature）のビルドスクリプトが `minreq` で Bitcoin Core 25.0 と electrs をダウンロードする。この実行環境は TLS を再終端するプロキシ配下にあり、`minreq` がそのプロキシ CA を信頼しないため `invalid peer certificate: UnknownIssuer`。
- 対処（**TLS 検証は無効化していない**）: 同じファイルを `curl`（プロキシ CA を信頼）で取得し、`BITCOIND_TARBALL_FILE` と `ELECTRSD_DOWNLOAD_ENDPOINT`（localhost の一時 HTTP サーバ）で渡した。両ビルドスクリプトは crate 同梱のハッシュ表で SHA-256 を検証するので完全性は保たれる（Bitcoin Core 25.0: `33930d43…0aec`、electrs: `865e26a9…ddf1`、いずれも一致を確認）。
- 通常のネットワーク環境ではこの回避は不要。

## 4. BlindBit Oracle

確認したパス: `README.md`, `blindbit.example.toml`, `go.mod`, `internal/config/{vars.go,config.go,endpoints.go}`, `internal/server/README.md`, `docker-compose-example.yaml`, `LICENSE`

- HEAD は v2（README: "This is version 2.0 - a major rewrite"）。タグは `v0.1.0`, `indexes-stable` のみで、**v2 のリリースタグは無い**。v1 は `v1` ブランチ。
- 要件: Go 1.24.1+、**Bitcoin Core v30+ の REST**（PR #32540 の `/rest/spenttxouts/:blockhash.bin`）、非 pruned ノード。
- 対応チェーン（設定値）: `main`, `testnet`, `signet`, `regtest`（`internal/config/config.go`）。デフォルトは signet。
- API: **HTTP JSON は `GET /info` 以外 deprecated**。データ取得は gRPC `OracleService`（`StreamComputeIndex`, `StreamBlockScanDataShort`, `GetFullBlock`, `GetInfo`）が推奨。
- bdk-sp の `oracles/src/tweaks/blindbit.rs` は HTTP の `block-height`, `tweaks/{height}?dustLimit=`, `info` を使う。**これは Oracle v2 が deprecated としている経路**であり、応答形式が現行 v2 と一致するかは未確認。
- ライセンス: MIT。
- `UNVERIFIED`:
  - gRPC の protobuf スキーマ（別リポジトリ `blindbit-lib` にあり、未取得）
  - regtest での実ビルド・実起動・実応答（未実施。Phase 4 の Gate 条件）
  - `docker-compose-example.yaml` が v2 でそのまま使えるか（v1 時代の `leveldb_data` ボリューム名が残っている）

## 5. Index server specification (WIP)

確認したパス: `README.md`, `diagrams/`

- 表題どおり WIP。Remote Scanner（鍵をサーバに渡す）／Tweak Server（匿名、tweak を配る）／My Scanner（個人用）の 3 スタックを整理した文書で、**確定した API 標準ではない**。本プロジェクトでも標準として扱わない。
- Remote Scanner / My Scanner は scan 鍵をサーバに渡す方式で、設計書 §2 の非目標に該当する。

## 6. Bitcoin Core

- `https://bitcoincore.org/bin/` の一覧で確認: 最新は **32.1**。30.x は 30.2〜30.5、31.x は 31.0〜31.3 が公開されている（30.0 / 30.1 のディレクトリは存在しない）。
- 32.1 linux x86_64 tarball の SHA-256（`SHA256SUMS` より）: `61545c17c3120fb7412f896d248b11993af04ec8817aad3649c55b2afb044963`。**署名（`SHA256SUMS.asc`）の検証は未実施**。
- 上流 `bdk_testenv 0.11.1` が使うのは Bitcoin Core **25.0**。Oracle v2 は **30+** を要求するので、Phase 2（Core 直接）と Phase 4（Oracle）を同一ノードで比較するには 30+ が必要。
- この実行環境には `bitcoind` が入っていない（`docker` と `go` はある）。
- `UNVERIFIED`: Core 30+ の `rest/spenttxouts` や `getblock` verbosity 3 による prevout 取得の実挙動（Phase 2 で確認する）。

## 7. 未確認の参考資料

| 資料 | 状態 |
|---|---|
| https://bitcoin.org/bip/352/ | `UNVERIFIED`（正本の bips リポジトリを直接確認したため未取得） |
| https://silentpayments.net/lookout （The Lookout） | `UNVERIFIED`（未取得。Phase 4/5 の完全性議論の前に確認する） |
| https://developer.bitcoin.org/examples/testing.html | `UNVERIFIED`（Phase 2 で確認する） |

## 8. 実行環境

| 項目 | 値 |
|---|---|
| OS | Linux 6.18（コンテナ、x86_64、4 vCPU、15 GiB） |
| rustc / cargo | 1.97.0 |
| clippy | 0.1.97 |
| ネットワーク | TLS 再終端プロキシ経由（§3.4 参照） |
| bitcoind | なし（上流テスト用に 25.0 を一時取得したのみ） |
