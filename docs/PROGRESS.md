# PROGRESS

最終更新: 2026-11-21

| Phase | 内容 | 状態 |
|---|---|---|
| 0 | 上流調査・セットアップ | **完了**（Gate 0 通過） |
| 1 | 暗号処理を作らない受信検出（fixture） | **完了**（Gate 1 通過） |
| 2 | Bitcoin Core / regtest 統合 | 未着手 |
| 3 | 永続化・再開・復元 | 未着手 |
| 4 | 第二バックエンド | 未着手 |
| 5 | ベンチマーク・故障注入 | 未着手 |
| 6 | 進路決定 | 未着手 |

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

## Not done（このフェーズの対象外、または未実施）

- [ ] Bitcoin Core 連携、regtest での送信 → 採掘 → 検出（Phase 2）
- [ ] SQLite 永続化、チェックポイント、中断再開、reorg（Phase 3）
- [ ] tweak 方式の `Evidence`、Oracle アダプタ、`Verifier::cross_check`（Phase 4）
- [ ] ベンチマーク、故障注入レポート、`export` / `bench` / `status` / `received list` / `lab init` コマンド（Phase 3〜5）
- [ ] `docs/THREAT_MODEL.md`, `docs/BENCHMARK_PROTOCOL.md`（Phase 4〜5 の前に作成）
- [ ] 機密情報検出の pre-commit / CI ツール（DECISIONS O-009）
- [ ] MSRV 1.85.0 でのテスト実行（`cargo +1.85.0 check --workspace --all-targets --locked` の成功までは確認済み。CI には未追加）
- [ ] property-based / fuzz テスト（設計書 §11 で初期 MVP では任意）
- [ ] 上流への不具合報告（DECISIONS O-002）

## Blocked

現時点でブロックされている作業はない。

Phase 2 以降の前提として未解決のもの:
- この実行環境に `bitcoind` が無い。Bitcoin Core のバージョン選定と入手（DECISIONS O-003）が Phase 2 の最初の作業になる。
- BlindBit Oracle v2 の gRPC スキーマと regtest での実動作が `UNVERIFIED`（RESEARCH §4）。確認できなければ Phase 4 は設計書どおり fixture ベースの `indexed_data` アダプタを先に作り、外部 Oracle 連携を `BLOCKED` と記録する。

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
  - CI ワークフローは作成したが、**GitHub Actions 上での実行結果はまだ確認していない**。
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

## 次に取り組む最小タスク（Phase 2 の入口）

1. Bitcoin Core のバージョンと入手方法を決める（DECISIONS O-003。人間の判断が必要）。
2. regtest ノードを localhost バインドで起動 / 停止するスクリプト。認証は cookie、`.gitignore` 済み。
3. `getblock` で取得したブロックから `Evidence::FullBlock` を作る最小の `bitcoin_core` アダプタ。prevout の取得方法（DECISIONS O-004）を実挙動で確認する。
4. fixture 生成器を流用して実際に署名・ブロードキャストし、「送信 → 採掘 → 検出」の E2E テストを 1 本通す。
