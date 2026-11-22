# Silent Sync Lab

> **EXPERIMENTAL — 未監査の実験用コードです**
>
> 既定のビルドでは mainnet を指定できません（コードで拒否されます）。mainnet は、feature `mainnet` を明示的に有効にしたビルドでだけ使えます。
> その場合は**実資金を扱います**が、第三者の監査は受けておらず、安全性について一切の保証をしません。バグがあれば資金を失います。

Bitcoin [BIP352 (Silent Payments)](https://github.com/bitcoin/bips/blob/master/bip-0352.mediawiki) の**受信側の同期**を、
再現可能な形で検証・計測するための実験環境と、その上に作った**実験用のウォレット**と、アプリに組み込むための**窓口 API**（`silent_wallet::client`）です。製品ではありません。

- 同期の部分（`silent-sync`、`silent-sync-sqlite`、CLI `silent-sync`）は受信専用で、送金・署名の機能を持ちません。
- 送金 tx は、チェーンを見る者に余計な手がかりを与えないように組み立てます（入力と出力の順序は乱数、lock time は Bitcoin Core のウォレットと同じ規則、別々のラベルで受け取った出力はなるべく一緒に使わない、お釣り無しで払える組み合わせを優先、使わせたくない出力の保留）。何が残るかは `docs/THREAT_MODEL.md` §4.8。ペイジョインは入っていません（理由は `docs/DECISIONS.md` D-054）。
- ウォレットの部分（`silent-wallet`、CLI `silent-wallet`）は、「受け取る → 未使用を確認する → 送る」を行います。既定では regtest / signet だけで、mainnet は feature `mainnet` を有効にしたビルドでだけ指定できます。
- 受信専用の CLI（`silent-sync`）は公開のテスト鍵で動くので、どのビルドでも mainnet を拒否します。

- 要求仕様: [`SILENT_SYNC_LAB_DESIGN.md`](SILENT_SYNC_LAB_DESIGN.md)
- 現在地: [`docs/PROGRESS.md`](docs/PROGRESS.md)（Phase 0〜5、SDK 化、ウォレット段階 W1〜W3、mainnet の feature、窓口 API を実施。signet と mainnet の実ネットワークでの確認、UI は未了）
- ウォレットの範囲と設計: [`docs/WALLET_DESIGN_PROPOSAL.md`](docs/WALLET_DESIGN_PROPOSAL.md)、脅威モデルは [`docs/THREAT_MODEL.md`](docs/THREAT_MODEL.md) §4.6

## 出来ること / 出来ないこと

| 出来る | 出来ない |
|---|---|
| BIP352 公式テストベクトル（v1.1.2、29 件）による受信検出の検証 | 既定のビルドでの mainnet（指定そのものができません。feature `mainnet` が必要） |
| **窓口 API**（`silent_wallet::client`）: シードフレーズ（BIP39）の作成と復元、接続、受信アドレス、同期、未使用の集計、送金（組み立てと送信の 2 段階） | ライトニング、通常のオンチェーンのアドレスでの受信、手数料の見積もり |
| 4 種のデータ源からの検出: オフライン fixture、自分の Bitcoin Core（REST）、BlindBit Oracle v2（HTTP）、手元で計算した tweak インデックス | リモート（ループバック以外）のサーバへの接続 |
| 永続同期: チェックポイント、中断からの再開、reorg の巻き戻しと報告（深さの上限つき）、birthday からの復元 | 未承認の tx（mempool）の追跡。外から届いた入金は、ブロックに入るまで見えません |
| フルブロックで検出した出力が使われたことの記録（reorg で取り消し） | インデックス由来の検出についての使用済みの判定（入力が見えないため「不明」） |
| 2 つのデータ源の突き合わせ（`verify`）と、差分の tx 単位の説明 | 監査結果による証拠種別の格上げ |
| 保存済みの検出結果と使用済みの記録を、参照ソース（自分のノード）で照合する監査（`audit`） | ラベルの削除、再スキャンなしでのラベル追加 |
| ラベルの追加（`lab add-label`。範囲全体を別ファイルへスキャンし直し、何も失わないことを確かめてから差し替える） | 鍵・シード・シードフレーズの保管（毎回、環境変数や引数で渡します） |
| **ウォレット**（regtest / signet。mainnet は feature `mainnet`）: アドレスの表示、未使用の集計、サイレントペイメントと通常のアドレスへの送金 tx の組み立て・署名・自分のノードへの送信 | 手数料の引き上げ（RBF / CPFP）、複数の宛先への同時送金（CLI）、ハードウェアウォレット、マルチシグ |
| ベンチマーク（JSON / CSV / Markdown）と故障注入 | signet と、ブロックのある mainnet ノードでの動作確認（未実施）、GUI |

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

### アプリに組み込む（窓口 API）

`silent-wallet` の feature `client` を有効にすると、数個の関数でウォレットを組み込めます。データ源は、同じマシンで動く自分の Bitcoin Core（`-rest=1 -server=1`）です。

```toml
silent-wallet = { path = "crates/silent-wallet", features = ["client"] }
```

```rust
use silent_wallet::client::{ClientConfig, SeedPhrase, WalletClient, WordCount};
use silent_sync::Network;

// 初回だけ: シードフレーズを作り、利用者に書き留めてもらう（保存はしません）
let phrase = SeedPhrase::generate(WordCount::Twelve)?;

// 起動のたびに: フレーズを渡して接続する（ストアは初回に作られます）
let config = ClientConfig::new(Network::Signet, "wallet.sqlite3", "http://127.0.0.1:38332")
    .with_rpc_cookie_file("/path/to/signet/.cookie");
let mut wallet = WalletClient::connect(config, &phrase, "")?;

println!("受け取り用アドレス: {}", wallet.receive_address(None)?);
wallet.sync()?;
println!("未使用: {} sat", wallet.holdings()?.unspent_sats());

// 送金は 2 段階: 組み立てて内容を見せ、利用者が決めてから送る
let prepared = wallet.prepare_send("tsp1q…", 10_000, 2)?;
println!("{prepared}");
let txid = wallet.send(prepared)?;
```

- 受信はサイレントペイメントのアドレス、送金はサイレントペイメントと通常のオンチェーンのアドレスの両方に対応します。ライトニングと、通常のアドレスでの受信はありません。
- `send` が `OutcomeUnknown` を返したら、**送られたかどうかは分かりません**。「送られなかった」として扱わないでください（その tx は送信中として記録され、入力は再利用されません）。
- mainnet で使うには `features = ["client", "mainnet"]` が必要です。1 回の送金額には既定の上限（100,000 sat）があります。

### ウォレットの CLI（regtest / signet。mainnet は feature `mainnet`）

自分の Bitcoin Core ノード（`-rest=1 -server=1`、ループバック）が必要です。下は regtest の例です。

ウォレットの秘密は、実行のたびに環境変数から読みます。**保存はしません。** シードフレーズ（BIP39）を使う場合は、まず作って書き留めます（このコマンドだけが、作ったフレーズを 1 回表示します）。

```bash
cargo run -p wallet-cli -- new-phrase
```

表示された語を環境変数 `SILENT_WALLET_PHRASE` に入れます（BIP39 のパスフレーズを使う場合は `SILENT_WALLET_PASSPHRASE` にも）。16 進のシード（16〜64 バイト）を `SILENT_WALLET_SEED` で渡す方法も使えます。設定するのはどちらか一方です。

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
- **mainnet**: `cargo run -p wallet-cli --features mainnet -- --network mainnet …` のように、feature を有効にしたビルドでだけ指定できます。実資金が動きます。確認語は `SEND REAL BITCOIN`、1 回の送金額の既定の上限は 100,000 sat（`--max-amount` で上げられます）。手数料の率は自分で指定し、あとから引き上げる手段はありません。ブロックのある実際の mainnet ノードでの動作は未確認です。

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
