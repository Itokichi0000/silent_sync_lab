# CLAUDE.md — Silent Sync Lab 運用規約

要求仕様は `SILENT_SYNC_LAB_DESIGN.md`。現在地は `docs/PROGRESS.md`。迷ったら仕様書に戻る。
ウォレット段階の範囲は `docs/WALLET_DESIGN_PROPOSAL.md`（所有者が 2026-11-21 に承認。DECISIONS D-046）。規約 3・5・6 は、その承認の範囲に限って改訂してある。

## 最重要ルール

1. **EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.** mainnet 接続・実資金・本番運用はしない。`Network` に mainnet を足さない。
2. **独自の暗号を書かない。** 楕円曲線演算・ハッシュ・BIP352 のマッチングは上流 `bdk_sp` を呼ぶ。`silent-sync` が足してよいのは script の検査、データの検証、エラー分類、オーケストレーションだけ。
3. **秘密を出さない。** scan 秘密鍵・spend 秘密鍵・シード・出力ごとの tweak・RPC 認証情報を、stdout / ログ / JSON / `Debug` / panic / テスト成果物 / コミットに書かない。新しい秘密鍵をリポジトリに追加しない（テスト鍵は BIP352 公式ベクトルの公開値のみ。テストが使う使い捨ての鍵は実行時に作る）。鍵とシードはファイルに保存しない（起動時に受け取る）。出力ごとの tweak は保存せず、支出するときにだけフルブロックから再計算して使う。
4. **fail-closed。** 部分応答・順序違い・重複・ハッシュ不連続・ネットワーク不一致・tx 単位のエラーは、範囲全体の失敗にする。静かに「同期済み」としない。
5. **検出は残高ではない。** 受信専用の部分（`silent-sync`、`lab-cli`）の表示は `detected_received_outputs`。「未使用」と呼んでよいのは、**フルブロック由来の検出のうち、使用済みの記録が無いもの**だけで、根拠（自分のノードで確認）と条件（承認数）を必ず添える。インデックス由来の検出は合計に入れない。根拠を添えずに「残高」「利用可能額」と書かない。
6. **送金・署名は `crates/silent-wallet` と `crates/wallet-cli` だけ。** `silent-sync`・`silent-sync-sqlite`・`lab-cli` には置かない（上流の送信関数と tx の署名を使ってよいのは、この 2 つとテスト専用の `crates/lab-testkit` だけ）。regtest と signet のみ。送信の前に必ず内容を表示して確認を求める。通常の依存に `lab-testkit` を入れない（`dev-dependencies` のみ）。
7. **嘘をつかない。** テスト未実行で「完成」と言わない。未実装は `NOT IMPLEMENTED`、未確認は `UNVERIFIED`、外部要因で進めないものは `BLOCKED` と書く。計測値を捏造しない。モックで成功を装わない。
8. **API を記憶から推測しない。** 上流の実コードを読んで確認し、commit SHA を `docs/RESEARCH.md` に残す。
9. **Phase Gate を飛ばさない。** UI の作り込み、Tauri、製品形態（SDK / ウォレット）の決め打ちは Phase 6 まで行わない。

## 人間レビューが必須の変更

秘密情報の外部送信／暗号処理に関わる変更（上流の挙動の補正を含む）／資金利用を可能にする変更／暗号スタックのバージョン変更。判断が要るときは実装前に質問する。

## 構成

```
crates/silent-sync/   同期コア（SDK 候補）。CLI・表示・ストレージに依存しない
  src/network.rs      Network（mainnet 不可）, ChainIdentity
  src/protocol.rs     上流スキャナのラッパー（適格性、K_max 補正、panic 封じ込め、tweak インデックスからのスキャン）
  src/engine.rs       scan_range（ステートレス）、バッチ整合性検査、インデックス申告の上限検査
  src/store.rs        SyncStore トレイト、全実装共通の不変条件 check_append、MemoryStore
  src/sync.rs         永続同期（再開、reorg と深さの上限、中断時の部分レポート）
  src/verify.rs       cross_check（2 ソースの突き合わせと差分の説明）
  src/audit.rs        audit_store（保存済みの検出結果と参照ソースの再スキャンの照合。ストアは読むだけ）
  src/rescan.rs       rescan_into（ラベル追加時の再スキャン。別のストアに作り直し、差し替えは呼び出し側）
  src/source/         ScanSource トレイト、Evidence（FullBlock / TweakIndex）、fixture アダプタ
  src/source/block_check.rs   フルブロック共通の構造検査
  src/source/http.rs          ループバック専用 HTTP（IPv4 リテラル + ポート、上限つき）
  src/source/bitcoin_core.rs  Bitcoin Core REST バックエンド（feature `core-rest`、genesis からヘッダ鎖を検証）
  src/source/blindbit.rs      BlindBit Oracle v2 HTTP バックエンド（feature `blindbit-http`）
  src/source/local_index.rs   フルブロックから手元で計算する tweak インデックス
crates/silent-sync-sqlite/ SyncStore の SQLite 実装と、再スキャン後のファイル差し替え。依存方向は silent-sync-sqlite → silent-sync
crates/silent-wallet/ ウォレットのライブラリ: 未使用の集計、コイン選択、送金 tx の組み立てと署名（上流の関数を呼ぶ）
crates/wallet-cli/  ウォレットの CLI（バイナリ名 silent-wallet）。依存方向は wallet-cli → silent-wallet → silent-sync, silent-sync-sqlite
crates/lab-cli/     薄い CLI（バイナリ名 silent-sync）。受信専用。依存方向は lab-cli → silent-sync, silent-sync-sqlite
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
UPDATE_API_LISTS=1 cargo test -p silent-sync --test public_api   # 公開 API を変えたら docs/api/ を更新
cargo clippy -p silent-sync --lib --locked --features <feature> -- -D warnings   # ライブラリ単体・feature ごと（CI と同じ）

# 実ノードを使う E2E（通常の cargo test では ignored。バイナリが無いと失敗する）
export BITCOIND_EXE="$(VERIFY_SIGNATURES=1 scripts/fetch_bitcoind.sh /path/outside/repo/core)"
export BLINDBIT_ORACLE_EXE="$(scripts/build_blindbit_oracle.sh /path/outside/repo/oracle)"
cargo test --workspace --locked -- --ignored
```

- 検証コード（`silent-sync` の各検査、`silent-sync-sqlite` の不変条件、CLI の拒否条件）を足したり変えたりしたら、`scripts/mutation_check.py` の項目も更新し、スクラッチコピーに対して実行して生存 0 を確認する（使い方はスクリプト冒頭。全項目で 1 時間以上かかる）。
- `probes/` はワークスペースの外にある。クレートごとに `-p` を付けて実行する（まとめて実行すると feature が統合され、「既定では届かない」ことの検査が意味を失う）。`scripts/check_embedder_view.sh` がその形で実行する。
- バックエンドやエンジンの挙動を変えたら `cargo run -p lab-testkit --bin fault_report` で故障注入の表を作り直す（`docs/FAULT_INJECTION.md` と一致しないとテストが失敗する）。
- ベンチマークの数値を手で書かない。`silent-sync bench` → `export` の出力を使う（`docs/BENCHMARK_PROTOCOL.md`）。計測中は他の重い処理を動かさない。
- インデックス由来（`tweak_index`）の検出を、フルブロック由来と同じ確度のものとして表示・記述しない。
- 公開 API（`silent-sync`、`silent-sync-sqlite`）を変えるときは、`docs/api/` の一覧の差分を確認してコミットする。実験・診断用の機能は feature の下に置き、既定で有効にしない。出力ごとの tweak は、既定の公開 API からは渡さない。feature `spend` を有効にしたときだけ、支出のための狭い関数から渡す（D-039 を D-046 で改訂。これ以上広げる変更は人間レビュー必須）。
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
| 6 | 進路決定 | 所有者が決定: SDK を先に完成させ、その後ウォレット（`docs/PROGRESS.md` の「SDK 化 Result」）。ウォレット段階に入るには、上の規約の変更と設計案の承認が先 |
