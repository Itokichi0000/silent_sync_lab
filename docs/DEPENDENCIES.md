# DEPENDENCIES — 依存バージョンとライセンス

2026-11-21 時点。`Cargo.lock` を追跡し、CI は `--locked` で実行する。
一覧は `cargo tree --workspace --locked --format "{p} | {l}"` の出力から作成した。

## ツールチェーン

| 項目 | 固定値 | 固定場所 |
|---|---|---|
| Rust | 1.97.0（rustfmt, clippy 込み） | `rust-toolchain.toml` |
| MSRV（宣言） | 1.85.0（上流 `bdk_sp` と同じ。**1.85 でのビルドは未検証**） | `Cargo.toml` |

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
| `serde` | 1.0.229 | MIT OR Apache-2.0 | `sync-core`（モデルと fixture 形式） |
| `serde_json` | 1.0.154 | MIT OR Apache-2.0 | `sync-core`, `lab-cli` |
| `thiserror` | 2.0.21 | MIT OR Apache-2.0 | `sync-core`（エラー型） |
| `clap` | 4.6.9 | MIT OR Apache-2.0 | `lab-cli` のみ |

## 推移的依存（すべて）

`anstream 1.0.0`, `anstyle 1.0.14`, `anstyle-parse 1.0.0`, `anstyle-query 1.1.5`, `arrayvec 0.7.8`, `cc 1.11.3`, `clap_builder 4.6.7`, `clap_derive 4.6.9`, `clap_lex 1.1.2`, `colorchoice 1.0.5`, `find-msvc-tools 0.1.15`, `heck 0.5.0`, `is_terminal_polyfill 1.70.2`, `itoa 1.0.19`, `proc-macro2 1.0.107`, `quote 1.0.47`, `serde_core 1.0.229`, `serde_derive 1.0.229`, `shlex 2.0.1`, `syn 3.0.10`, `thiserror-impl 2.0.21`, `utf8parse 0.2.2` — いずれも MIT OR Apache-2.0。
`memchr 2.8.3`（Unlicense OR MIT）、`strsim 0.11.1`（MIT）、`zmij 1.0.23`（MIT）、`unicode-ident 1.0.27`（(MIT OR Apache-2.0) AND Unicode-3.0）。

コピーレフト系のライセンスは含まれない。

## テストデータ

| データ | 入手元 | ライセンス |
|---|---|---|
| `fixtures/bip352/send_and_receive_test_vectors.json` | bitcoin/bips `fdcd8024595bc1e97db73883e716dbb91627c371` | BSD-2-Clause |
| `fixtures/local/regtest_basic_v1.json` | 本リポジトリの生成器 | 本リポジトリのライセンスに従う（未定、DECISIONS O-001） |

## まだ依存していないもの（今後のフェーズ）

| 対象 | フェーズ | 現状 |
|---|---|---|
| Bitcoin Core（バイナリ） | 2 | 未導入。バージョン未定（DECISIONS O-003） |
| SQLite バインディング | 3 | 未選定 |
| BlindBit Oracle v2 / gRPC クライアント | 4 | 未導入。前提条件が未確認（RESEARCH §4） |
