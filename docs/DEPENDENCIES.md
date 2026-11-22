# DEPENDENCIES — 依存バージョンとライセンス

2026-11-21 時点。`Cargo.lock` を追跡し、CI は `--locked` で実行する。
一覧は `cargo tree --workspace --locked --format "{p} | {l}"` の出力から作成した。

## ツールチェーン

| 項目 | 固定値 | 固定場所 |
|---|---|---|
| Rust | 1.97.0（rustfmt, clippy 込み） | `rust-toolchain.toml` |
| MSRV（宣言） | 1.85.0（上流 `bdk_sp` と同じ。`cargo +1.85.0 check --workspace --all-targets --locked` は成功。**1.85 でのテスト実行は未実施**） | `Cargo.toml` |

## 上流（BIP352 実装）

| crate | バージョン | 入手元 | ライセンス | 用途 |
|---|---|---|---|---|
| `bdk_sp` | 0.2.0 + 未リリース分 | git `bitcoindevkit/bdk-sp` rev `bb373e566fa2a9ee037cb15de2cbe6e679aac7b0`、feature `serde` | MIT OR Apache-2.0（リポジトリの `LICENSE` は MIT） | BIP352 の暗号処理すべて |

- 使用する API と、その限界は `docs/RESEARCH.md` §3.2〜3.3。
- 上流は実験的と明言しており、mainnet 利用を推奨していない。
- 更新するときは: (1) 新 commit で上流の `cargo test` と slow テストを再実行、(2) `docs/RESEARCH.md` §3.3 の差異が解消されたか確認、(3) 解消されていれば `docs/DECISIONS.md` D-005 / D-006 の補正を外す、(4) 本プロジェクトの全テストを通す、(5) 理由を DECISIONS に記録。

## 暗号スタック（上流の `Cargo.lock` と同一バージョンに固定。DECISIONS D-004）

| crate | バージョン | ライセンス |
|---|---|---|
| `bitcoin` | 0.32.7 | CC0-1.0 |
| `secp256k1` | 0.29.1 | CC0-1.0 |
| `secp256k1-sys` | 0.10.1（libsecp256k1 を同梱、C コードは MIT） | CC0-1.0 |
| `bitcoin_hashes` | 0.14.0 | CC0-1.0 |
| `bech32` | 0.11.0 | MIT |
| `bitcoin-io` | 0.1.3 | CC0-1.0 |
| `bitcoin-units` | 0.1.2 | CC0-1.0 |
| `bitcoin-internals` | 0.3.0 | CC0-1.0 |
| `base58ck` | 0.1.0 | CC0-1.0 |
| `hex-conservative` | 0.2.1 | CC0-1.0 |
| `hex_lit` | 0.1.1 | MITNFA（MIT + 虚偽表示禁止条項） |

`cargo update` を無指定で実行すると、これらが `0.32.10x` / `0.29.100` 系に上がる。**意図せず上げないこと。** 上げる場合は DECISIONS O-007 に従う。

## 直接依存（暗号以外）

| crate | バージョン | ライセンス | 使用箇所 |
|---|---|---|---|
| `serde` | 1.0.229 | MIT OR Apache-2.0 | `silent-sync`（モデルと fixture 形式） |
| `serde_json` | 1.0.154 | MIT OR Apache-2.0 | `silent-sync`, `lab-cli` |
| `thiserror` | 2.0.21 | MIT OR Apache-2.0 | `silent-sync`（エラー型） |
| `clap` | 4.6.9 | MIT OR Apache-2.0 | `lab-cli` のみ |
| `minreq` | 2.14.1（`default-features = false`: TLS・プロキシ・JSON なし） | ISC | `silent-sync` の feature `core-rest` / `blindbit-http`（ループバックの HTTP GET のみ。DECISIONS D-020） |
| `rusqlite` | 0.37.0（feature `bundled`: SQLite を同梱ビルド） | MIT | `silent-sync-sqlite` のみ（DECISIONS D-024） |
| `libsqlite3-sys` | 0.35.0（同梱する SQLite 本体はパブリックドメイン） | MIT | `rusqlite` 経由 |

- `silent-sync` は既定の feature では HTTP クライアントにもデータベースにも依存しない（`minreq` は optional、SQLite は別クレート）。
- `lab-cli` は `build.rs` を持つ（依存なし）。ビルドに使ったコンパイラのバージョンとターゲットを、ベンチマーク結果に記録するために埋め込むだけ。

### ウォレットのクレート（`silent-wallet`、`wallet-cli`）

**外部の依存は増やしていない。** `silent-wallet` は `silent-sync`（feature `spend`、`low-level`）、`bdk_sp`（送信側の関数。ワークスペースと同じ固定 rev）、`serde`、`serde_json`、`thiserror` を使う。`wallet-cli` は加えて `silent-sync-sqlite` と `clap`（`lab-cli` と同じ版）。ノードへの送信に使う JSON-RPC（HTTP）と Basic 認証の base64 は、依存を足さずに `silent-wallet/src/rpc.rs` に書いた（暗号ではない。RFC の例でテスト）。`Cargo.lock` の差分はこの 2 クレートの追加だけ。

### 窓口 API（`silent-wallet` の feature `client`。DECISIONS D-052）

| crate | バージョン | ライセンス | 使用箇所 |
|---|---|---|---|
| `bip39` | 2.2.2（厳密に固定。`default-features = false`、feature `std`。英語の単語表のみ、`rand` なし） | CC0-1.0 | シードフレーズ ⇔ エントロピー ⇔ シード（BIP39）。rust-bitcoin のクレートで、ハッシュは既存の `bitcoin_hashes 0.14.0` を使う |
| `getrandom` | 0.2.17 | MIT OR Apache-2.0 | 新しいフレーズのエントロピー（OS の乱数源） |

- 推移的に増えたもの: `unicode-normalization 0.1.26`、`tinyvec 1.13.3`（`bip39` 経由）、`cfg-if 1.0.5`、`libc 0.2.191`、`wasi 0.11.1`（`getrandom` 経由。`wasi` は WASI 向けのビルドでだけ使われる）。いずれも MIT OR Apache-2.0 系（`tinyvec` は Zlib OR Apache-2.0 OR MIT）。
- `wallet-cli` は `silent-wallet` の feature `client` を有効にする（`new-phrase` とフレーズの読み取りのため）。`silent-wallet` を `client` なしで使う場合、これらは入らない。
- 暗号スタック（上の表）の版は変わっていない。`Cargo.lock` の差分は、これらの追加だけ。

## 推移的依存（すべて）

`anstream 1.0.0`, `anstyle 1.0.14`, `anstyle-parse 1.0.0`, `anstyle-query 1.1.5`, `arrayvec 0.7.8`, `cc 1.11.3`, `clap_builder 4.6.7`, `clap_derive 4.6.9`, `clap_lex 1.1.2`, `colorchoice 1.0.5`, `find-msvc-tools 0.1.15`, `heck 0.5.0`, `is_terminal_polyfill 1.70.2`, `itoa 1.0.19`, `proc-macro2 1.0.107`, `quote 1.0.47`, `serde_core 1.0.229`, `serde_derive 1.0.229`, `shlex 2.0.1`, `syn 3.0.10`, `thiserror-impl 2.0.21`, `utf8parse 0.2.2` — いずれも MIT OR Apache-2.0。
`memchr 2.8.3`（Unlicense OR MIT）、`strsim 0.11.1`（MIT）、`zmij 1.0.23`（MIT）、`unicode-ident 1.0.27`（(MIT OR Apache-2.0) AND Unicode-3.0）。
`rusqlite` 経由: `bitflags 2.13.2`, `fallible-iterator 0.3.0`, `fallible-streaming-iterator 0.1.9`, `hashbrown 0.15.5`, `hashlink 0.10.0`, `pkg-config 0.3.34`, `smallvec 1.16.6`, `vcpkg 0.2.15`（いずれも MIT OR Apache-2.0）、`foldhash 0.1.5`（Zlib）。

コピーレフト系のライセンスは含まれない。

## テストデータ

| データ | 入手元 | ライセンス |
|---|---|---|
| `fixtures/bip352/send_and_receive_test_vectors.json` | bitcoin/bips `fdcd8024595bc1e97db73883e716dbb91627c371` | BSD-2-Clause |
| `fixtures/local/regtest_basic_v1.json` | 本リポジトリの生成器 | 本リポジトリのライセンスに従う（MIT OR Apache-2.0。DECISIONS D-038） |

## リポジトリに含めない外部バイナリ（テストとベンチマーク用）

| 対象 | バージョン | 入手方法 | 検証 | ライセンス |
|---|---|---|---|---|
| Bitcoin Core | 32.1（DECISIONS D-019） | `scripts/fetch_bitcoind.sh`（bitcoincore.org の公式配布物） | SHA-256 を固定。`VERIFY_SIGNATURES=1` で、commit を固定した `guix.sigs` の相異なる署名者の署名も検証 | MIT |
| BlindBit Oracle | v2 系、commit `f9ab7fd8bc35467c492324f743ba1b69049a2bb6`（RESEARCH §4） | `scripts/build_blindbit_oracle.sh`（ソースからビルド。Go が必要） | commit を固定してチェックアウト | MIT（上流リポジトリの `LICENSE` による。取得時に再確認すること） |

どちらもリポジトリの外に置く。通常の `cargo test` では使わず、`--ignored` の E2E テストとベンチマークだけが使う。

## 検討したが依存していないもの

| 対象 | 理由 |
|---|---|
| gRPC クライアント（BlindBit の protobuf） | HTTP JSON で足りたため未導入。上流では HTTP が deprecated（DECISIONS O-014） |
| Bitcoin Core の JSON-RPC クライアント | REST だけで足り、認証情報を扱わずに済むため（DECISIONS D-020）。テスト用ノードの操作は `bitcoin-cli` を子プロセスで呼ぶ |
| 非同期ランタイム | 同期処理は逐次で、ブロッキング HTTP で足りる |
| property-based / fuzz テストのライブラリ | 設計書 §11 で初期 MVP では任意。未導入 |
