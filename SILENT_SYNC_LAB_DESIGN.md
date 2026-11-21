# Silent Sync Lab — Claude Code 実装設計書

> Version: 0.1.0 / 作成日: 2026-11-21  
> ステータス: **実験用の設計案（未実装）**  
> 対象: Claude Code / Rust / Bitcoin BIP352 (Silent Payments)  
> 最重要方針: **ウォレットかSDKかを今は決めない。SDKとして切り出せる同期コアを先に作り、薄いCLI実験ウォレットを上に載せる。**

---

## 0. Claude Code に最初に読ませる指示（必読）

あなたはBitcoin BIP352、Rust、Bitcoin Core、テスト自動化を扱うシニアエンジニアです。この文書を唯一のプロジェクト要求仕様として、**実験用のSilent Payments同期基盤**を段階的に開発してください。

**最初の応答ではコードを書き始める前に**次を実施してください。

1. 本仕様を読み、スコープと安全上の制約を要約する。
2. 本文末尾の一次資料とGitHubリポジトリを現在の状態で確認する。特に依存ライブラリのAPI、タグ、ライセンス、対応ネットワーク、必須Bitcoin Coreバージョンを**実コード**と照合する。APIを記憶から推測しない。
3. 調査結果を `docs/RESEARCH.md`、決定・未決事項を `docs/DECISIONS.md` に記す。実際に確認できなかった事項は `UNVERIFIED` とする。
4. 環境を点検し、`cargo test` と上流サンプルの再現を先行する。失敗した場合はログと原因を残し、直した上で次へ進む。
5. 最初の小さな縦断実装（既存BIP352実装による**ローカルの送信→受信検出**）とテストを完成させる。
6. **Phase Gate を通過するまで**UIの美化、メインネット対応、送金機能、独自暗号の実装に進まない。

**作業原則**：毎フェーズで「実装 / 自動テスト / 実行ログ / 未解決事項 / 次フェーズ」を報告。テスト未実行なのに「完成」と言わない。破壊的変更、依存更新、セキュリティ上重要な変更はその理由を記録する。秘密鍵、シード、RPC認証情報を出力・コミットしない。現実の資金を扱わない。

---

## 1. プロジェクト概要

### 1.1 仮称

**Silent Sync Lab**（名称は変更可能）

### 1.2 解決したい問題

Silent Payments（BIP352）は静的な受取アドレスとオンチェーンの非リンク性を実現する一方、受取側が自分宛ての支払いを見つけるためにチェーンをスキャンする必要がある。特に軽量ウォレットでは、初回同期・長期間のオフライン後の追いつき・シード復元のコストが問題になる。

このプロジェクトは、単なる別のBitcoinウォレットではなく、**どの同期方法がどの条件で速い・軽い・安全なのかを再現性ある形で測れる実験環境**を作る。

### 1.3 基本戦略（暫定だが本MVPでは固定）

- **中心はRustのライブラリ `sync-core`**：同期データ取り込み・受信判定・状態保存・メトリクスを担当。
- **薄いCLIアプリ `lab-cli`**：テスト用受取アドレス表示、同期実行、履歴と計測結果の確認。
- **交換可能なバックエンド**：まず決定的なfixture、次にBitcoin Core、必要条件を満たすときにBlindBit Oracle v2など。
- **SDKとして外部公開するか、ウォレットを製品化するかはベンチマークの後に決める**。

### 1.4 MVPの「成功」とは

1. BIP352の公式テストベクトルと既存実装で受信判定を検証できる。
2. Regtest上のテスト用Silent Paymentを検出できる（ネットワーク連携が実装・検証可能な場合）。
3. 同じscan keyとブロック範囲に対して、比較対象のバックエンドから**同じ検出結果**が得られる。未対応バックエンドは「未対応」と明記。
4. コールド同期 / 中断再開 / 完全再スキャンを測定し、JSONまたはCSVで再現可能な結果を出せる。
5. データ欠落を故意に混ぜた場合の影響を示し、「検出できる・できない」を誠実に報告する。
6. 同期コアをCLI固有の表示ロジックと分離し、将来のSDK化やGUI化で再利用できる。

---

## 2. スコープの明確化

### MVPに含める

- `regtest` を第一対象、続いて `signet` の受信検出（互換性が確認できた範囲で）。
- **実験用の受信側（receive-only / watch-oriented）**。scan private keyとspend public keyを利用した受信検出。
- BIP352に準拠する鍵・アドレス導出（既存実装を優先）。
- ローカルfixtureスキャン、Bitcoin Core直接スキャン、Oracle系アダプタの比較。
- ブロックハッシュを伴う再開地点の保存、再編成（reorg）時の巻き戻し。
- 計測と故障注入テスト。
- 対応可能な範囲の復元テスト（wallet birthday / ラベルの有無 / 再スキャン）。

### MVPに含めない（明確な非目標）

- **メインネット接続 / 実資金 / 本番運用**。起動時にmainnetを明示的に拒否。
- ユーザーへの送金ボタン、署名機能、一般公開用のフルウォレット。
- シードやscan private keyを外部スキャナーに送る方式。研究上の比較を将来追加するときは**別モードと明示的同意**が必要。
- Lightning、Ark、Nostr連携、モバイルアプリ、クラウドホスティング。
- 完全に信頼不要な残高保証の達成を宣言すること。
- 標準化未完了のインデックスAPIを既成標準として扱うこと。

**用語に注意**：支払いの「検出」は、確認済みの「未使用UTXO残高」や「支出可能性」の証明と同義ではない。MVPでは `detected_received_outputs` を主表示とし、UTXO状態や合計残高の表示は別途検証が済むまで控える。

---

## 3. 技術調査と採用候補

### 3.1 基準仕様

- BIP352（正本）: https://github.com/bitcoin/bips/blob/master/bip-0352.mediawiki
- BIP352 v1.1.1（2026-04-16時点の更新履歴を確認済み）：`K_max = 2323`、2026年追加のエッジケーステスト等に注意。
- 公式テストベクトル: BIP352にリンクされたcanonical JSONを**現行コミットから取得**して固定する。

### 3.2 上流ライブラリ候補

- `bdk-sp`: https://github.com/bitcoindevkit/bdk-sp
  - Rust製、実験的。上流READMEに `cargo run --example send_and_scan` を記載。
  - `silentpayments`, `wallet`, `indexer`, `oracles`, `cli` 等の構成がある。
  - 2026年9月時点にも更新があるが、**現時点の正確なAPIとタグはClaude Code自身が調査する**。
  - 上流はメインネット利用を推奨していない。本プロジェクトも同様。
- `Bitcoin Core`: https://github.com/bitcoin/bitcoin
  - Regtest起動、テストチェーンの生成、フルブロックとprevout情報の取得。
- `BlindBit Oracle`: https://github.com/setavenger/blindbit-oracle
  - 2026年時点のv2では**Bitcoin Core REST / Core v30+ が要件**と説明され、v1とAPI互換性がない。
  - **Regtest/Signet対応の実効性、HTTP/gRPCスキーマ、構築の実用性を必ず確認**。ダメならOracleアダプタを無理に完成扱いせず、fixture adapterで先に比較基盤を完成させる。
- Index server specification (WIP): https://github.com/silent-payments/BIP0352-index-server-specification
  - Tweak server / remote scanner / personal scannerなど異なる信頼モデルがある。完成した標準ではない。
- The Lookout（別方式の研究例）: https://silentpayments.net/lookout
  - 検出漏れ・データ省略・応答の不一致に関する既存研究。**公開コミットメントがあるだけでは、ウォレットがその場で完全性を知れるとは限らない**。

### 3.3 依存の採用ルール

- 上流Rust crateは**実際にビルド可能なタグまたはコミットSHA**に固定。追跡対象の `DEPENDENCIES.md` を作成。
- 利用ライセンスとバージョン互換を確認する。
- 上流が提供するscannerを再利用し、独自楕円曲線演算は書かない。
- undocumentedな内部APIに依存する場合、その限界と将来の置換案を `DECISIONS.md` に残す。

---

## 4. アーキテクチャ

```
                      +-----------------------+
                      |   lab-cli (thin UI)   |
                      | address / sync / stats|
                      +-----------+-----------+
                                  |
                      +-----------v-----------+
                      |  sync-core (Rust lib)  |
                      | orchestrator, state,  |
                      | scanner, verification |
                      +-----+-------------+---+
                            |             |
                  +---------v---+   +-----v---------+
                  | scan adapter|   | local storage |
                  | abstraction |   | SQLite        |
                  +------+------+   +---------------+
                         |
        +----------------+-------------------+
        |                |                   |
 +------v------+  +------v------+    +-------v--------+
 | Fixture     |  | Bitcoin Core|    | Oracle v2      |
 | deterministic| | direct      |    | conditional    |
 +-------------+  +-------------+    +----------------+
                         |
                +--------v---------+
                | Benchmark Runner |
                | CSV / JSON       |
                +------------------+
```

### 4.1 境界設計

**Protocol / crypto**：BIP352の鍵導出、スキャン適格性判定、マッチング。原則、既存の検証済み実装を呼び出す。

**Data source**：ブロック高、ブロックハッシュ、取引ID、適格入力の公開データ、tweak、Taproot outputs等を取得する。**各ソースが同じ情報を提供できるとは限らない**ため、最初に現実の能力差を整理する。

**Engine**：ブロック区間の処理、保存、再開、進捗、エラー分類、再編成対応。

**Storage**：受信検出と同期チェックポイント、runごとのメトリクス、テストネット識別。鍵の平文永続化は禁止。

**Verification**：受信候補を独立したソースと照合し、必要なら実取引/ブロックから再検証する。サーバー由来の証拠の範囲を明記する。

### 4.2 抽象インターフェース案（実コードではない）

```
ScanSource::capabilities() -> SupportedCapabilities
ScanSource::network() -> NetworkIdentity
ScanSource::fetch_range(start_height, end_height) -> ScanBatch
Scanner::match_outputs(scan_key, spend_pubkey, ScanBatch) -> DetectedOutput[]
SyncEngine::sync(from, to, source) -> SyncReport
Verifier::cross_check(detected_outputs, reference_source) -> VerificationReport
```

注意：このインターフェースは**概念設計**。BIP352のscannerに必要な最小データ・エラー処理・ストリーミング方法を調査後に確定する。tweakベースとフルブロック方式を、情報欠落を隠して「完全に同一の型」に押し込めないこと。

### 4.3 データ契約（論理モデル）

- `ChainIdentity`: network, genesis_hash
- `BlockRef`: height, block_hash, previous_block_hash
- `ScanEvidence`: backend_id, evidence_kind, txid, eligible_input_or_tweak, taproot_outputs, source_block_ref
- `DetectedOutput`: network, block_ref, txid, vout, value_sats (確認可能なとき), label_id (該当時), match_provenance
- `SyncCheckpoint`: network, backend_id, wallet_id, last_verified_height, last_verified_block_hash, scanner_version
- `BenchmarkRecord`: run_id, mode, network, height_range, wall_ms, cpu_ms, downloaded_bytes, processed_tx_count, eligible_tx_count, detected_count, errors, version_info, cache_condition

パブリックなJSON/CSVエクスポートに鍵、シード、RPC cookie、IP、デリバティブ秘密情報を含めない。取引履歴も利用者が明示的に選ぶまで自動公開しない。

---

## 5. 安全性・プライバシーの要件

1. **MVPは Regtest / Signet に限定**。`mainnet` 指定はCLI・設定ローダー両方で拒否。
2. `scan private key`はローカルに保持し、HTTP/gRPCの送信payloadに絶対に含めない。`spend private key`はMVPには原則不要（検出に必要なspend public keyのみを使う）。
3. 初期段階は固定の**テスト専用鍵**で再現。ユーザーの既存ウォレットからシードをインポートしない。永続化が必要ならOSのキーストア等で保護した方式を検討する。
4. リモートのtweak serverを使用すると、IPアドレス・接続時刻・要求ブロック範囲等は見える。scan keyを渡さないだけで**完全な匿名性を保証しない**。
5. サーバーが**データを省略**した場合、対応ウォレットが入金を見逃す可能性がある。誤ったデータ・省略の検出能力は分けて評価。
6. 単一サーバーの応答が妥当でも**完全性は証明されない**。複数サーバーの一致も独立した正解証明ではない。
7. Bitcoin Core RPC/RESTやOracleを外部公開しない。デフォルトlocalhostバインド。認証情報は環境変数/シークレット管理、`.env` はGitに含めない。
8. シードや秘密鍵はstdout, logs, telemetry, benchmark, panic報告、README、テスト成果物へ書き込まない。
9. reorg、ネットワーク不一致、API中断、partial-response、重複応答をfail-closed寄りに扱い、静かに同期済みとみなさない。
10. 未監査の実験コードであり、本番資産向けの安全性を一切主張しない。READMEとCLI起動時に表示。

---

## 6. 正確性の重要ポイント（BIP352の落とし穴）

- 受信対象となる取引の条件（Taproot出力あり、共有秘密導出に適格な入力あり、規格上除外される入力の扱い）を現行BIP352に合わせる。
- 入力タイプ別のpubkey抽出、prevout情報、Taproot key/script pathの条件を正しく処理。
- `input_hash` / 集約鍵 / shared secretは**規格通り**。算術・曲線演算を手作りしない。
- 複数の受信出力を処理する `k` の増加条件は**暗号学的マッチ**で判定する。アプリが少額・使用済み等と判断したことを理由にスキャンを早期終了しない。
- 2026年の仕様にある `K_max = 2323` を扱う。古い実装との差異を検証する。
- change label `m=0`は復元時に常に意識する。通常のラベルを扱う場合はラベルバックアップの必要性を表示する。
- `wallet birthday` より前の受信はスキャンしないため、birthdayが誤ると復元漏れにつながる。**開始高さの根拠**を記録。
- シードだけの復元と、ラベル/履歴を含む完全復元は同義ではない。
- 検出されたoutputの使用済み判定や残高計算は別課題。無検証で「利用可能残高」を示さない。

---

## 7. リポジトリ構成（提案）

```
silent-sync-lab/
├── Cargo.toml                   # Rust workspace
├── Cargo.lock                   # 追跡して固定
├── README.md                    # セットアップ、制限事項
├── CLAUDE.md                    # Claude Code の運用規約
├── crates/
│   ├── sync-core/              # 後のSDK候補
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── protocol.rs
│   │       ├── engine.rs
│   │       ├── models.rs
│   │       ├── verify.rs
│   │       └── source/
│   │           ├── mod.rs
│   │           ├── fixture.rs
│   │           ├── bitcoin_core.rs
│   │           └── blindbit_v2.rs
│   ├── lab-storage/            # SQLiteとcheckpoint
│   ├── lab-bench/              # 計測・fault injection
│   └── lab-cli/                # 実験ウォレットCLI
├── fixtures/                    # 公開可能なテストデータのみ
├── tests/                       # e2e / recovery / reorg / omission
├── scripts/                     # Regtest起動と再現コマンド
├── docs/
│   ├── RESEARCH.md
│   ├── DECISIONS.md
│   ├── DEPENDENCIES.md
│   ├── THREAT_MODEL.md
│   ├── BENCHMARK_PROTOCOL.md
│   └── PROGRESS.md
└── .github/workflows/ci.yml
```

これは初期案。複数crateに分けることで余分な複雑性が発生する場合は、小さなWorkspaceから始めてもよいが、**ライブラリとCLIの依存方向（CLI→Core）は守る**。

---

## 8. CLI機能（仮インターフェース）

ここにあるコマンドは**実装目標であって、既に動くコマンドではない**。

```bash
silent-sync doctor
silent-sync lab init --network regtest
silent-sync address show
silent-sync sync --backend fixture --from 1 --to 120
silent-sync sync --backend core --from 1 --to 120
silent-sync sync --backend oracle-v2 --from 1 --to 120
silent-sync status
silent-sync received list
silent-sync bench --backends core,oracle-v2 --from 1 --to 120 --repeat 5
silent-sync export --format json --output benchmark.json
```

- `doctor`: rust toolchain、Bitcoin Core疎通、ネットワーク、Oracle version/capabilitiesを点検し、利用できないbackendを明示。
- `address show`: BIP352のテスト用アドレス。Signet/TestnetでのプレフィックスとRegtestでのサポートを上流で確かめる。
- `sync`: 進行状況、開始/終了高さ、source、エラー、処理済みブロック数を出す。
- `received list`: 検出した出力のみ。残高保証を印象づけるラベルを使わない。
- `bench`: キャッシュ条件、chain高さ、ソフトウェア/サーバーバージョン、runの順序、cold/warmを明示。
- `export`: 実験結果は個人データを除外したものを標準出力またはファイルへ。

---

## 9. フェーズ別の実装手順とGate

### Phase 0 — 上流調査・セットアップ（最優先）

**実施**

- READMEと本仕様を作業ディレクトリへ配置。
- `bdk-sp`のREADME、examples、`Cargo.toml`、関連モジュール、現行のタグとコミットを確認。
- `BlindBit Oracle`の現行v2 API、Core要件、対応ネットワーク、docker/local実行可能性を確認。
- Bitcoin Coreの現行バージョン、Regtest起動とテストデータ生成の手順を確認。
- `docs/RESEARCH.md`、`docs/DEPENDENCIES.md`、`docs/DECISIONS.md`、`docs/PROGRESS.md` を生成。

**Gate 0**：上流の `cargo test` と README のサンプルを実行・結果記録できる。実行不可なら環境原因と代替手順を明記（成功を偽装しない）。

### Phase 1 — 暗号処理を作らない受信検出（fixture）

**実施**

- upstream scannerを呼び出す最小Rustコードを書く。
- BIP352公式テストベクトルとローカルfixtureを用いて正常系と例外ケースを実装。
- `ScanSource`の最低限のI/Fとfixture adapterを作成。
- テスト用scan key / spend pubkeyで、検出したtxidとvoutを出力。

**Gate 1**：`cargo test`が通る。fixtureで検出件数が期待値と一致し、異なる鍵では誤検出しない。未完成のAPIはモックで誤魔化さず実装範囲を明示。

### Phase 2 — Bitcoin Core / Regtest 統合

**実施**

- ローカルBitcoin CoreでRegtestを起動するscriptsを用意。
- Regtest送金用fixture generatorを作る（上流の送金実装を**テスト生成専用**に使い、CLIにユーザー送金機能は追加しない）。
- BIP352対象取引を作成してブロードキャストし、ブロックを生成する。
- `bitcoin_core` adapterでprevout等の必要情報を取得し、受信検出まで通す。
- 同じtxを二重登録しない。

**Gate 2**：Regtestで「送信→採掘→受信検出」の一連のテストを再現できる。期待txidとvoutを断言する自動テストがある。

### Phase 3 — 永続化・再開・復元

**実施**

- SQLiteでblock hashつきcheckpoint、検出結果、metadataを保存。
- 中断再開、重複処理、ネットワーク不一致、reorg時のrollback/再スキャン。
- wallet birthdayを指定して初回同期と完全再スキャンを比較。
- ラベルなし / change label / ラベル利用時の期待挙動を試験（使用する上流APIの能力に合わせ段階導入）。

**Gate 3**：途中で強制終了→再開しても検出漏れと重複なし。reorgで取り消されたブロック内の検出を正しく修正できる。

### Phase 4 — 第二バックエンドの追加

**実施**

- Oracle v2の利用条件と対応ネットワークが整えば `blindbit_v2` adapterを実装。
- **整わない場合**はローカルfixture-based `indexed_data` adapterを先に実装し、外部Oracle連携を `BLOCKED` と記録。架空の成功レポートを出さない。
- 同じchain range / wallet key / detection policyでCore直接方式と比較。
- 取引ごとの証拠、プライバシー露出、レスポンス完全性を区別して報告。

**Gate 4**：独立した2方式の結果一致、または不一致の理由を個別tx単位で説明できる。外部Oracle未利用なら「実サーバー比較未完了」と明示。

### Phase 5 — ベンチマーク・故障注入

**実施**

- cold sync、warm/resume、復元時の同期を計測。
- 同じrangeで5回以上測定し、中央値とばらつきを出す。
- メトリクス：wall time、CPU、download bytes、server-side preprocess time（取得可能な場合）、candidate count、eligible tx数、検出件数、エラー率。
- サーバー応答からtweak 1件を削除、データ順序変更、重複、timeout、chain tip不一致、reorgを注入。
- checksum・checkpoint・cross-source比較で「検出できる不正」と「検出できない不正」を分けて表現。
- benchmark結果を人が読めるMarkdownレポートとCSV/JSONで出す。

**Gate 5**：再現コマンド、構成、バージョン、全試行結果、限界が揃っている。

### Phase 6 — 進路決定（ここで初めて製品形態を判断）

以下の3択から**測定結果と実装体験にもとづき**選ぶ。

1. **SDK化**：同期処理を多くのウォレットが再利用できる。モジュール境界/APIの安定性とドキュメントを重視。
2. **実験ウォレットの進化**：同期状況、検出漏れリスク、復元UXの見せ方に独自価値がある。Tauri等のUI検討。
3. **研究/インフラ特化**：サーバー信頼性、完全性検査、インデックス形式の最適化が最も有望。基盤研究を深掘る。

このGate以前に「製品はウォレットだ」「SDKだ」と固定しない。

---

## 10. ベンチマーク設計

### 10.1 同条件の定義

- 同じマシン、同じchain (genesis hashまで一致)、同じ開始/終了block hash、同じ鍵、同じ受信ポリシー。
- Core直接方式はblock/prevoutデータ取得時間を含むが、**インデックス方式はサーバーが行った前処理時間・ストレージ費用を別計上**。
- ウォレット側の受信判定処理時間と、通信待ち/デコード時間は可能なら分離。
- cache cold/warm、初回同期/再開、ローカル/リモートの条件を明記。
- オフラインfixtureのみの測定はインターネットでの性能を示さないと明記。

### 10.2 ケース

| ID | ケース | 期待する確認 |
|---|---|---|
| T01 | BIP352公式vectors | 暗号的検出の一致 |
| T02 | 1件の受信 | txid/vout一致 |
| T03 | 複数送金、複数入力 | すべて検出 |
| T04 | 同一tx内の複数該当出力 | `k`を正しく進める |
| T05 | ラベル・change label | 必要なラベルで復元できる |
| T06 | 10/100/1000ブロック | 負荷に応じた時間と通信量 |
| T07 | 50%で同期中断→再開 | 二重計上・漏れなし |
| T08 | DB削除→birthdayから復元 | 完全再スキャンと比較 |
| T09 | 再編成を発生させる | checkpointと検出内容の更新 |
| T10 | tweak 1件を削除 | 欠落時の影響を観察、検出可否を分離 |
| T11 | 不正データ・重複・順序変更 | エラー扱い/耐性 |
| T12 | ノード・Oracle停止 | 偽の「同期完了」にしない |

### 10.3 JSON結果例（数値はダミー）

```json
{
  "run_id": "example-only",
  "network": "regtest",
  "backend": "bitcoin_core",
  "from_height": 1,
  "to_height": 120,
  "cache_condition": "cold",
  "wall_ms": null,
  "downloaded_bytes": null,
  "detected_count": null,
  "verification_status": "not_run",
  "notes": "Schema example only: no measurement has been performed"
}
```

---

## 11. 開発品質とCI

- `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` を最低限のゲートにする。
- APIが変わりやすい依存を含むため、CIでrust toolchainとupstream commitを固定。
- Unit + integrationテストは速いものを常時実行。Bitcoin Coreが必要なE2Eは専用jobとして分離。
- スキャンの例外ケース（不正値・奇妙な入力・複数出力）はproperty-based / fuzzも検討するが、初期MVPでは必須ではない。
- 機密情報検出のpre-commit/CIと `.gitignore` を用意する。
- Rustの警告を強引な `unwrap()` で回避しない。エラーはログに分類し、ユーザーへは意味を示す。
- READMEに明記：`EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS`。

---

## 12. 人間が確認するポイント（AI実装のレビュー境界）

| チェック | 承認条件 |
|---|---|
| ネットワーク安全性 | mainnet拒否がコード・テストで証明 |
| 暗号実装 | upstream再利用、BIP352公式vector通過 |
| 鍵の露出 | 外部通信とログにscan private key/seedが含まれない |
| ブロックデータ | chain hashとブロックハッシュを検証 |
| 再開処理 | 中断/再編成でも検出漏れがない |
| 検出結果 | バックエンド間で一致、または差異を説明可能 |
| ベンチマーク | 手順と条件が再現可能で、計測値を捏造していない |
| リリース表記 | 実資金非対応、監査なしを明記 |

Claude Codeに判断をすべて委ねず、特に「秘密情報の外部送信」「暗号処理の変更」「資金利用を可能にする変更」は人間レビュー必須とする。

---

## 13. 初回作業の具体的なタスクリスト

Claude Code はまず**以下だけ**に取り組み、それができたら報告する。

- [ ] 新規ディレクトリ `silent-sync-lab` とGitリポジトリを作る（既存repoがある場合は破壊しない）
- [ ] `CLAUDE.md` に最重要ルール、MVPスコープ、実装フェーズを簡潔に転記
- [ ] `docs/RESEARCH.md` に上流実装の確認結果（commit SHA / 確認したコードパス）
- [ ] `docs/DECISIONS.md` に設計上の選択と未決事項
- [ ] `docs/DEPENDENCIES.md` に依存バージョン・ライセンス
- [ ] Rust workspace最小構成、`sync-core` と `lab-cli`
- [ ] `bdk-sp` の公式サンプルと単体テストを**実行**
- [ ] upstream scannerを利用してfixtureを1つ検出する最小テスト
- [ ] `cargo fmt`, `cargo clippy`, `cargo test`の結果を保存
- [ ] `docs/PROGRESS.md` にDone / Not done / Blocked と次の作業を記録

**この段階ではTauriを入れない。独自暗号実装をしない。メインネットに接続しない。**

---

## 14. 方向性を決めるための評価基準

| 判断材料 | SDKへ寄せる | ウォレットへ寄せる | インフラ研究へ寄せる |
|---|---|---|---|
| 再利用の需要 | 複数の開発者がSDKを使いたい | 一般利用者からUXへの需要 | サーバー側に未解決の課題 |
| 技術成果 | 小さく安定した公開API | 同期画面・復元体験が独自 | 欠落検出・速度で独自改善 |
| メンテ負担 | API互換性・ドキュメント | UI・鍵管理・サポート | ノード・サーバー運用 |
| 最大の差別化 | 埋め込みやすさ | 見える化と使いやすさ | 信頼性と性能の検証 |

**意思決定タイミング**：Phase 4〜5を通過して検証可能なデータが出たとき。そこまでは実験基盤を優先する。

---

## 15. Claude Code が毎フェーズ報告するテンプレート

```
## Phase X Result
- 実装したファイル:
- 実行したコマンド:
- テスト結果（成功 / 失敗 / 未実行）:
- 出力証拠（ログや成果物のパス）:
- 調べた上流資料・commit SHA:
- 安全性で気をつけたこと:
- 未完成 / ブロッカー:
- 次に取り組む最小タスク:
```

推測で通ったことにせず、必要なバイナリがない、ネットワークに接続できない等の制約があればそのまま報告する。

---

## 16. 参考リンク（2026-11-21確認、実装時再確認）

1. BIP352正本 — https://github.com/bitcoin/bips/blob/master/bip-0352.mediawiki
2. BIP352概説とバージョン履歴 — https://bitcoin.org/bip/352/
3. bdk-sp — https://github.com/bitcoindevkit/bdk-sp
4. BlindBit Oracle v2 — https://github.com/setavenger/blindbit-oracle
5. Silent Payments index server spec (WIP) — https://github.com/silent-payments/BIP0352-index-server-specification
6. The Lookout — https://silentpayments.net/lookout
7. Bitcoin Core Regtest — https://developer.bitcoin.org/examples/testing.html

※ リンクは調査の出発点。上流に破壊的変更があり得るため、機能・関数・バージョンを読まずに決め打ちしない。

---

## 17. これを読み込ませた後、Claude Code に送る短い開始メッセージ

```
このリポジトリの SILENT_SYNC_LAB_DESIGN.md を最初から最後まで読んでください。
「Phase 0 → Phase 1」の範囲だけを進めてください。

目的は、ウォレットかSDKかをまだ決めず、SDKに切り出せるRustのBIP352同期コアと、薄い実験CLIの土台を作ることです。

最初に上流 bdk-sp の現在のAPIとサンプルを実際に確認し、使えるコードを再利用してください。独自暗号実装、メインネット対応、UI作り込みには着手しないでください。

まず research / decisions / dependencies / progress を記録し、公式テストベクトルを使った最小受信検出テストが通るところまで実装してください。コード・実行コマンド・テスト結果・未解決事項を報告してください。
```

---

**最終原則：最初に作るのは「誰かの資産を預かるウォレット」ではなく「Silent Paymentsの同期がどう動くかを安全に検証し、他のプロジェクトに再利用できるエンジン」。**
