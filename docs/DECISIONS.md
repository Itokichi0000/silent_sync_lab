# DECISIONS — 設計上の決定と未決事項

各決定には根拠と、見直す条件を書く。`承認` は人間（リポジトリ所有者）の明示的な回答があったもの。

## 決定済み

### D-001 リポジトリ直下をワークスペースにする
設計書 §13 は「新規ディレクトリ `silent-sync-lab` を作る（既存 repo は破壊しない）」。作業対象の `silent_sync_lab` リポジトリが空で用意されていたため、入れ子にせずその直下を Cargo ワークスペースにした。

### D-002 最小構成は `sync-core` + `lab-cli` の 2 crate
`lab-storage` / `lab-bench` は Phase 3 / 5 で実体ができてから切り出す（設計書 §7 の「小さな Workspace から始めてよい」）。依存方向は `lab-cli → sync-core` のみ。`sync-core` は表示・CLI・ストレージに依存しない。

### D-003 `bdk_sp` は git の commit SHA で固定する
`bb373e566fa2a9ee037cb15de2cbe6e679aac7b0`（master HEAD、2026-10-27）。crates.io / タグの 0.2.0 は `K_MAX` 対応（PR #75）より前なので使えない。
- 限界: 未リリースの commit であり、上流が履歴を書き換えるとビルド不能になる。
- 見直し: `K_MAX` を含むリリース（0.3.0 など）が出たら crates.io 版へ移行する。

### D-004 暗号スタックのバージョンを上流の `Cargo.lock` に合わせる
`bitcoin 0.32.7`, `secp256k1 0.29.1`, `secp256k1-sys 0.10.1`, `bitcoin_hashes 0.14.0`, `bech32 0.11.0`, `bitcoin-io 0.1.3`, `bitcoin-units 0.1.2`, `base58ck 0.1.0`, `hex-conservative 0.2.1`, `bitcoin-internals 0.3.0`。
- 理由: 何も指定しないと cargo は `bitcoin 0.32.102` / `secp256k1 0.29.100`（2026 年の「ブリッジ」リリース。後者は 2026-10-26 公開）を選ぶ。上流スキャナ自身のテストが走った組み合わせで動かす方が、実験用の暗号コードとしては保守的。
- 代償: `bitcoin 0.32.8〜0.32.10` の修正（P2P メッセージのデコード、`CompactSize` 範囲チェック等）を含まない。本プロジェクトは P2P を使わないが、Phase 2 で Core からブロックをデコードする際に再評価する。
- 強制方法: `Cargo.lock` を追跡し、CI は `--locked`。

### D-005 K_max 超過時は BIP352 準拠の結果に補正する【承認】
上流は 2324 件以上マッチする tx でエラーを返す（RESEARCH U-1）。BIP352 は最初の 2323 件を検出して停止する。
- 実装（`protocol.rs` `scan_tx_capped_at_k_max`）: 上流が `ScanLimitExceeded` を返したときだけ、(1) 上流の `compute_tweak_data` で公開 tweak を得る、(2) 上流の `get_spks_from_tweak(tweak, K_MAX)` で `k = 2323` の候補 script を得る、(3) tx のコピー上でその script を持つ出力を空にする（位置は保つので vout は不変）、(4) **無改変の上流スキャナ**をもう一度かける、(5) txid を元の値に戻す。結果が 2323 件でなければエラー。
- 独自の曲線演算・ハッシュ・マッチングは書いていない。鍵に触れるのは上流関数だけ。
- 検証: 公式ベクトル #27 で 2323 件。さらに別経路（`get_spks_from_tweak` を `k = 0..2322` で直接呼ぶ）の script 集合と完全一致し、唯一マッチしなかった出力が `k = 2323` の script であることをテストで確認。
- 代償: 該当 tx では上流スキャンが 2 回走る（デバッグビルドで約 50 秒）。攻撃者がこれを起こすには受取人宛の出力を 2324 個以上持つ tx を採掘させる必要がある。
- 見直し: 上流が BIP どおりに直したら、この関数を削除する。

### D-006 「SegWit v>1 を使う tx はスキャンしない」を `sync-core` で補う【承認】
上流に無い規則（RESEARCH U-2）。`ReceiveScanner::eligibility` が prevout の `scriptPubKey` の witness version を見て `SkipReason::SpendsUnknownSegwitVersion` を返す。暗号処理ではなく script の検査。公式ベクトルに該当ケースが無いので fixture（高さ 6）で検証し、「上流単体ならマッチする」ことも同じテストで示している。

### D-007 入力数と prevout 数の不一致はエラー
上流は黙って切り詰める（RESEARCH U-3）。切り詰められると共有秘密が変わり、**支払いを静かに見逃す**。`ProtocolError::PrevoutCountMismatch` を返し、エンジンは範囲全体を失敗にする。

### D-008 上流の panic をエラーとして封じ込める
ソース由来のデータはこの crate ではコンセンサス検証されない。不正な witness で上流がスライス範囲外 panic を起こす（RESEARCH U-5）ので、上流呼び出しを `catch_unwind` で包み `ProtocolError::UpstreamPanic` にする。プロセスを落とさず、その範囲を「スキャン済み」にもせず失敗させる。
- 注意: `panic = "abort"` でビルドすると効かない。ワークスペースでは設定していない。

### D-009 mainnet は型で表現できないようにする
`LabNetwork` は `Regtest` / `Signet` の 2 値のみ。`FromStr`・`serde`・`bitcoin::Network` からの変換のすべてで mainnet を専用エラーで拒否する。`ChainIdentity::verified` は genesis hash も照合するので、「regtest と名乗る mainnet データ」も拒否される。CLI は clap の値パーサ経由で同じ関数を使う。
- 限界: カスタム signet はデフォルト signet と genesis を共有するため、genesis hash だけでは区別できない。

### D-010 固定テスト鍵は BIP352 公式ベクトルの公開鍵素材を使う
`lab-cli` の受取人は `fixtures/bip352/send_and_receive_test_vectors.json` の先頭ケースの鍵。リポジトリ独自の秘密鍵は 1 つも追加していない。
- scan 秘密鍵は `ReceiverKeys` の中に閉じ、`Debug` は伏せ字、`Serialize` / `Display` は実装しない。spend 秘密鍵は公開鍵に変換した時点で捨てる。
- 公開済みの値であっても CLI はこれを出力しない（テストで検査）。
- fixture の送信者鍵は固定文字列のハッシュから導出する使い捨ての値で、どのチェーンの資金も支配しない。

### D-011 検出結果に tweak を持たせない
上流の `SpOut` は出力ごとの tweak を含むが、受信専用の MVP には不要。`DetectedOutput` に入れず、エンジンで捨てる。`ScanReport` の JSON / `Debug` に scan 鍵・tweak が現れないことをテストで検査。
- 見直し: 将来「支出」を扱うなら、保存方法を別途設計し人間レビューを通す。

### D-012 `Evidence` は列挙型にし、tweak 方式を今は作らない
フルブロック方式と tweak 方式を同じ型に押し込めない（設計書 §4.2）。現状は `Evidence::FullBlock` のみで `#[non_exhaustive]`。tweak 方式は Phase 4 で最初の実バックエンドと一緒に追加する。先回りのモックは置かない。

### D-013 fixture は本物のブロック構造を持たせる
合成の「ブロックハッシュ文字列」ではなく、コンセンサス直列化したブロック（有効な regtest PoW、マークルルート、BIP141 witness commitment、BIP34 高さ）を使う。Bitcoin Core が返すデータと同じ形なので、Phase 2 のアダプタと `Evidence::FullBlock` を共有できる。
- witness commitment を検証する理由: BIP352 の入力判定は witness（P2WPKH の公開鍵、Taproot の control block）を読むが、ヘッダのマークルルートは witness にコミットしない。
- 検証しないもの: 署名・script・金額・prevout の実在。`SourceCapabilities::consensus_validated = false` として明示。

### D-014 Phase 1 のエンジンは `scan_range`（ステートレス）
永続化・再開・reorg は Phase 3。CLI のコマンド名は設計書どおり `sync` だが、実行時に「stateless, nothing is persisted」と表示する。エンジンはソースを信用せず、バッチの件数・高さの連続・ハッシュ連結を自分でも検査する。tx 単位のエラーは範囲全体の失敗にする。

### D-015 依存の最適化プロファイル
`[profile.dev.package."*"] opt-level = 3`。K_max ベクトルは約 540 万回の点演算を要し、無最適化では数分かかる（上流の slow テストは 124 秒）。

### D-016 BIP34 の高さは自前で読む
`bitcoin::Block::bip34_block_height` は高さ 1〜16（Bitcoin Core が `OP_1`〜`OP_16` で符号化する）を読めない。regtest は必ずここから始まるので `source::coinbase_height` を用意した。script の解析のみ。

## 未決事項

| ID | 事項 | 判断時期 |
|---|---|---|
| O-001 | **このリポジトリのライセンス**。現状 `LICENSE` ファイルも `Cargo.toml` の `license` も置いていない。依存はいずれも MIT / Apache-2.0 / CC0 / BSD 系で、MIT でも Apache-2.0 でも支障はない | 所有者の判断待ち |
| O-002 | 上流 bdk-sp への報告（RESEARCH U-1 K_max、U-2 SegWit v>1、U-5 panic）。issue / PR を出すかどうか | 所有者の判断待ち。未報告 |
| O-003 | Phase 2 で使う Bitcoin Core のバージョン（Oracle v2 と同一ノードで比べるなら 30+）と、その入手方法・署名検証 | Phase 2 開始時 |
| O-004 | prevout の取得方法（`getblock` verbosity 3 / REST `spenttxouts` / undo データ）と、その検証可能性 | Phase 2 |
| O-005 | signet 対応の範囲。HRP は `tsp`。カスタム signet の扱い（D-009 の限界） | Phase 2 以降 |
| O-006 | ラベルの発行・バックアップの扱い。fixture で「未知ラベルが同一 tx の後続出力を隠す」ことを確認済み。復元 UX にどう出すか | Phase 3 |
| O-007 | D-004 のバージョン固定をいつまで続けるか（`bitcoin 0.32.10` への引き上げ等） | Phase 2 でブロックをデコードする前 |
| O-008 | BlindBit Oracle v2 の gRPC スキーマと regtest 実動作（RESEARCH §4 の `UNVERIFIED`） | Phase 4 |
| O-009 | 機密情報検出の pre-commit / CI ツール（設計書 §11）。現状は `.gitignore` とテストによる出力検査のみ | Phase 2 で RPC 認証情報を扱う前 |
