# 開発ガイド

## 準備

- Rust は `rust-toolchain.toml` の版（1.97.1）を使います。`rustup` があれば自動で入ります。
- 標準ビルドには FFmpeg の開発用ライブラリと `pkg-config` が必要です。
  - macOS: `brew install ffmpeg pkg-config`
  - Ubuntu: `sudo apt-get install pkg-config clang libavcodec-dev libavformat-dev libavutil-dev libavfilter-dev libavdevice-dev libswscale-dev libswresample-dev`
- macOS 以外で PDF のサムネイルを試すには pdfium の共有ライブラリを置きます（実行ファイルと同じフォルダ、作業ディレクトリ、システムのライブラリパスの順に探します）。
- FFmpeg を入れずに開発するなら `--no-default-features --features minimal` でビルドします。

## ビルドと実行

```bash
cargo run -p iris-ui-shell
```

```bash
cargo run -p iris-ui-shell -- ~/Pictures
```

フィーチャーの一覧は [README](../README.md#ビルドと実行) にあります。

## テスト

```bash
cargo test --workspace
```

| 種類 | 場所 | 内容 |
|---|---|---|
| 単体テスト | 各クレートの `#[cfg(test)]` | 通常のテスト |
| 画面の結合テスト | `crates/ui-shell/tests/integration_smoke.rs` | 状態遷移と描画結果を JSON のスナップショット（`tests/snapshots/`）と比べる |
| RAW のゴールデンテスト | `crates/thumb-preview/tests/golden.rs` | 各 RAW 形式から埋め込みプレビューを正しく取り出せるか |
| SMB のフレーム | `crates/smb-client/tests/golden_frames.rs` | 固定のパケットを正しく解析できるか |
| ジョブ保存の負荷 | `crates/jobstore/tests/concurrent_500.rs` | 多数のジョブを同時に書き込んでも壊れないか |

画面を変えてスナップショットを更新するときは、`IRIS_SNAPSHOT_UPDATE=1` を付けてテストを実行し、差分を確認してからコミットします。

```bash
IRIS_SNAPSHOT_UPDATE=1 cargo test -p iris-ui-shell --test integration_smoke
```

### テスト用の共通部品

テストの補助は一か所にまとめています。新しいテストでは同じものを書かずにこれを使ってください。

| 場所 | 内容 |
|---|---|
| `thumb-preview` の `test_fixtures`（`test-fixtures` フィーチャー） | 合成した RAW（TIFF・BigTIFF・CR3・RW2・RAF・X3F）、JPEG、範囲読み込みを記録するメモリ上のストレージ |
| `smb-client` の `test_support`（`test-support` フィーチャー） | SMB の応答パケットと各コマンドの応答本体、16 進変換 |
| `ui-shell` の `test_support` | 一覧項目とオブジェクト参照 |

ベンチマークや結合テストからは、`[dev-dependencies]` でフィーチャーを有効にして使います。

### SFTP / SMB の結合テスト

Docker で SFTP サーバーと Samba を立ち上げ、実際に転送します（数分かかります）。

```bash
./scripts/perf-transfer-containers.sh
```

SMB のプロトコル版や署名・暗号化の有無は環境変数（`IRIS_PERF_SMB_MIN_PROTOCOL` など）で切り替えます。CI では SMB 3.1.1（暗号化）、3.0（AES-CCM）、2.1（署名）の 3 通りを流しています。

### fuzz

nightly と `cargo-fuzz` が必要です。対象は RAW・画像の判別・SMB のパケット・設定ファイル・マウント一覧・検索の解析です。

```bash
FUZZ_RUNS=1000 ./scripts/fuzz-all.sh
```

```bash
./scripts/fuzz-target.sh raw_tiff_parse
```

fuzz 用のクレートは各クレートの `fuzz/` にあり、ワークスペースとは別に依存を固定しています（`fuzz/Cargo.lock`）。

### ベンチマーク

`criterion` を使います。

```bash
cargo bench -p iris-thumb-preview
```

ほかに `backends`（ローカルの一覧）、`search`（10 万件の走査）、`transfer`（クライアント経由のコピー）、`ui-shell`（並べ替えと描画）にもあります。

## コードの決まり

CI で次のすべてを確かめます。手元でも同じコマンドで確認できます。

```bash
cargo fmt --all --check
```

```bash
cargo clippy --workspace --all-targets --no-deps -- -D warnings
```

```bash
cargo clippy --workspace --lib --no-deps -- -D warnings -D clippy::unwrap_used
```

```bash
cargo clippy -p iris-ui-shell --no-default-features --features minimal --all-targets --no-deps -- -D warnings
```

```bash
cargo machete
```

```bash
./scripts/audit-dependencies.sh
```

- 警告を `#[allow(...)]` で黙らせません。CI はこの属性を見つけると失敗します。
- ライブラリのコードでは `unwrap()` を使いません（テストは使えます）。
- `unsafe` は OS の API を呼ぶ一部のファイル（`ci.yml` の一覧）だけに置き、総数は 73 を上限にしています。
- 使っていない依存クレートは残しません。
- ソースコードのコメントは英語で書きます。名前を言い直すだけのコメントは書かず、理由や制約など、読んでも分からないことだけを書きます。
- 古い形式のデータを読むための互換処理は書きません。形式を変えるときは版を上げ、古いデータは作り直します（[保存するデータ](architecture.md#保存するデータ)）。
- 同じ処理を二度書きません。既にある部品（ファイル種別の判定 `MediaFormat`、ストレージの既定動作、テスト用の共通部品など）を使います。
- コミットメッセージは英語の 1 行です。

## 依存クレートの更新

Dependabot が毎週、更新のプルリクエストを作ります。手元でまとめて最新にするときは次のようにします（`cargo-edit` が必要です）。

```bash
cargo upgrade --incompatible
```

```bash
cargo update
```

fuzz 用のクレートは各 `fuzz/` ディレクトリで同じ操作をします。

## リリース

1. `crates/ui-shell/Cargo.toml` の `version` を上げてコミットします。
2. `v<version>` のタグ（例: `v0.4.0`）を push します。
3. `release.yml` が macOS・Linux・Windows のパッケージを作り、GitHub の Release に添付します。

手元でパッケージを作るスクリプトもあります。出力先は `dist/` です。

| OS | コマンド | できるもの |
|---|---|---|
| macOS | `./scripts/package-macos-app.sh` | `Iris.app` と zip。`IRIS_MACOS_SIGN_IDENTITY` と `IRIS_MACOS_NOTARY_PROFILE` を設定すると署名と公証も行う |
| Linux | `./scripts/package-linux-appdir.sh` | `Iris.AppDir`（`appimagetool` があれば AppImage にできる） |
| Windows | `./scripts/package-windows.ps1` | `Iris.exe` を含む zip（`IRIS_BUILD_VARIANT=minimal`） |

## CI

| ワークフロー | いつ | 内容 |
|---|---|---|
| `ci.yml` | push・プルリクエスト・週 1 回 | 整形、lint、依存の監査、ビルド、バイナリサイズの上限、テスト、SFTP / SMB の結合テスト、fuzz の短時間実行。Windows は minimal 構成 |
| `release.yml` | `main` 上の version 一致 `v<version>` タグ | 3 つの OS のアーカイブ・インストーラと SHA-256 チェックサムを検証して Release に添付 |
| `fuzz-long.yml` | 週 1 回 | fuzz を長時間実行 |
