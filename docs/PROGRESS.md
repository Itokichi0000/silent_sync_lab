# PROGRESS

最終更新: 2026-11-21

| Phase | 内容 | 状態 |
|---|---|---|
| 0 | 上流調査・セットアップ | **完了**（Gate 0 通過） |
| 1 | 暗号処理を作らない受信検出（fixture） | **完了**（Gate 1 通過） |
| 2 | Bitcoin Core / regtest 統合 | **完了**（Gate 2 通過） |
| 3 | 永続化・再開・復元 | **完了**（Gate 3 通過） |
| 4 | 第二バックエンド | **完了**（Gate 4 通過） |
| 5 | ベンチマーク・故障注入 | **完了**（Gate 5 通過） |
| 6 | 進路決定 | 判断材料を作成。**決定は所有者待ち**（`docs/PHASE6_DECISION_MATERIALS.md`） |

## Done

- [x] 既存の空リポジトリ直下に Rust ワークスペースを作成（`sync-core`, `lab-cli`）
- [x] `CLAUDE.md` に最重要ルール・スコープ・フェーズを記載
- [x] `docs/RESEARCH.md`（commit SHA・確認したコードパス・`UNVERIFIED` 一覧）
- [x] `docs/DECISIONS.md`（決定 18 件、未決 12 件）
- [x] `docs/DEPENDENCIES.md`（全依存のバージョンとライセンス）
- [x] 上流 `bdk-sp` の単体テスト・slow テスト・README サンプルを実行して記録
- [x] 上流スキャナによる受信検出: BIP352 公式ベクトル 29 件すべて
- [x] `ScanSource` の最小 I/F と fixture アダプタ
- [x] 決定的 fixture チェーンと、期待 txid / vout を断言するテスト
- [x] 異なる鍵で誤検出しないことのテスト
- [x] mainnet 拒否（型・パーサ・serde・genesis hash・CLI）のテスト
- [x] `cargo fmt` / `cargo clippy -D warnings` / `cargo test` の結果を保存
- [x] CI ワークフロー（`.github/workflows/ci.yml`）
- [x] 独立レビューの実施と指摘への対応、検査の無効化チェック（`scripts/mutation_check.py`）
- [x] Bitcoin Core REST バックエンド、実ノードでの「送信 → 採掘 → 検出」（Phase 2）
- [x] 永続化・再開・reorg・復元（Phase 3）: `lab init` / `sync` / `status` / `received list`
- [x] 第二バックエンド（BlindBit Oracle v2）と突き合わせ `verify`（Phase 4）
- [x] ベンチマーク（`bench` / `export`）、故障注入マトリクス、`docs/THREAT_MODEL.md`、`docs/BENCHMARK_PROTOCOL.md`、`docs/BENCHMARK_REPORT.md`、`docs/FAULT_INJECTION.md`（Phase 5）
- [x] 進路決定の判断材料（Phase 6。決定そのものは未了）
- [x] GitHub Actions: 通常テストと、実ノード + 実 Oracle の E2E ジョブが成功していることを確認（直近のコミットまで）

## Not done

- [ ] **進路の決定**（Phase 6）。材料は揃えたが、選ぶのは所有者。
- [ ] ライセンスの決定（DECISIONS O-001）、上流への不具合報告（O-002）
- [ ] 機密情報検出の pre-commit / CI ツール（O-009）。現状は `.gitignore` と、出力・DB・結果ファイルに鍵が現れないことのテスト
- [ ] MSRV 1.85.0 でのテスト実行（`cargo check` までは Phase 1 で確認。CI には未追加）
- [ ] property-based / fuzz テスト（設計書 §11 で初期 MVP では任意）
- [ ] gRPC（O-014）、リモートのサーバとの比較（O-015）、ヘッダ鎖の永続化（O-016）、reorg 深さの上限（O-013）、ラベル追加時の再スキャン（O-006）
- [ ] 無効化チェックの CI での定期実行（ワークフロー `mutation.yml` は用意したが、GitHub 上での実行結果は未確認: UNVERIFIED）

## Blocked

ブロックされている作業はない。

## Unverified（確認できていないこと）

- 実際の signet での動作。インターネット越し・mainnet 規模での性能。
- 非 Linux 環境（ファイル権限、CPU 時間の計測）。
- BlindBit Oracle の他の構成（cut-through、dust フィルタ）、gRPC、ノードの reorg に対する実 Oracle の挙動。
- Oracle が先端の高さを報告するまでの時間が、インデックス完了を正しく表すか（BENCHMARK_REPORT §5.2）。

---

## Phase 0 Result

- **実装したファイル**: `docs/RESEARCH.md`, `docs/DECISIONS.md`, `docs/DEPENDENCIES.md`, `docs/PROGRESS.md`, `CLAUDE.md`, `README.md`, `SILENT_SYNC_LAB_DESIGN.md`（仕様の配置）, `scripts/reproduce_upstream.sh`, `fixtures/bip352/{send_and_receive_test_vectors.json,SOURCE.md}`
- **実行したコマンド**（上流 `bdk-sp` @ `bb373e5` のクローン内）:
  - `cargo test --no-fail-fast --all-features`
  - `cargo test --no-fail-fast --all-features -- --ignored slow::`
  - `cargo run --example send_and_scan`
  - 上記 3 つをまとめた `scripts/reproduce_upstream.sh <work-dir>`（既存クローンに対して実行し、同じ結果を確認。新規クローンからの実行は未実施）
- **テスト結果**:
  - 1 回目: **失敗**。ビルドスクリプトの TLS エラー（実行環境のプロキシ CA を `minreq` が信頼しない）
  - 2 回目（ファイルを別途取得し、ビルドスクリプトの SHA-256 検証を通して供給）: **成功**。112 + 53 + 2 + 26 + 9 passed、3 ignored
  - slow: **成功**。1 passed（124 秒）
  - サンプル: **成功**
- **出力証拠**: `docs/logs/phase0_upstream_cargo_test_attempt1_tls_failure.log`, `phase0_upstream_cargo_test.log`, `phase0_upstream_cargo_test_slow.log`, `phase0_upstream_example_send_and_scan.log`
- **調べた上流資料・commit SHA**: bitcoin/bips `fdcd802`、bdk-sp `bb373e5`、blindbit-oracle `f9ab7fd`、index-server-specification `dc705a0`（詳細は RESEARCH §1）
- **安全性で気をつけたこと**: TLS 検証を無効化せずに回避した。取得したバイナリは crate 同梱のハッシュと一致することを確認した。上流の調査用クローンとバイナリはリポジトリ外（スクラッチ領域）に置き、コミットしていない。
- **未完成 / ブロッカー**: BlindBit Oracle v2 の実動作、gRPC スキーマ、Bitcoin Core 30+ の prevout 取得、The Lookout など 3 資料は `UNVERIFIED`（RESEARCH §4, §6, §7）。
- **Gate 0 判定**: 通過。上流の `cargo test` と README サンプルを実行し結果を記録した。

### Phase 0 で分かった重要事項

1. BIP352 の現行は **v1.1.2**（設計書記載の v1.1.1 より新しい）。公式ベクトルは 29 件。
2. `K_MAX` は bdk-sp のリリース版（0.2.0）には無く、master にのみある。
3. 上流と BIP352 正本の間に差異がある（RESEARCH §3.3 U-1〜U-6）。うち 2 件は検出結果が変わる。

## Phase 1 Result

- **実装したファイル**:
  - `crates/sync-core/src/{lib.rs,network.rs,models.rs,protocol.rs,engine.rs,source/mod.rs,source/fixture.rs}`
  - `crates/sync-core/tests/{bip352_vectors.rs,fixture_scan.rs,support/fixture_gen.rs}`
  - `crates/sync-core/examples/gen_fixture.rs`
  - `crates/lab-cli/src/{main.rs,lab_keys.rs}`, `crates/lab-cli/tests/cli.rs`
  - `fixtures/local/{regtest_basic_v1.json,README.md}`
  - `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.gitignore`, `.github/workflows/ci.yml`
- **実行したコマンド**:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`
  - `cargo test --workspace --locked`
  - `cargo run -p sync-core --example gen_fixture -- fixtures/local/regtest_basic_v1.json`
  - `silent-sync doctor` / `address show` / `sync --backend fixture ...`（手動実行で出力を確認）
- **テスト結果**: **成功**。51 件すべて通過（ignored なし、所要約 2 分。ほぼ全てが K_max の 2 テスト）
  | テスト対象 | 件数 |
  |---|---|
  | `sync-core` 単体（mainnet 拒否、チェーン識別、BIP34 高さ） | 10 |
  | `bip352_vectors`（公式ベクトル、K_max ベクトル、K_max 自作ケース） | 5 |
  | `fixture_scan`（検出、ラベル、他人の鍵、アダプタ検証、エンジン、witness 欠落、秘密の非漏洩） | 26 |
  | `lab-cli`（E2E 9、鍵の読み込み 1） | 10 |
  - fmt: 差分なし。clippy: 警告なし（`-D warnings`）。
  - 検査の無効化チェック: 27 件すべてテストで検出（生存 0）。
  - CI（GitHub Actions）: コミット `a24c146` の実行が成功したことを確認（run 44863625853）。レビュー対応後のコミットの結果は Actions の履歴を参照。
- **出力証拠**: `docs/logs/phase1_cargo_fmt.log`, `phase1_cargo_clippy.log`, `phase1_cargo_test.log`, `phase1_mutation_check.log`
- **調べた上流資料・commit SHA**: Phase 0 と同じ。加えて `rust-bitcoin 0.32.7` の `Witness::taproot_control_block` / `Block::bip34_block_height` / `check_witness_commitment` の実装を確認。
- **安全性で気をつけたこと**:
  - mainnet を型で表現不能にし、文字列・serde・genesis hash・CLI のすべての入口で拒否。テストあり。
  - 独自の暗号処理なし。K_max の補正も上流関数のみで構成（DECISIONS D-005）。
  - scan 鍵・tweak がレポート JSON / `Debug` / CLI 出力に現れないことをテストで検査。
  - リポジトリ独自の秘密鍵を追加していない（公式ベクトルの公開値を使用）。
  - ソースを信用しない: 件数・高さ・ハッシュ連結・マークルルート・witness commitment（厳密）・PoW（`bits` の正規性と上限込み）・BIP34 高さ・coinbase の位置・txid の重複を検査し、不整合は範囲全体の失敗にする。
  - witness を剥がしたデータはアダプタとスキャナの二段で拒否する（DECISIONS D-017）。
  - CLI は spend 秘密鍵を読み込まない。
  - 送金・署名・残高の機能とコマンドは存在しない（テストあり）。
- **未完成 / ブロッカー**: 上の Not done を参照。Phase 1 の範囲内での未完成はない。

### Gate 1 判定: 通過

| 条件 | 根拠 |
|---|---|
| `cargo test` が通る | `docs/logs/phase1_cargo_test.log` |
| fixture で検出件数が期待値と一致 | `detects_exactly_the_outputs_created_for_the_receiver`（ラベル 1 を知るウォレットで 7 件。txid / vout / 金額 / ラベル / 高さを照合）。期待値は**送信側の知識**から作っており、スキャナの出力を写したものではない |
| 異なる鍵では誤検出しない | `other_keys_do_not_detect_the_receivers_outputs`（別の受取人は自分宛の 1 件のみ。鍵を片方だけ入れ替えると 0 件） |
| 未完成の API をモックで誤魔化していない | `core` / `oracle-v2` は「未実装」を表示し終了コード 3。tweak 方式の `Evidence` は未定義のまま（DECISIONS D-012） |

### 設計書のテストケースとの対応

| ID | 状態 | 備考 |
|---|---|---|
| T01 公式 vectors | **済** | 29 件。アドレス・公開 tweak・出力鍵・tweak・署名まで一致 |
| T02 1 件の受信 | **済（fixture）** | 実ノードでは未実施 |
| T03 複数送金・複数入力 | **済（fixture）** | 同上 |
| T04 同一 tx 内の複数出力 | **済（fixture + 公式ベクトル）** | K_max 境界を含む |
| T05 ラベル・change label | **済（fixture + 公式ベクトル）** | 復元時の見え方を 4 パターンで確認 |
| T06〜T12 | 未着手 | Phase 2〜5。T10 / T11 の一部（ブロック欠落・重複・順序変更の拒否）はエンジンの単体レベルで確認済み |

### 独立レビューの結果と対応

文脈を共有しない別エージェントに、BIP352 正本・上流コード・`rust-bitcoin` の実装と突き合わせる敵対的レビューを依頼した。確認済みバグは自分のテストで再現してから修正した。その他の指摘は、該当コードの読解か、検査を無効化するとテストが通ってしまうこと（無効化チェック）で確認してから対応している。「レビューが正しいと確認した項目」はレビュー側の実行結果であり、こちらでは再実行していない。

| 区分 | 指摘 | 対応 |
|---|---|---|
| **確認済みバグ** | witness を剥がしたブロックを fixture アダプタが受理し、支払いを黙って見逃す（検出 7 → 0 件、エラーなし） | **修正**。アダプタで commitment を厳密照合、スキャナで witness 欠落をエラー化（D-013, D-017） |
| 堅牢化 | エンジンが tx 0 件のブロック・coinbase なし・txid 重複を「走査済み」にする | **修正**（D-014） |
| 堅牢化 | PoW 検査が任意の `bits`（オーバーフローする符号化を含む）を受け入れる | **修正**（D-013） |
| 堅牢化 | `ChainIdentity` をデシリアライズすると genesis 検証を迂回できる／上流のアドレス型経由で `sp1…` を出力できる | **修正**（D-009） |
| 堅牢化 | `scan_tx` の戻り値が tweak ごと直列化できる | **修正**（D-011） |
| 堅牢化 | CLI が不要な spend 秘密鍵を読み込んでいる | **修正**（D-010） |
| 堅牢化 | prevout と witness の整合を検査していない | **未対応**（O-010。Phase 2 以降で要否を判断） |
| 堅牢化 | 同じ `k` に 2 出力がマッチすると結果が並びに依存する（上流の挙動） | **未対応**（O-011、RESEARCH U-7） |
| テストの穴 | 9 つの検査が、無効化してもテストで検出されなかった | **修正**。各検査に固有のテストを追加し、27 件の無効化がすべて検出されることを確認 |
| 軽微 | パイプ切断時に CLI が panic（終了コード 101） | **修正**。エラー終了（終了コード 1）。stdout が閉じられている場合は未対応（O-012） |
| 軽微 | 封じ込めた上流 panic のメッセージが stderr に出る（位置情報のみ、鍵は含まない） | 未対応（既知。D-008） |

レビューが「正しい」と確認した項目: K_max 補正が BIP352 と一致すること（自作の 3 ケースを含む）、適格性判定が BIP の条件と一致すること（annex、NUMS、P2A、SegWit v2 / v16 など）、`InvalidPublicKeySum` の分類に他の発生源が無いこと、30,000 件の破損 fixture と 200,000 件のランダム入力で panic が外に出ないこと、鍵が出力に現れないこと、CLI から mainnet を扱えないこと。

### Phase 1 で分かったこと

1. **未知のラベルは、同一 tx 内の後続の出力も隠す。** BIP352 のスキャンは `k = 0` から順に進み、マッチが無い `k` で止まる。fixture の高さ 5 の tx（ラベル 1 が `k=0`、change が `k=1`、ラベル 5 が `k=2`）を「鍵だけ」で復元すると、change 出力まで見えなくなる。ラベル 5 を知っていてもラベル 1 を失っていれば、ラベル 5 の出力も見えない。「ラベルを失うとそのラベル宛の入金だけが見えなくなる」という理解は不正確（DECISIONS O-006）。
2. **エンジン単体では、整合したブロックから tx が 1 件抜かれても気付けない。** `engine_alone_cannot_detect_an_omitted_transaction` に記録。検出できるのはソース側でマークルルートを検証している場合か、別ソースと突き合わせた場合のみ。tweak 方式のバックエンドではこれが本質的な課題になる（Phase 4〜5）。
3. 上流は不正な witness で panic する。未検証データを渡す経路では封じ込めが必要（DECISIONS D-008）。
4. **witness を剥がしたブロックは、ブロックハッシュ・マークルルート・PoW では見分けられない。** `rust-bitcoin` の commitment 検査もそれを通す。Phase 2 で Bitcoin Core からブロックを取るときも、witness 付きで取得できていることを確認する必要がある。
5. 「検査がある」ことと「検査が効いていることをテストが保証する」ことは別だった。最初の実装では 9 つの検査が、外してもテストが通る状態だった。

## Phase 2 Result

- **実装したファイル**:
  - `crates/sync-core/src/source/bitcoin_core.rs`（REST バックエンド、feature `core-rest`）, `source/block_check.rs`（フルブロック共通検査。fixture アダプタから切り出し）, `source/mod.rs`（`SourceStats`, `SourceError::Network`）
  - `crates/lab-testkit/`（新規・テスト専用）: `fixture_gen.rs`（移動）, `regtest_node.rs`, `regtest_wallet.rs`, `fake_core_rest.rs`, `bin/gen_fixture.rs`
  - `crates/sync-core/tests/{regtest_e2e.rs,core_rest_fake.rs}`, `crates/lab-cli/tests/regtest_cli.rs`
  - `crates/lab-cli/src/main.rs`（`sync --backend core --core-rest-url`, `doctor --core-rest-url`）
  - `scripts/{fetch_bitcoind.sh,regtest_node.sh}`, `scripts/mutation_check.py`（41 項目に拡張）, `.github/workflows/ci.yml`（`regtest-e2e` ジョブ）
- **実行したコマンド**:
  - `VERIFY_SIGNATURES=1 scripts/fetch_bitcoind.sh <dir>`（SHA-256 一致、署名 8 件有効）
  - `scripts/regtest_node.sh start|status|stop`（手動確認）
  - `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets --locked -- -D warnings` / `cargo test --workspace --locked`
  - `BITCOIND_EXE=<Bitcoin Core 32.1> cargo test --workspace --locked -- --ignored`
  - `python3 scripts/mutation_check.py <スクラッチコピー>`
- **テスト結果**: **成功**
  | 対象 | 件数 |
  |---|---|
  | 通常テスト（bitcoind 不要） | 61 passed（うち Core アダプタ単体 3、偽 REST サーバ経由 6、CLI 10） |
  | 実ノード E2E（Bitcoin Core 32.1 regtest） | 5 passed（`sync-core` 3、`lab-cli` 2） |
  | 検査の無効化チェック | 41 件すべて検出（生存 0） |
  - fmt: 差分なし。clippy: 警告なし。
- **出力証拠**: `docs/logs/phase2_cargo_fmt.log`, `phase2_cargo_clippy.log`, `phase2_cargo_test.log`, `phase2_regtest_e2e.log`, `phase2_mutation_check.log`
- **調べた上流資料・commit SHA**: Bitcoin Core 32.1 の REST 実挙動（RESEARCH §6.2）、`bitcoin-core/guix.sigs` `1326cef`（署名鍵）。
- **安全性で気をつけたこと**:
  - Core アダプタは REST のみを使い、RPC の認証情報を一切扱わない。URL はループバックの `http://` に限定し、それ以外はリクエストを送る前に拒否する。
  - mainnet を名乗るノード、genesis が一致しないノードを拒否。ネットワークに接続しない mainnet パラメータのノードを使った E2E で確認。
  - witness なしで返されたブロックを Core 経路でも拒否する（偽 REST サーバのテストで確認）。
  - 送信・署名のコードはテスト専用クレートに隔離。CLI に送金機能は無い。
  - bitcoind はハッシュと署名を検証して取得し、リポジトリには含めない。テスト用ノードは 127.0.0.1 のみにバインドし、P2P を開かず、一時ディレクトリを使い捨てる。
- **未完成 / ブロッカー**: なし。signet の実ノードでは未検証（regtest のみ。O-005）。

### Gate 2 判定: 通過

| 条件 | 根拠 |
|---|---|
| regtest で「送信 → 採掘 → 受信検出」を再現できる | `regtest_send_mine_detect`。実際に署名した tx を 1 件は mempool 経由でブロードキャスト、残りは `generateblock` で採掘し、REST バックエンドでスキャン |
| 期待 txid と vout を断言する自動テストがある | 同テストで 7 件の (高さ, txid, vout, 金額, ラベル) を送信側の知識と照合。CLI 経由でも同じ照合（`sync_against_a_regtest_node_reports_the_expected_outputs`） |
| 同じ tx を二重登録しない | エンジンが範囲内の txid 重複を拒否（Phase 1 のレビュー対応）。永続化層での重複拒否は Phase 3 |

### 設計書のテストケースとの対応（Phase 2 時点の追加）

| ID | 状態 | 備考 |
|---|---|---|
| T02〜T05 | **済（実ノード）** | fixture と同じ状況を regtest 上の有効な tx で再現 |
| T12 ノード停止 | **済** | `stopped_node_is_an_error_not_a_completed_scan`。停止後のスキャン・tip 取得・接続はすべてエラー |
| T11 の一部 | **済** | 偽 REST サーバで不正応答（別ブロック、余剰バイト、prevout 不足、高さ索引の食い違い）を拒否 |

### Phase 2 で分かったこと

1. fixture アダプタと Core アダプタは、同じチェーンに対して**同一の検出結果・同一のバッチ**を返す（`honest_node_gives_the_same_result_as_the_fixture_adapter`）。設計書 §1.4-3 の「同じ検出結果」の最初の実証。
2. `/rest/spenttxouts` のおかげで、prevout をノードから追加の往復なしに正確な金額で取れる。1 ブロックあたり 3 リクエスト（ハッシュ、ブロック、使用済み出力）。
3. Bitcoin Core 30 未満ではこの経路は使えない（`spenttxouts` が無い）。

## Phase 3 Result

- **実装したファイル**:
  - `crates/sync-core/src/store.rs`（`SyncStore` トレイト、不変条件 `check_append`、`MemoryStore`）, `src/sync.rs`（再開可能な同期と reorg 処理）, `src/engine.rs`（ブロック走査を共通化）, `ScanSource::block_ref`、`ReceiverKeys::wallet_id`
  - `crates/lab-storage/`（新規）: SQLite 実装 `SqliteStore`
  - `crates/lab-testkit/src/{sync_suite.rs,test_source.rs}`（全ストア実装に適用する振る舞いテスト群と、reorg・遅延・障害・競合を演じるソース）、`fixture_gen::generate_fork`、`bin/scenario_node.rs`
  - `crates/lab-cli/src/main.rs`: `lab init` / `sync`（永続）/ `status` / `received list` を追加、従来のステートレス実行は `scan` に改名
  - テスト: `crates/sync-core/tests/sync_memory.rs`, `crates/lab-storage/tests/sync_sqlite.rs`, `crates/lab-cli/tests/store_cli.rs`, `regtest_cli.rs`（実 reorg）
- **実行したコマンド**: `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets --locked -- -D warnings` / `cargo test --workspace --locked` / `BITCOIND_EXE=… cargo test --workspace --locked -- --ignored` / `python3 scripts/mutation_check.py <スクラッチコピー>`
- **テスト結果**: **成功**
  | 対象 | 件数 |
  |---|---|
  | 通常テスト | 100 passed（同期スイート: メモリ 14、SQLite 18。CLI の永続系 7） |
  | 実ノード E2E（Bitcoin Core 32.1） | 6 passed（うち実 reorg 1） |
  | 検査の無効化チェック | 67 件すべて検出 |
  - 1 回目のゲートで 2 点失敗し、修正した: (1) 整形差分 1 ファイル、(2) 実 reorg の E2E が**順序依存の比較**をしており不安定だった（同じ高さに 2 件あるとき、ストアは txid 順で返す）。比較を順序非依存に直し、E2E 全体を 6 回連続で実行して全て成功を確認。
  - 無効化チェックの 1 項目は、無効化の書き方自体がコンパイルできなかったため修正して再実行した（ログに明記）。
- **出力証拠**: `docs/logs/phase3_cargo_fmt.log`, `phase3_cargo_clippy.log`, `phase3_cargo_test.log`, `phase3_regtest_e2e.log`, `phase3_mutation_check.log`
- **安全性で気をつけたこと**:
  - ストアに鍵・tweak を保存しない。SQLite ファイルのバイト列に scan 鍵・spend 鍵・全出力の tweak が現れないことを検査。ファイルは 0600。
  - 既存のストアを上書きしない。別ネットワーク・別ウォレット・別バックエンド・別ラベル集合・別スキャナ版のストアには触れずにエラーにする。
  - 遅れているだけのソースを reorg と誤認して結果を捨てない。チェーンが動いた場合は中断する。
  - 追記と巻き戻しはアトミック（それぞれ 1 トランザクション）。根拠は、トランザクションの途中で失敗させるテスト（下の「独立レビューの結果と対応」で追加）。SIGKILL のテストが示すのは「強制終了後のストアが走査済みの接頭部と一致し、再開できる」ことで、原子性そのものではない。
- **未完成 / ブロッカー**: ラベルを後から追加する運用（自動再スキャン）は未実装（O-006）。reorg の深さ上限なし（O-013）。

### Gate 3 判定: 通過

| 条件 | 根拠 |
|---|---|
| 途中で強制終了 → 再開しても検出漏れと重複なし | `killed_sync_resumes_without_loss_or_duplicates`（CLI プロセスを SIGKILL。直後のストアは走査済み接頭部と一致、再開後は期待 7 件と完全一致）。加えて全チャンクサイズ × 全停止位置、ソース障害、コミット失敗の注入 |
| reorg で取り消されたブロック内の検出を正しく修正できる | fixture の分岐チェーン（両実装のスイート、CLI）と、実ノードの `invalidateblock`（`persistent_sync_follows_a_real_reorg`: 孤立した 2 件が消え、再承認後に新しいブロックで再検出） |

### 設計書のテストケースとの対応（Phase 3）

| ID | 状態 | 備考 |
|---|---|---|
| T07 50% で中断 → 再開 | **済** | 50% 停止、全停止位置、SIGKILL |
| T08 DB 削除 → birthday から復元 | **済** | birthday 1〜8 のすべてで「完全再スキャンのうち birthday 以上の部分」と一致。birthday が 1 ブロック遅いと最初の入金を失うことも確認 |
| T09 再編成 | **済** | fixture と実ノード |
| T05 ラベル | 追記 | ラベル集合をストアに固定。別の集合での同期は拒否 |

### 独立レビューの結果と対応（Phase 3）

別エージェントによる敵対的レビュー（対象 commit `ab1ee4f`）の指摘と対応は DECISIONS D-034 に記録した。要点:

- **同期が途中まで進んでから失敗したとき、既に行った変更（reorg の巻き戻しを含む）を必ず報告する**ようにした（`SyncError::Interrupted`）。以前は巻き戻しで消した検出の一覧が失われ得た。
- 止まった理由を `stopped_because` として明示（推測の文言をやめた）。
- SQLite とメモリ実装の並び順の不一致を修正（規約は `Txid` 順）。
- SQLite ファイルを開くときの整合性検査、共通不変条件の追加、書き込みロックの扱い。
- 原子性を、トランザクション途中の失敗を注入するテストで直接確認。

修正後の結果（2026-11-21）:

| 対象 | 件数 |
|---|---|
| 同期スイート（全ストア共通） | 17 ケース × 2 実装 |
| SQLite 固有 | 9（再オープン、上書き拒否、権限、追記の原子性、巻き戻しの原子性、破損ファイル拒否、破損行、ロック待ち、鍵が含まれないこと） |
| CLI 永続系 | 10 |
| 実ノード E2E（Core 32.1 + BlindBit Oracle） | 8 passed |

### Phase 3 で分かったこと

1. 「ソースがチェックポイントより短い」は 2 通りある: 別のチェーンに切り替わった（reorg）か、同じチェーンの手前にいるだけか。後者で巻き戻すと、正しい結果を捨てることになる。区別には保存済みのブロックハッシュ列が必要で、チェックポイント 1 点だけでは足りない。
2. 再承認された同じ tx は、同じ txid・vout のまま別のブロックに属する検出として現れる。検出の同一性は outpoint、所属は (高さ, ブロックハッシュ) で持つ必要がある。
3. テストの比較が暗黙の順序に依存していると、実ノードでは txid が毎回変わるため不安定になる。fixture（決定的）では表に出なかった。
4. 「エラーを返す」だけでは fail-closed として足りない場合がある。エラーの前に状態を変えていたなら、何を変えたかも返さないと、利用者は「消えた検出」を知る手段を失う（レビューで判明）。
5. 2 つの実装に同じ規約を守らせるには、規約を 1 つの関数に置き、同じテストを両方に流すのが確実。SQL の `ORDER BY` に規約を書き写すと、文字列順とバイト順のような差が紛れ込む。

## Phase 4 Result

- **実装したファイル**:
  - `crates/sync-core/src/source/blindbit.rs`（BlindBit Oracle v2 の HTTP バックエンド、feature `blindbit-http`）, `source/local_index.rs`（手元で計算する tweak インデックス）, `source/http.rs`（ループバック専用 HTTP クライアントを共通化・厳格化）, `src/verify.rs`（`cross_check`）
  - `Evidence::TweakIndex` / `IndexedTx` / `IndexedOutput`、`ReceiveScanner::scan_indexed_tx`、エンジンの対応
  - `crates/lab-testkit/src/{fake_http.rs,oracle_node.rs}`（偽の Core REST / 偽の Oracle、実 Oracle プロセスの起動）, `test_source::Tamper`
  - `crates/lab-cli/src/main.rs`: `--backend oracle-v2` / `--backend local-index`、`verify` コマンド、`doctor --oracle-url`
  - テスト: `crates/sync-core/tests/{tweak_index.rs,oracle_fake.rs,oracle_e2e.rs}`, `crates/lab-cli/tests/verify_cli.rs`, `regtest_cli.rs`（実 Oracle）
  - `scripts/build_blindbit_oracle.sh`、CI（E2E ジョブで Oracle をビルド、無効化チェックは別ワークフロー）
  - Phase 2 レビュー対応（DECISIONS D-029）: `source/bitcoin_core.rs` の書き直し（genesis からのヘッダ鎖で全ブロックを固定）ほか
- **実行したコマンド**: `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets --locked -- -D warnings` / `cargo test --workspace --locked` / `BITCOIND_EXE=… BLINDBIT_ORACLE_EXE=… cargo test --workspace --locked -- --ignored` / `scripts/build_blindbit_oracle.sh <dir>` / `VERIFY_SIGNATURES=1 scripts/fetch_bitcoind.sh <dir>` / `python3 scripts/mutation_check.py <スクラッチコピー>`
- **テスト結果**: **成功**
  | 対象 | 件数 |
  |---|---|
  | 通常テスト | 131 passed |
  | 実ノード + 実 Oracle の E2E（Bitcoin Core 32.1、BlindBit Oracle `f9ab7fd`） | 8 passed（3 回連続で実行し全て成功） |
  | 検査の無効化チェック | 92 項目: 検出 91、設定エラー 1（下の「追記」を参照） |
- **出力証拠**: `docs/logs/phase4_cargo_fmt.log`, `phase4_cargo_clippy.log`, `phase4_cargo_test.log`, `phase4_e2e.log`, `phase4_mutation_check.log`
- **調べた上流資料・commit SHA**: BlindBit Oracle v2 `f9ab7fd` を実際にビルドし regtest で動かして応答を確認（RESEARCH §4.1）。
- **安全性で気をつけたこと**:
  - tweak インデックスのスキャンは上流の関数だけで構成（独自のマッチングなし）。公式ベクトルのうち tweak データが定義される受信ケースすべてでフル tx 経路と一致。人間レビュー対象として D-030 に明記。
  - Oracle にはブロックの高さしか送らない。接続先はループバックの IPv4 リテラルに限定。mainnet を名乗る Oracle は拒否。
  - Oracle のブロックは、自分のノードの（genesis から検証した）ヘッダと照合する。追いついていない Oracle や別チェーンの Oracle は「入金なし」にならずエラーになる。
  - インデックスの中身は検証できないことを、型（`SourceCapabilities`）・ドキュメント・テストで明示。
- **未完成 / ブロッカー**: gRPC 未使用（O-014）。ホスト型 Oracle との比較は未実施（O-015）。Oracle の他の構成（cut-through、dust フィルタ）は未検証。

### Gate 4 判定: 通過

| 条件 | 根拠 |
|---|---|
| 独立した 2 方式の結果一致、または不一致の理由を個別 tx 単位で説明できる | 実物どうしの比較（`real_oracle_matches_core_except_for_the_segwit_version_rule`）: Core 直接方式と BlindBit Oracle 方式は、送信側が作った出力については**完全に一致**。差異は 1 件だけで、SegWit v2 の出力を使う tx の出力を Oracle 方式だけが検出する。`verify` はこの差異を `reference_not_eligible: spends_unknown_segwit_version` と説明する（説明不能 0 件） |
| 外部 Oracle 未利用なら「実サーバー比較未完了」と明示 | 該当せず。実物の BlindBit Oracle v2 を regtest で動かして比較した。ただしローカルで動かした 1 構成のみで、公開サーバとの比較ではない |

### 取引ごとの証拠・プライバシー露出・完全性（設計書 Phase 4）

| 観点 | Core 直接（フルブロック） | tweak インデックス（BlindBit Oracle v2） |
|---|---|---|
| クライアントが受け取るもの | ブロック全体（witness 込み）と、使用した出力 | tx ごとの tweak と Taproot 出力 |
| BIP352 の適格性判定 | クライアント自身が行う | サーバが行う（クライアントは入力を見ない） |
| クライアントが検証できること | ハッシュ・マークルルート・witness commitment・PoW・genesis からの連結 | ブロックハッシュが自分のヘッダと一致することだけ |
| 省略（tx を載せない）の検出 | マークルルートで検出 | **不可**（別ソースとの突き合わせが必要） |
| 誤った tweak / 存在しない入金の捏造 | 該当なし | **不可**（同上）。アドレスを知るサーバは偽の入金を「検出」させられる |
| 相手に見えるもの | 自分のノードなので外部には無し | サーバに: 接続元、時刻、要求したブロック範囲（今回はループバックなので外部には無し） |
| scan 鍵 | 手元のみ | 手元のみ（送らない） |
| 1 ブロックあたりのリクエスト | 2（ブロック、使用済み出力）＋ヘッダは 2000 件ずつ | 1（＋ヘッダは自分のノードから） |

### 設計書のテストケースとの対応（Phase 4）

| ID | 状態 | 備考 |
|---|---|---|
| T10 tweak 1 件を削除 | **済** | インデックス単体のスキャンは成功し 1 件見逃す。`verify` は `candidate_omitted_transaction` と報告 |
| T11 不正データ・重複・順序変更 | **済** | 出力・tx の重複はエラー。順序変更は結果に影響しない。誤った tweak、出力の省略・改変、捏造は単体では通り、突き合わせで判明 |

### Phase 4 で分かったこと

1. **BlindBit Oracle v2 は「SegWit v>1 を使う tx はスキャンしない」規則を適用していない。** 上流 bdk-sp と同じ逸脱で、tweak だけを受け取るクライアントには直しようがない（入力が見えない）。BIP 準拠のウォレットと検出結果が食い違う。
2. **Oracle は範囲外の高さに HTTP 200・空のハッシュ・空の結果を返す。** 別経路のヘッダと照合しないクライアントは、未同期の範囲を「入金なし」と区別できない。
3. **インデックスは、アドレスを知っていれば存在しない入金を作れる。** tweak と出力を自分で選べばよく、クライアントのスキャンは正しく「検出」してしまう。防ぐには tx の実在を別途確かめる必要がある。
4. 同一 tx 内で `k = 0` の出力を省かれると、`k = 1` 以降の出力も見えなくなる（ラベル欠落と同じ構造）。`verify` は省かれた出力は説明できるが、連鎖して見えなくなった出力は「説明不能」に分類される。
5. Phase 2 のレビューで、Core 経路の mainnet 拒否が「申告の確認」にとどまっていたことが分かった。**ブロックが genesis に連結していること**を確かめて初めて、データそのものに対する拒否になる。

### 追記: 無効化チェック（Phase 4）

Phase 4 のコミット（`3319c3d`）に対する 92 項目の実行結果: **検出 91、設定エラー 1**（`docs/logs/phase4_mutation_check.log`）。
設定エラーは「PoW 検査を外す」項目の置換パターンが、Phase 2 レビュー対応でのコード変更に追従していなかったもの（検査が外れたのに通ったのではなく、無効化そのものが適用されなかった）。パターンを直し、以後の実行で検出されることを確認した。

### 独立レビューの結果と対応（Phase 4）

別エージェントによる敵対的レビュー（対象 commit `bd2221e`）。**重大・高の欠陥の指摘は無し**。中〜低 13 件の指摘と対応は DECISIONS D-035。要点:

- HTTP のヘッダ・ステータス行に上限が無かった → 16 KiB。
- ノードが応答しないとき、検証済みヘッダのキャッシュを 1 件ずつ捨てながらタイムアウトを繰り返していた → 「無いと答えた」と「答えない」を区別。reorg の分岐点は二分探索。
- `verify` が `scan` より弱かった（チャンク連結・範囲全体の重複を見ない）→ 同じ規則を適用。
- CLI のテキスト出力がインデックス由来の検出を断りなく表示していた → 証拠種別と「UNVERIFIED index claim」を必ず表示。
- インデックスの「あり得ない申告」（総発行量超の金額、出力の無い項目、多すぎる出力）を拒否。
- ローカルインデックスのポリシーを backend id に含め、ストアで混ざらないようにした。
- ヘッダ参照をブロックごとの 1 リクエストから、範囲ごとの 1 リクエストに。

## Phase 5 Result

### 独立レビューの結果と対応（Phase 5）

別エージェントによる敵対的レビュー（対象 commit `d5c6149`）。指摘 9 件（うち重要度「高」1 件。「重大」は無し）と対応は DECISIONS D-037。要点:

- **（高）ベンチマーク結果に txid・パス・URL が入り得た**（失敗した run のエラー文面を記録していた）→ エラーの分類だけを記録。置いてあった結果ファイルに漏れは無かった。
- フルブロック型の保証を文書が強く書きすぎていた → 「接続先が自分のノードである限り」に訂正。一貫した別チェーンを出すノード（F12）を故障注入に追加。
- 「突き合わせで分かる」は、ソースがいまも同じ嘘をつく場合だけ → 1 回だけ嘘をつくインデックス（I19、**検出不能**）を追加。
- 変更後のストア読み出しが失敗すると、巻き戻しが報告されない経路があった → 変更後は読み返さない実装に。
- リクエスト数・バイト数が接続時の分を含んでいなかった → 修正し、**計測を取り直した**。
- レポートの文章にデータで裏付けられない記述があった → 派生値を `export` が生成し、文章はその表だけを引用。

レビューはレポートの全数値を生データから再計算しており、表は `export` の出力と一致していた（食い違いは手書きの文章の 3 箇所で、いずれも書き直した）。

- **実装したファイル**:
  - `crates/lab-cli/src/bench.rs`（計測・集計・CSV / Markdown 出力）, `src/main.rs`（`bench` / `export` コマンド）, `build.rs`（コンパイラとターゲットを結果に記録）
  - `crates/lab-testkit/src/fixture_gen.rs`（任意長の決定的チェーン `generate_long`）, `regtest_wallet.rs`（実ノード上の長いシナリオ `long_scenario`、`fund_many`、捏造項目の生成）, `fault_matrix.rs`（故障注入マトリクス）, `bin/{bench_node,fault_report}.rs`, `bin/gen_fixture.rs`（`--long`）
  - テスト: `crates/sync-core/tests/{long_chain.rs,fault_matrix.rs}`, `crates/lab-cli/tests/bench_cli.rs`、既存テストへの追加
  - 文書: `docs/BENCHMARK_PROTOCOL.md`, `docs/BENCHMARK_REPORT.md`, `docs/benchmarks/`（全試行の JSON / CSV / Markdown）, `docs/FAULT_INJECTION.md`, `docs/THREAT_MODEL.md`, `docs/PHASE6_DECISION_MATERIALS.md`
  - Phase 3 / Phase 4 のレビュー対応（DECISIONS D-034 / D-035）
- **実行したコマンド**: `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets --locked -- -D warnings` / `cargo test --workspace --locked` / `BITCOIND_EXE=… BLINDBIT_ORACLE_EXE=… cargo test --workspace --locked -- --ignored` / `python3 scripts/mutation_check.py <スクラッチコピー>` / `cargo build --release --locked -p lab-cli -p lab-testkit` / `gen_fixture --long N` / `bench_node 1000 10` / `silent-sync bench …`（6 回）/ `silent-sync export …` / `cargo run -p lab-testkit --bin fault_report`（手順の全文は `docs/BENCHMARK_PROTOCOL.md` §5）
- **テスト結果**: **成功**
  | 対象 | 件数 |
  |---|---|
  | 通常テスト（K_max の 2 件を含む） | 179 passed, 0 failed |
  | 実ノード + 実 Oracle の E2E（Bitcoin Core 32.1、BlindBit Oracle `f9ab7fd`） | 9 passed, 0 failed |
  | 検査の無効化チェック（最終コード） | 158 項目すべて検出（生存 0、設定エラー 0）。レビュー前のコードでの 147 項目の実行は検出 146・無効化の書き方の誤り 1（`phase5_mutation_check_before_review.log`） |
  | ベンチマーク | 300 試行、失敗 0、結果の食い違い 0（レビュー対応後に取り直したもの） |
  | 故障注入 | 35 種: 単独で拒否 23、影響なし 2、単独では検出不能で突き合わせで判明 9、検出不能 1 |
  | GitHub Actions | 通常テストと E2E の両ジョブが成功（コミット `8dfbc3b` まで確認） |
- **出力証拠**: `docs/logs/phase5_cargo_fmt.log`, `phase5_cargo_clippy.log`, `phase5_cargo_test.log`, `phase5_e2e.log`, `phase5_mutation_check.log`（いずれも最終コードで取り直したもの）、`docs/benchmarks/all_runs.{json,csv,md}`, `bench_node.{json,log}`
- **調べた上流資料・commit SHA**: BlindBit Oracle `f9ab7fd` の `internal/server/handler.go`（`GetFullBlock`: 項目は出力を持つ tx についてのみ作られ、使用済みの出力も除かれない）、`internal/database/dbpebble/read.go`（`FetchOutputsAll`）。minreq 2.14.1 の `with_max_headers_size` / `with_max_status_line_length`。
- **安全性で気をつけたこと**:
  - 計測結果のファイルに txid・ウォレット識別子・パス・URL・鍵を入れない（テストで検査）。既存の結果ファイルは上書きしない。
  - 計測値を手で書かない。レポートの表は `export` の出力で、引用した比率も同じ結果ファイルから計算した。
  - サーバ側の前処理時間は取得できないので `null` のまま（推定値を入れていない）。
  - 故障注入で「検出できない」と分かったものを、検出できるかのように書かない。判定はテストで固定し、文書の表は生成物と一致することを検査。
  - インデックス由来の検出は「サーバの申告」であることを、CLI の全出力に表示。
- **未完成 / ブロッカー**: ループバック・小さなブロック・最大 1000 ブロックでの計測に限られる（`BENCHMARK_REPORT.md` §6）。サーバ側の費用は未計測。ブロッカーは無い。

### Gate 5 判定: 通過

| 条件（再現コマンド、構成、バージョン、全試行結果、限界が揃っている） | 根拠 |
|---|---|
| 再現コマンド | `docs/BENCHMARK_PROTOCOL.md` §5 |
| 構成 | 同 §2〜§4（モード、同条件の定義、測定対象のチェーン） |
| バージョン | 結果ファイルの `environment`（ツール、スキャナ + 上流 rev、コンパイラ、ターゲット、ビルド種別）と `note`（Bitcoin Core 32.1、BlindBit Oracle `f9ab7fd`） |
| 全試行結果 | `docs/benchmarks/all_runs.json` / `.csv`（300 試行。中央値・最小・最大・ばらつきは `BENCHMARK_REPORT.md` §3） |
| 限界 | `BENCHMARK_REPORT.md` §6、`BENCHMARK_PROTOCOL.md` §7、`FAULT_INJECTION.md`「この表が示さないこと」 |

### 設計書のテストケースとの対応（Phase 5 時点の全体）

| ID | 状態 | 根拠 |
|---|---|---|
| T01 公式 vectors | 済 | `bip352_vectors.rs` |
| T02 1 件の受信 / T03 複数送金・複数入力 / T04 同一 tx 内の複数出力 / T05 ラベル | 済 | `fixture_scan.rs`、実ノード `regtest_e2e.rs` |
| T06 10 / 100 / 1000 ブロック | **済** | `BENCHMARK_REPORT.md`、`long_chain.rs`（チャンク境界）、実ノード `long_scenario_is_detected_exactly` |
| T07 中断 → 再開 / T08 復元 / T09 再編成 | 済 | 同期スイート、CLI（SIGKILL）、実ノードの reorg。計測は `sync_resume` / `restore` |
| T10 tweak 1 件を削除 | **済** | `FAULT_INJECTION.md` I01〜I03、I19 |
| T11 不正データ・重複・順序変更 | **済** | 同 F01〜F05、F10〜F12、I04〜I11、I15〜I18、B01〜B04 |
| T12 ノード・Oracle 停止 | **済** | 同 F06〜F09、I12〜I14、同期スイート（障害・遅延）、タイムアウト |

### Phase 5 で分かったこと

1. **この条件（ループバック、1 KB 未満のブロック）では、インデックス方式は速くならなかった。** クライアントの CPU は減る（0.6 倍前後）が、wall は 1.07〜1.34 倍で、同じ日の 2 回の計測でも比が変わった。時間の内訳は分離して測っておらず、インデックス方式の利点が出るはずの条件も測れていない。
2. **「genesis からヘッダ鎖を検証する」ことには、チェーン長に比例する固定費がある**（1116 ブロックで約 90 KB / 接続）。安全性のために入れた検査の費用が、計測して初めて数字になった（O-016）。
3. **tweak インデックス型は、見逃しだけでなく「誤った金額」「存在しない入金」も成功として返す。** 捏造に必要なのは受信者のアドレスだけ。表示上の区別（証拠種別）が安全性の一部になる。
4. **フルブロック型の保証も「接続先が自分のノードである限り」のものだった。** ヘッダごと一貫した偽のチェーンは、regtest / signet では誰でも作れる。レビューで指摘されるまで、文書はこれを書いていなかった。
5. **突き合わせは「いまの答え」を比べるだけ**で、過去のスキャン時の嘘は見つけられない。
6. 「検出できる／できない」を文書に書くだけでは、コードの変更とずれていく。判定をテストで固定し、表を生成物と突き合わせて初めて維持できる（実際、`verify` を直したときにテストが文書の更新を要求した）。
7. 計測コード自体にも、測る区間の取り方（準備を含めない、接続を含める）やラウンドロビンの実行順など、結果を左右する決定が多い。手順書を先に書くと、何を測っていないかが明確になる。

## Phase 6（進路決定）

設計書は Phase 6 を「測定結果と実装体験にもとづき 3 択から選ぶ」と定めている。**選択は所有者の判断であり、行っていない。**
判断材料を `docs/PHASE6_DECISION_MATERIALS.md` にまとめた（手元にある成果、測定と実装から分かった事実、3 つの進路それぞれに必要なもの、決める前に答える必要のある問い、実装者の所見）。

UI の作り込み、Tauri、製品形態の決め打ちは行っていない（CLAUDE.md 規約 9）。

## 所有者の確認・判断が必要なもの

| 項目 | 内容 |
|---|---|
| 進路（Phase 6） | SDK 化／実験ウォレット／研究・インフラ。`docs/PHASE6_DECISION_MATERIALS.md` |
| ライセンス（O-001） | 未設定。決まるまで第三者は利用できない |
| 上流への報告（O-002） | bdk-sp の 2 点（K_max、SegWit v>1）、BlindBit Oracle の 1 点（SegWit v>1）。未報告 |
| 人間レビュー対象の承認 | D-005・D-006（承認済みの方針に基づく補正の実装内容）、**D-030（tweak インデックスからのスキャン。未承認）** |
| Bitcoin Core のバージョン（D-019） | 32.1 を選んだ。別のバージョンを望む場合は差し替え可能 |

## 過去の記録: 次に取り組む最小タスク（Phase 1 完了時点）

1. Bitcoin Core のバージョンと入手方法を決める（DECISIONS O-003。人間の判断が必要）。
2. regtest ノードを localhost バインドで起動 / 停止するスクリプト。認証は cookie、`.gitignore` 済み。
3. `getblock` で取得したブロックから `Evidence::FullBlock` を作る最小の `bitcoin_core` アダプタ。prevout の取得方法（DECISIONS O-004）を実挙動で確認する。
4. fixture 生成器を流用して実際に署名・ブロードキャストし、「送信 → 採掘 → 検出」の E2E テストを 1 本通す。
