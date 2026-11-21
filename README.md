# Silent Sync Lab

> **EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS**
>
> 未監査の実験用コードです。mainnet・実資金・本番運用には使えません（mainnet の指定はコードで拒否されます）。
> 安全性について一切の保証をしません。

Bitcoin [BIP352 (Silent Payments)](https://github.com/bitcoin/bips/blob/master/bip-0352.mediawiki) の**受信側の同期**を、
再現可能な形で検証・計測するための実験環境です。ウォレット製品ではありません。

- 要求仕様: [`SILENT_SYNC_LAB_DESIGN.md`](SILENT_SYNC_LAB_DESIGN.md)
- 現在地: [`docs/PROGRESS.md`](docs/PROGRESS.md)（**Phase 1 まで**。Bitcoin Core 連携・永続化・Oracle・ベンチマークは未実装）

## いま出来ること / 出来ないこと

| 出来る | 出来ない（未実装） |
|---|---|
| BIP352 公式テストベクトル（v1.1.2、29 件）による受信検出の検証 | Bitcoin Core / regtest ノードからの同期（Phase 2） |
| オフライン fixture チェーンのスキャンと検出結果の出力 | チェックポイント保存・中断再開・reorg 対応（Phase 3） |
| テスト用 Silent Payment アドレスの表示（regtest / signet） | BlindBit Oracle 等の第二バックエンド（Phase 4） |
| mainnet の拒否、欠落・改ざんデータの拒否（fail-closed） | ベンチマーク・故障注入レポート（Phase 5） |
| | 送金・署名・残高表示（**MVP の対象外**） |

「検出した出力」は「未使用残高」ではありません。使用済みかどうかは追跡していません。

## 構成

```
crates/sync-core/   同期コア（ライブラリ）。BIP352 の暗号処理は上流 bdk_sp に委譲
crates/lab-cli/     薄い CLI（バイナリ名: silent-sync）
fixtures/           公式テストベクトルと決定的なローカル fixture
docs/               調査結果・決定事項・依存関係・進捗・実行ログ
```

## セットアップ

Rust 1.97.0 が必要です（`rust-toolchain.toml` で固定。rustup が自動で取得します）。

```bash
cargo test --workspace --locked
```

K_max 関連の 2 テストがあるため、全体で約 2 分かかります。

## 使い方

```bash
cargo run -p lab-cli -- doctor
```

```bash
cargo run -p lab-cli -- address show
```

```bash
cargo run -p lab-cli -- sync --backend fixture --fixture fixtures/local/regtest_basic_v1.json --label 1
```

```bash
cargo run -p lab-cli -- sync --backend fixture --fixture fixtures/local/regtest_basic_v1.json --label 1 --json
```

- `--network` は `regtest`（既定）か `signet`。`mainnet` はエラーになります。
- `sync` は現状ステートレスなスキャンで、何も保存しません。
- `--backend core` / `--backend oracle-v2` は「未実装」と表示して終了コード 3 を返します。

## 鍵について

- CLI が使う受取人は、BIP352 公式テストベクトルに掲載されている**公開済みのテスト用鍵**です（`fixtures/bip352/SOURCE.md`）。このリポジトリ独自の秘密鍵はありません。
- この鍵から導出したアドレスに、価値のあるものを送らないでください。
- 自分のウォレットのシードや鍵を取り込む機能はありません。

## 上流実装との関係

暗号処理はすべて [`bdk-sp`](https://github.com/bitcoindevkit/bdk-sp)（commit `bb373e5`）を呼び出します。
調査の過程で BIP352 正本との差異を確認しており、`sync-core` 側で BIP 準拠になるよう補っています
（[`docs/RESEARCH.md`](docs/RESEARCH.md) §3.3、[`docs/DECISIONS.md`](docs/DECISIONS.md) D-005〜D-008）。

## ドキュメント

| ファイル | 内容 |
|---|---|
| [`docs/RESEARCH.md`](docs/RESEARCH.md) | 上流の調査結果（commit SHA、確認したコードパス、未確認事項） |
| [`docs/DECISIONS.md`](docs/DECISIONS.md) | 設計上の決定と未決事項 |
| [`docs/DEPENDENCIES.md`](docs/DEPENDENCIES.md) | 依存バージョンとライセンス |
| [`docs/PROGRESS.md`](docs/PROGRESS.md) | Done / Not done / Blocked、フェーズ報告 |
| [`docs/logs/`](docs/logs/) | 実行ログ |
| [`CLAUDE.md`](CLAUDE.md) | Claude Code 向けの運用規約 |

## ライセンス

未定です（[`docs/DECISIONS.md`](docs/DECISIONS.md) O-001）。
