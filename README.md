# Silent Sync Lab

> **EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS**
>
> 未監査の実験用コードです。mainnet・実資金・本番運用には使えません（mainnet の指定はコードで拒否されます）。
> 安全性について一切の保証をしません。

Bitcoin [BIP352 (Silent Payments)](https://github.com/bitcoin/bips/blob/master/bip-0352.mediawiki) の**受信側の同期**を、
再現可能な形で検証・計測するための実験環境です。ウォレット製品ではありません。送金・署名・残高表示の機能はありません。

- 要求仕様: [`SILENT_SYNC_LAB_DESIGN.md`](SILENT_SYNC_LAB_DESIGN.md)
- 現在地: [`docs/PROGRESS.md`](docs/PROGRESS.md)（Phase 0〜5 を実施。Phase 6 の進路は未決定で、判断材料を [`docs/PHASE6_DECISION_MATERIALS.md`](docs/PHASE6_DECISION_MATERIALS.md) にまとめてあります）

## 出来ること / 出来ないこと

| 出来る | 出来ない |
|---|---|
| BIP352 公式テストベクトル（v1.1.2、29 件）による受信検出の検証 | 送金・署名・残高表示（**対象外**） |
| 4 種のデータ源からの検出: オフライン fixture、自分の Bitcoin Core（REST）、BlindBit Oracle v2（HTTP）、手元で計算した tweak インデックス | mainnet、実資金 |
| 永続同期: チェックポイント、中断からの再開、reorg の巻き戻しと報告、birthday からの復元 | 自分のシード・鍵の取り込み（公開テスト鍵のみ） |
| 2 つのデータ源の突き合わせ（`verify`）と、差分の tx 単位の説明 | リモート（ループバック以外）のサーバへの接続 |
| ベンチマーク（JSON / CSV / Markdown）と故障注入 | 使用済みかどうかの追跡 |

「検出した出力」は「未使用残高」でも「着金の確認」でもありません。
特に **tweak インデックス由来の検出は、サーバの申告をそのまま照合した結果**で、サーバは入金を省略することも、存在しない入金を見せることもできます
（[`docs/FAULT_INJECTION.md`](docs/FAULT_INJECTION.md)）。CLI は証拠の種類を必ず表示します。

## 構成

```
crates/sync-core/     同期コア（ライブラリ）。BIP352 の暗号処理は上流 bdk_sp に委譲。CLI・DB に依存しない
crates/lab-storage/   同期状態の SQLite 実装
crates/lab-cli/       薄い CLI（バイナリ名: silent-sync）
crates/lab-testkit/   テスト専用: fixture 生成、regtest ノード操作、署名付きテスト tx、偽サーバ、故障注入
fixtures/             公式テストベクトルと決定的なローカル fixture
scripts/              テスト用バイナリの取得、無効化チェック
docs/                 調査・決定・依存・進捗・脅威モデル・計測結果・実行ログ
```

## セットアップ

Rust 1.97.0 が必要です（`rust-toolchain.toml` で固定。rustup が自動で取得します）。

```bash
cargo test --workspace --locked
```

K_max 関連の 2 テストがあるため、全体で約 2 分かかります。実ノードを使うテストは既定では実行されません（下記）。

## 使い方

オフラインの fixture で一通り試せます。

```bash
cargo run -p lab-cli -- doctor
```

```bash
cargo run -p lab-cli -- address show
```

```bash
cargo run -p lab-cli -- scan --backend fixture --fixture fixtures/local/regtest_basic_v1.json --label 1
```

永続同期（ストアを作り、同期し、状態と検出結果を見る）:

```bash
cargo run -p lab-cli -- lab init --db /tmp/lab.sqlite3 --backend fixture --fixture fixtures/local/regtest_basic_v1.json --birthday 1 --birthday-basis "fixture chain starts at height 1" --label 1
```

```bash
cargo run -p lab-cli -- sync --db /tmp/lab.sqlite3 --backend fixture --fixture fixtures/local/regtest_basic_v1.json
```

```bash
cargo run -p lab-cli -- status --db /tmp/lab.sqlite3
```

```bash
cargo run -p lab-cli -- received list --db /tmp/lab.sqlite3
```

2 つの方式の突き合わせ（フルブロックと、同じデータから計算した tweak インデックス）:

```bash
cargo run -p lab-cli -- verify --backend local-index --fixture fixtures/local/regtest_basic_v1.json --reference fixture --label 1
```

- `--network` は `regtest`（既定）か `signet`。`mainnet` はエラーになります。
- `--backend` は `fixture` / `core` / `oracle-v2` / `local-index`。`core` と `oracle-v2` は、この機械で動いている自分のノード・Oracle にループバック（`http://127.0.0.1:<port>`）で接続します。
- `scan` はステートレス（何も保存しない）、`sync` はストアに保存して再開できます。
- 終了コード: 0 成功、1 エラー、2 引数エラー、4 `verify` で差分あり。
- `--json` で機械可読な出力になります。

### 実ノードを使うテストとベンチマーク

```bash
export BITCOIND_EXE="$(VERIFY_SIGNATURES=1 scripts/fetch_bitcoind.sh /path/outside/repo/core)"
```

```bash
export BLINDBIT_ORACLE_EXE="$(scripts/build_blindbit_oracle.sh /path/outside/repo/oracle)"
```

```bash
cargo test --workspace --locked -- --ignored
```

ベンチマークの手順は [`docs/BENCHMARK_PROTOCOL.md`](docs/BENCHMARK_PROTOCOL.md)、結果は [`docs/BENCHMARK_REPORT.md`](docs/BENCHMARK_REPORT.md)。

## 鍵について

- CLI が使う受取人は、BIP352 公式テストベクトルに掲載されている**公開済みのテスト用鍵**です（`fixtures/bip352/SOURCE.md`）。このリポジトリ独自の秘密鍵はありません。
- この鍵から導出したアドレスに、価値のあるものを送らないでください。
- 自分のウォレットのシードや鍵を取り込む機能はありません。

## 上流実装との関係

暗号処理はすべて [`bdk-sp`](https://github.com/bitcoindevkit/bdk-sp)（commit `bb373e5`）を呼び出します。
調査の過程で BIP352 正本との差異を確認しており、`sync-core` 側で BIP 準拠になるよう補っています
（[`docs/RESEARCH.md`](docs/RESEARCH.md) §3.3、[`docs/DECISIONS.md`](docs/DECISIONS.md) D-005〜D-008、D-030）。これらは人間によるレビューの対象です。

## ドキュメント

| ファイル | 内容 |
|---|---|
| [`docs/PROGRESS.md`](docs/PROGRESS.md) | フェーズごとの報告、Done / Not done / Blocked |
| [`docs/RESEARCH.md`](docs/RESEARCH.md) | 上流の調査結果（commit SHA、確認したコードパス、未確認事項） |
| [`docs/DECISIONS.md`](docs/DECISIONS.md) | 設計上の決定、レビューへの対応、未決事項 |
| [`docs/THREAT_MODEL.md`](docs/THREAT_MODEL.md) | 何を守り、何を守れないか |
| [`docs/FAULT_INJECTION.md`](docs/FAULT_INJECTION.md) | 不正なデータを注入した結果（検出できる／できない） |
| [`docs/BENCHMARK_PROTOCOL.md`](docs/BENCHMARK_PROTOCOL.md) / [`BENCHMARK_REPORT.md`](docs/BENCHMARK_REPORT.md) | 計測の手順と結果。生データは [`docs/benchmarks/`](docs/benchmarks/) |
| [`docs/PHASE6_DECISION_MATERIALS.md`](docs/PHASE6_DECISION_MATERIALS.md) | 進路決定のための材料（決定はしていない） |
| [`docs/DEPENDENCIES.md`](docs/DEPENDENCIES.md) | 依存バージョンとライセンス |
| [`docs/logs/`](docs/logs/) | 実行ログ |
| [`CLAUDE.md`](CLAUDE.md) | Claude Code 向けの運用規約 |

## ライセンス

未定です（[`docs/DECISIONS.md`](docs/DECISIONS.md) O-001）。ライセンスが決まるまで、第三者による利用・再配布の許諾はありません。
