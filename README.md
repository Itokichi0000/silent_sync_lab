# Silent Sync Lab

> **EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS**
>
> 未監査の実験用コードです。mainnet・実資金・本番運用には使えません（mainnet の指定はコードで拒否されます）。
> 安全性について一切の保証をしません。

Bitcoin [BIP352 (Silent Payments)](https://github.com/bitcoin/bips/blob/master/bip-0352.mediawiki) の**受信側の同期**を、
再現可能な形で検証・計測するための実験環境と、その上に作った**実験用のウォレット**（regtest / signet 専用）です。製品ではありません。

- 同期の部分（`silent-sync`、`silent-sync-sqlite`、CLI `silent-sync`）は受信専用で、送金・署名の機能を持ちません。
- ウォレットの部分（`silent-wallet`、CLI `silent-wallet`）は、無価値のコインで「受け取る → 未使用を確認する → 送る」を試すためのものです。mainnet は指定できません。

- 要求仕様: [`SILENT_SYNC_LAB_DESIGN.md`](SILENT_SYNC_LAB_DESIGN.md)
- 現在地: [`docs/PROGRESS.md`](docs/PROGRESS.md)（Phase 0〜5、SDK 化、ウォレット段階 W1〜W3 を実施。signet での確認と UI は未了）
- ウォレットの範囲と設計: [`docs/WALLET_DESIGN_PROPOSAL.md`](docs/WALLET_DESIGN_PROPOSAL.md)、脅威モデルは [`docs/THREAT_MODEL.md`](docs/THREAT_MODEL.md) §4.6

## 出来ること / 出来ないこと

| 出来る | 出来ない |
|---|---|
| BIP352 公式テストベクトル（v1.1.2、29 件）による受信検出の検証 | mainnet、実資金（指定そのものができません） |
| 4 種のデータ源からの検出: オフライン fixture、自分の Bitcoin Core（REST）、BlindBit Oracle v2（HTTP）、手元で計算した tweak インデックス | リモート（ループバック以外）のサーバへの接続 |
| 永続同期: チェックポイント、中断からの再開、reorg の巻き戻しと報告（深さの上限つき）、birthday からの復元 | 未承認の tx（mempool）の追跡。外から届いた入金は、ブロックに入るまで見えません |
| フルブロックで検出した出力が使われたことの記録（reorg で取り消し） | インデックス由来の検出についての使用済みの判定（入力が見えないため「不明」） |
| 2 つのデータ源の突き合わせ（`verify`）と、差分の tx 単位の説明 | 監査結果による証拠種別の格上げ |
| 保存済みの検出結果と使用済みの記録を、参照ソース（自分のノード）で照合する監査（`audit`） | ラベルの削除、再スキャンなしでのラベル追加 |
| ラベルの追加（`lab add-label`。範囲全体を別ファイルへスキャンし直し、何も失わないことを確かめてから差し替える） | 鍵・シードの保管（毎回、環境変数で渡します） |
| **ウォレット（regtest / signet）**: アドレスの表示、未使用の集計、送金 tx の組み立て・署名・自分のノードへの送信 | 手数料の引き上げ（RBF / CPFP）、複数の宛先への同時送金（CLI）、ハードウェアウォレット、マルチシグ |
| ベンチマーク（JSON / CSV / Markdown）と故障注入 | signet での動作確認（この環境から接続できず未確認）、GUI |

「検出した出力」は、それだけでは「未使用残高」でも「着金の確認」でもありません。ウォレットが「未使用」と呼ぶのは、**フルブロックで検出し、使用済みの記録が無い出力**だけです。
特に **tweak インデックス由来の検出は、サーバの申告をそのまま照合した結果**で、サーバは入金を省略することも、存在しない入金を見せることもできます
（[`docs/FAULT_INJECTION.md`](docs/FAULT_INJECTION.md)）。CLI は証拠の種類を必ず表示します。

## 構成

```
crates/silent-sync/     同期コア（ライブラリ）。BIP352 の暗号処理は上流 bdk_sp に委譲。CLI・DB に依存しない
crates/silent-sync-sqlite/   同期状態の SQLite 実装
crates/lab-cli/       薄い CLI（バイナリ名: silent-sync）。受信専用
crates/silent-wallet/ ウォレットのライブラリ: 未使用の集計、送金 tx の組み立てと署名（暗号は上流の関数を呼ぶ）
crates/wallet-cli/    ウォレットの CLI（バイナリ名: silent-wallet）
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

保存済みの結果の監査（ストアが持つ範囲を参照ソースでスキャンし直して比べる。ストアは変更しません）:

```bash
cargo run -p lab-cli -- audit --db /tmp/lab.sqlite3 --backend fixture --fixture fixtures/local/regtest_basic_v1.json
```

ラベルの追加（スキャン済みの範囲を、新しいラベルを含めてスキャンし直します。中断しても元のストアは変わらず、再実行で続きから進みます）:

```bash
cargo run -p lab-cli -- lab add-label --db /tmp/lab.sqlite3 --backend fixture --fixture fixtures/local/regtest_basic_v1.json --label 5
```

- `--network` は `regtest`（既定）か `signet`。`mainnet` はエラーになります。
- `--backend` は `fixture` / `core` / `oracle-v2` / `local-index`。`core` と `oracle-v2` は、この機械で動いている自分のノード・Oracle にループバック（`http://127.0.0.1:<port>`）で接続します。
- `lab add-label` と `lab rescan`（同じラベルのままスキャンし直す。別バージョンのスキャナが書いたストア用）は、途中経過をストアの隣の `<store>.rescan` に置きます。前回の残りを捨ててやり直すには `--restart` を付けます。
- ストアは自分だけが書けるディレクトリに置いてください（上の例の `/tmp` は手元で試すためのものです）。
- `scan` はステートレス（何も保存しない）、`sync` はストアに保存して再開できます。チェックポイントより短い競合チェーンには追随しません。`sync --max-reorg-depth <n|none>` で、巻き戻しを許す深さ（既定 100 ブロック）を変えられます。
- 終了コード: 0 成功、1 エラー、2 引数エラー、4 `verify` / `audit` で差分あり。
- `--json` で機械可読な出力になります。

### ウォレット（regtest / signet 専用）

自分の Bitcoin Core ノード（`-rest=1 -server=1`、ループバック）が必要です。下は regtest の例です。

シードは実行のたびに環境変数 `SILENT_WALLET_SEED`（16〜64 バイトを 16 進数で）から読みます。**保存も表示もしません。このプログラムはシードを作りません。** 試すときは、たとえば次のように自分で用意します（この値に価値のあるものを結び付けないでください）。

```bash
export SILENT_WALLET_SEED="$(openssl rand -hex 32)"
```

受け取り用のアドレスを表示し、ストアを作って同期します。

```bash
cargo run -p wallet-cli -- address
```

```bash
cargo run -p wallet-cli -- init --db ~/silent-wallet.sqlite3 --core-rest-url http://127.0.0.1:18443
```

```bash
cargo run -p wallet-cli -- sync --db ~/silent-wallet.sqlite3 --core-rest-url http://127.0.0.1:18443
```

```bash
cargo run -p wallet-cli -- status --db ~/silent-wallet.sqlite3
```

送金します。内容が表示され、標準入力から `SEND` を受け取ったときだけノードに渡します（確認を省くフラグはありません。パイプで `SEND` を流し込めば自動化はできますが、それは利用者が明示的に行うことです）。

```bash
cargo run -p wallet-cli -- send --db ~/silent-wallet.sqlite3 --core-rest-url http://127.0.0.1:18443 --to <相手のアドレス> --amount 50000 --fee-rate 2 --rpc-cookie-file ~/.bitcoin/regtest/.cookie
```

- `status` の「unspent」は、**自分のノードのフルブロックで検出し、使用済みの記録が無い出力**だけの合計です。ブロックに入っていない tx は見えません。
- `--no-broadcast` を付けると、署名した tx を表示するだけで送りません。表示された tx は有効なので、その文字列を持つ人は誰でも送信できます。
- 送信の結果が分からなかった場合（応答が失われた等）は「送られたかどうか不明」と表示し、その tx を送信中として記録したままにします。**分かるまで同じ支払いを繰り返さないでください。**
- 送ったあとブロックに入るまでの tx は `pending` で確認できます。
- シードを失うと、そのウォレットの出力は使えなくなります。保管の仕組みはありません（実験用です）。
- signet での動作は未確認です（`docs/PROGRESS.md`）。

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
- 受信専用の CLI（`silent-sync`）に、自分のシードや鍵を取り込む機能はありません。ウォレットの CLI（`silent-wallet`）は、環境変数で渡されたシードをその実行の間だけ使います。

## 上流実装との関係

暗号処理はすべて [`bdk-sp`](https://github.com/bitcoindevkit/bdk-sp)（commit `bb373e5`）を呼び出します。
調査の過程で BIP352 正本との差異を確認しており、`silent-sync` 側で BIP 準拠になるよう補っています
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

次のいずれかを、利用する側が選べます（Rust のエコシステムで一般的なデュアルライセンス）。

- MIT License（[`LICENSE-MIT`](LICENSE-MIT)）
- Apache License, Version 2.0（[`LICENSE-APACHE`](LICENSE-APACHE)）

どちらも無保証です。加えて、これは未監査の実験コードであり、mainnet・実資金での利用を想定していません。

`fixtures/bip352/` の公式テストベクトルは BIP リポジトリのもの（BSD-2-Clause）で、出典と条件は [`fixtures/bip352/SOURCE.md`](fixtures/bip352/SOURCE.md) にあります。

特に明示しない限り、このリポジトリへのコントリビューションは、上記のデュアルライセンスで提供されたものとして扱います（Apache-2.0 の定義による）。追加の条件は付きません。
