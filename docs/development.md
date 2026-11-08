# 開発ガイド

## 準備

- Rust は `rust-toolchain.toml` の版（1.97.1）を使います。`rustup` があれば自動で入ります。
- 標準ビルドには FFmpeg の開発用ライブラリと `pkg-config` が必要です。
  - macOS: `brew install ffmpeg pkg-config`
  - Ubuntu: `sudo apt-get install pkg-config clang libavcodec-dev libavformat-dev libavutil-dev libavfilter-dev libavdevice-dev libswscale-dev libswresample-dev`
- macOS 以外で PDF のサムネイルを試すには pdfium の共有ライブラリを置きます（実行ファイルと同じフォルダ、インストール先の `lib/iris`、作業ディレクトリ、システムのライブラリパスの順に探します）。
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

1. ルートの `Cargo.toml` にある `[workspace.package].version` を上げます。各クレートのバージョンはここから継承されます。`cargo check --workspace` で `Cargo.lock` を更新し、両方をコミットします。
2. 更新したバージョンと一致する `v<version>` タグ（例: `v0.4.0`）を `main` 上のコミットに付けて push します。リリース前検証はタグとルートのワークスペースバージョンの一致を確認します。
3. `release.yml` が macOS・Linux・Windows のパッケージを作り、GitHub の Release に添付します。

手元でパッケージを作るスクリプトもあります。出力先は `dist/` です。

| OS | コマンド | できるもの |
|---|---|---|
| macOS | `./scripts/package-macos-app.sh` | pinned FFmpeg/liblzma をビルドした `Iris.app` と ZIP/PKG（[runtime/source/notice](../packaging/macos/ffmpeg-README.md) 同梱）。`IRIS_MACOS_SIGN_IDENTITY` と `IRIS_MACOS_NOTARY_PROFILE` を設定すると署名と公証も行う |
| Linux | `./scripts/package-linux-appdir.sh` | `Iris.AppDir`（`appimagetool` があれば AppImage にできる） |
| Windows | `./scripts/package-windows.ps1` | `Iris.exe` を含む zip（`IRIS_BUILD_VARIANT=minimal`） |

### 公開しないパッケージ監査

`Release` の **Run workflow** で監査機能を含む workflow revision を選択します。
監査対象はその `workflow_dispatch` run の `github.sha` に固定され、全 OS の package job が同じ SHA を checkout します。
独立した revision 指定欄はなく、実行後に監査対象を別の commit へ差し替えることはできません。
手動実行はタグを選んでも `publish` を実行せず、macOS の署名・公証 secret も使いません（ad-hoc 署名）。
read-only GitHub token も渡さないため、手動 run は PDFium の固定サイズ・digest のみ確認し、upstream attestation は未検証と JSON に記録します。
タグ push は従来どおり PDFium attestation を検証します。タグ push の `main` 到達性・バージョン一致チェックと公開ファイル名・チェックサム形式は変わりません。

実行の Actions artifacts に `release-macos` / `release-linux` / `release-windows` と、
別名の `audit-macos` / `audit-linux` / `audit-windows`（JSON）が残ります。
公開ジョブは `release-*` のみ取得します。監査は検証失敗時にも可能な範囲で実行します。
監査追加はリリース保留の解除やライセンス・脆弱性対応の承認ではありません。

- macOS: staged `Iris.app` の実行ファイル・dylib の SHA-256 と `otool` load commands、
  解決できた依存を記録します。製品の pinned FFmpeg の source/signature/configuration と
  署名後 dylib の hash・SDK closure・必須 preview corpus 結果はアプリ内の
  `Contents/Resources/ffmpeg/` と `ffmpeg-runtime.json` に同梱します。
  別の report-only 比較で採取する Homebrew metadata は製品 runtime の証明ではありません。
- Linux: staged Iris と PDFium から `readelf` / `ldd` で辿った依存の SHA-256、
  所有 Debian package の正確な installed/source version、APT origin 情報と copyright を記録します。
  APT 設定は過去のダウンロード元の証明ではありません。bundled PDFium の PROVENANCE、
  VERSION、args.gn、LICENSE と第三者ライセンスも記録します。
- FFmpeg 情報は辿った**実ライブラリ**を別プロセスで読み、その version/configuration/license API
  を問い合わせます。`ffmpeg-next` や別の `ffmpeg` 実行ファイルから推測しません。
  読込み失敗・不足証拠は失敗として残ります。
- Windows: 現行 release の minimal flags、staged executable の SHA-256 と payload files、
  利用可能なら LLVM / Visual Studio ツールによる PE imports を記録します。
  FFmpeg/PDFium は選択されず、動画・PDF preview 対応を主張しません。

各 OS の report は `rustc --version --verbose` の host を使い、
`cargo tree -p iris-ui-shell --target <host> --offline --locked --prefix indent --charset ascii --format '{p}|{f}' -e normal,build`
を実行します（macOS/Linux は defaults、Windows は `--no-default-features --features minimal`）。
product graph、有効 features、target filtering 後の normal/build edges はこの package-rooted tree が正とし、
`cargo metadata --format-version 1 --locked --offline --filter-platform <host> --manifest-path crates/ui-shell/Cargo.toml`
（同じ variant flags）は package/license カタログにのみ使います。Cargo tree の build group から edge kind を読み、
metadata の未評価 target predicates は graph に載せません。
`cargo metadata` 単体は workspace 全体を解決するため per-node features が水増しされる
（dev/bench 由来の image/avif、tokio/test-util、iris-thumb-preview/test-fixtures 等）ので、
resolve の features は記録しません。`iris-ui-shell` を起点とする normal/build closure だけを記録し、
dev-only・未到達の workspace package・他 target の依存は含めません。package ID/source/version、
tree の有効 features、宣言された license/license-file（未指定は null と `missing_metadata`）、
依存 edges を保持します。`dependency_kind` は normal/build/ambiguous（同一 package edge が両方に出た場合）を区別し、
分類を `dependency_kinds` に残します。`proc_macro` は依存先の proc-macro target を表し、
package の `contexts` は runtime/build/proc-macro を区別します。build tool・proc-macro の
normal 依存も build-time として伝播します。同じ package が複数 context に現れることがあります。
これは Cargo tree/metadata の宣言情報であり、license 式の互換性判定、Iris の license の推定、
native code の網羅性の保証ではありません。path source は null（`source_kind=path`）です。

`python scripts/test-release-audit.py` は軽量・offline の回帰検証です。
レポートは `python scripts/release-audit.py <macos|linux|windows> --output <file>` でも生成できます。
既定値は `IRIS_RELEASE_AUDIT_MAX_REPORT_BYTES=2097152`、
`IRIS_RELEASE_AUDIT_MAX_COMMAND_BYTES=32768`、`IRIS_RELEASE_AUDIT_MAX_METADATA_BYTES=8388608`、
`IRIS_RELEASE_AUDIT_MAX_CARGO_PACKAGES=1024`、`IRIS_RELEASE_AUDIT_MAX_NATIVE_FILES=256`、
`IRIS_RELEASE_AUDIT_COMMAND_SECONDS=30` です。workflow 実行では同名の GitHub repository variables
で調整でき、個別実行では `--max-report-bytes`、`--max-command-bytes`、
`--max-metadata-bytes`、`--max-cargo-packages`、`--max-native-files`、`--command-seconds` で上書きできます。すべて正の整数で、report budget は
JSON metadata を保持できる大きさが必要です。抜粋には元ファイル全体の hash と
`truncated`、容量不足には `omitted_records` が付き、欠落は承認と解釈できません。
cargo metadata/tree の stdout/stderr はそれぞれ metadata byte limit で制限し、超過時は producer を停止します。
cargo command 失敗・timeout・byte 超過・不正 JSON/tree 形式は graph `status=absent`、package 上限は `status=partial` と
`truncated=true` です。graph 全体が残り report budget に入らなければ、代わりに欠落理由を記録します
（その記録も入らない場合は `omitted_records`）。report 全体の 2 MiB 既定上限は変わりません。
環境変数一覧や credential は取得せず、runner の workspace/home/temp paths は置換します。
GitHub Actions 外でも workspace root と home は `$WORKSPACE`/`$HOME` に置換し、絶対 path を残しません。
Iris 自体のライセンス、静的リンクされた全 native code、未解決・dlopen-only 依存、vendor の
patch 適用状況やライセンス互換性をこのレポートだけで保証するものではありません。

## CI

| ワークフロー | いつ | 内容 |
|---|---|---|
| `ci.yml` | push・プルリクエスト・週 1 回 | 整形、lint、依存の監査、ビルド、バイナリサイズの上限、テスト、SFTP / SMB の結合テスト、fuzz の短時間実行。Windows は minimal 構成 |
| `release.yml` | `main` 上の version 一致 `v<version>` タグ / 選択した run ref での手動監査 | 3 OS のパッケージ・チェックサムと別 artifact の監査レポート。Release 公開はタグ push のみ |
| `fuzz-long.yml` | 週 1 回 | fuzz を長時間実行 |
