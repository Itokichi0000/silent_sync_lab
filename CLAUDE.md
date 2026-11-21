# CLAUDE.md — Silent Sync Lab 運用規約

要求仕様は `SILENT_SYNC_LAB_DESIGN.md`。現在地は `docs/PROGRESS.md`。迷ったら仕様書に戻る。

## 最重要ルール

1. **EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.** mainnet 接続・実資金・本番運用はしない。`LabNetwork` に mainnet を足さない。
2. **独自の暗号を書かない。** 楕円曲線演算・ハッシュ・BIP352 のマッチングは上流 `bdk_sp` を呼ぶ。`sync-core` が足してよいのは script の検査、データの検証、エラー分類、オーケストレーションだけ。
3. **秘密を出さない。** scan 秘密鍵・シード・出力ごとの tweak・RPC 認証情報を、stdout / ログ / JSON / `Debug` / panic / テスト成果物 / コミットに書かない。新しい秘密鍵をリポジトリに追加しない（テスト鍵は BIP352 公式ベクトルの公開値のみ）。
4. **fail-closed。** 部分応答・順序違い・重複・ハッシュ不連続・ネットワーク不一致・tx 単位のエラーは、範囲全体の失敗にする。静かに「同期済み」としない。
5. **検出は残高ではない。** 表示は `detected_received_outputs`。残高・利用可能額・支出可能性を示す語を使わない。
6. **送金・署名機能を作らない。** 上流の送信関数と tx の署名は、テスト専用クレート `crates/lab-testkit` でのみ使う。`sync-core` と `lab-cli` の通常の依存に `lab-testkit` を入れない（`dev-dependencies` のみ）。
7. **嘘をつかない。** テスト未実行で「完成」と言わない。未実装は `NOT IMPLEMENTED`、未確認は `UNVERIFIED`、外部要因で進めないものは `BLOCKED` と書く。計測値を捏造しない。モックで成功を装わない。
8. **API を記憶から推測しない。** 上流の実コードを読んで確認し、commit SHA を `docs/RESEARCH.md` に残す。
9. **Phase Gate を飛ばさない。** UI の作り込み、Tauri、製品形態（SDK / ウォレット）の決め打ちは Phase 6 まで行わない。

## 人間レビューが必須の変更

秘密情報の外部送信／暗号処理に関わる変更（上流の挙動の補正を含む）／資金利用を可能にする変更／暗号スタックのバージョン変更。判断が要るときは実装前に質問する。

## 構成

```
crates/sync-core/   同期コア（SDK 候補）。CLI・表示・ストレージに依存しない
  src/network.rs      LabNetwork（mainnet 不可）, ChainIdentity
  src/protocol.rs     上流スキャナのラッパー（適格性、K_max 補正、panic 封じ込め、tweak インデックスからのスキャン）
  src/engine.rs       scan_range（ステートレス）、バッチ整合性検査、インデックス申告の上限検査
  src/store.rs        SyncStore トレイト、全実装共通の不変条件 check_append、MemoryStore
  src/sync.rs         永続同期（再開、reorg、中断時の部分レポート）
  src/verify.rs       cross_check（2 ソースの突き合わせと差分の説明）
  src/source/         ScanSource トレイト、Evidence（FullBlock / TweakIndex）、fixture アダプタ
  src/source/block_check.rs   フルブロック共通の構造検査
  src/source/http.rs          ループバック専用 HTTP（IPv4 リテラル + ポート、上限つき）
  src/source/bitcoin_core.rs  Bitcoin Core REST バックエンド（feature `core-rest`、genesis からヘッダ鎖を検証）
  src/source/blindbit.rs      BlindBit Oracle v2 HTTP バックエンド（feature `blindbit-http`）
  src/source/local_index.rs   フルブロックから手元で計算する tweak インデックス
crates/lab-storage/ SyncStore の SQLite 実装。依存方向は lab-storage → sync-core
crates/lab-cli/     薄い CLI（バイナリ名 silent-sync）。依存方向は lab-cli → sync-core, lab-storage
  src/bench.rs        計測・集計・エクスポート（結果に個人データを入れない）
crates/lab-testkit/ テスト専用。fixture / 長いチェーンの生成、regtest ノード・Oracle 操作、
                    署名付きテスト tx、偽 REST / 偽 Oracle、同期スイート、故障注入マトリクス
fixtures/bip352/    公式ベクトル（SHA-256 固定、無改変）
fixtures/local/     決定的 fixture（生成器と一致することをテストで検査）
docs/               RESEARCH / DECISIONS / DEPENDENCIES / PROGRESS / THREAT_MODEL /
                    FAULT_INJECTION / BENCHMARK_PROTOCOL / BENCHMARK_REPORT / benchmarks / logs
```

## コマンド

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked          # K_max の 2 テストで約 2 分かかる
cargo run -p lab-testkit --bin gen_fixture -- fixtures/local/regtest_basic_v1.json
cargo run -p lab-cli -- scan --backend fixture --fixture fixtures/local/regtest_basic_v1.json --label 1
cargo run -p lab-testkit --bin fault_report        # docs/FAULT_INJECTION.md の表を再生成

# 実ノードを使う E2E（通常の cargo test では ignored。バイナリが無いと失敗する）
export BITCOIND_EXE="$(VERIFY_SIGNATURES=1 scripts/fetch_bitcoind.sh /path/outside/repo/core)"
export BLINDBIT_ORACLE_EXE="$(scripts/build_blindbit_oracle.sh /path/outside/repo/oracle)"
cargo test --workspace --locked -- --ignored
```

- 検証コード（`sync-core` の各検査、`lab-storage` の不変条件、CLI の拒否条件）を足したり変えたりしたら、`scripts/mutation_check.py` の項目も更新し、スクラッチコピーに対して実行して生存 0 を確認する（使い方はスクリプト冒頭。全項目で 1 時間以上かかる）。
- バックエンドやエンジンの挙動を変えたら `cargo run -p lab-testkit --bin fault_report` で故障注入の表を作り直す（`docs/FAULT_INJECTION.md` と一致しないとテストが失敗する）。
- ベンチマークの数値を手で書かない。`silent-sync bench` → `export` の出力を使う（`docs/BENCHMARK_PROTOCOL.md`）。計測中は他の重い処理を動かさない。
- インデックス由来（`tweak_index`）の検出を、フルブロック由来と同じ確度のものとして表示・記述しない。
- フェーズの終わりに、文脈を共有しない別エージェントへ敵対的レビューを依頼し、指摘は自分で再現してから対応する（DECISIONS D-018）。
- 依存を追加・更新したら `docs/DEPENDENCIES.md` と `docs/DECISIONS.md` に理由を書く。`cargo update` を無指定で実行しない（暗号スタックが上がる。DECISIONS D-004）。
- 本番コードで `unwrap()` は clippy が拒否する。エラーは型で分類する。
- 各フェーズの終わりに、仕様書 §15 のテンプレートで報告し、`docs/PROGRESS.md` とログ（`docs/logs/`）を更新する。

## フェーズ

| Phase | 内容 | 状態 |
|---|---|---|
| 0 | 上流調査・セットアップ | 完了（Gate 0 通過） |
| 1 | fixture による受信検出 | 完了（Gate 1 通過） |
| 2 | Bitcoin Core / regtest 統合 | 完了（Gate 2 通過） |
| 3 | 永続化・再開・復元 | 完了（Gate 3 通過） |
| 4 | 第二バックエンド | 完了（Gate 4 通過） |
| 5 | ベンチマーク・故障注入 | 完了（Gate 5 通過） |
| 6 | 進路決定 | 判断材料を作成（`docs/PHASE6_DECISION_MATERIALS.md`）。**決定は所有者待ち** |
