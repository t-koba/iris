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

### 日時依存の方針

日付トークンとカレンダー、EXIF の日時、タイムスタンプ整形には `chrono` 0.4.45 を使う。RUSTSEC-2026-0543 は未メンテナンス状態の通知であり、現時点で欠陥や CVE は報告されていない。`jiff` は 1.0.1 で、`jiff-chrono-conversions` 0.2.0（MIT）は `jiff-10` を既定で有効にし、`jiff` 1.x への変換を提供する（docs.rs の crate ドキュメントで確認）。ただしこの conversions crate は 2026-11-16 公開で、固定ツールチェーンとの MSRV/feature 互換性と、既存の日付フィルタを含む 11 箇所の呼び出しサイトの挙動等価性は未検証である。そのため依存グラフは変更しない。リリース後に、11 箇所の呼び出しサイトについて依存リゾルバの記録、オフラインビルド、ゴールデンテストによる挙動確認を行い、移行の可否を評価する。再検討する条件は、`chrono` に実害のある advisory が出たとき、または上記の評価が完了したときとする。

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

1. ルートの `Cargo.toml` にある `[workspace.package].version` を上げます。各クレートのバージョンはここから継承され、Windows 実行ファイルの VERSIONINFO とインストーラーのバージョンもこの値から生成されます。`cargo check --workspace` で `Cargo.lock` を更新し、両方をコミットします。
2. 更新したバージョンと一致する `v<version>` タグ（例: `v0.4.0`）を `main` 上のコミットに付けて push します。リリース前検証はタグとルートのワークスペースバージョンの一致を確認します。
3. `release.yml` が 3 OS のパッケージを作り、GitHub の Release に添付します。macOS/Linux は各 stage の実 bytes から native-evidence sidecar を生成してから audit と policy を実行します。Windows は検証用 reusable workflow が同じ SHA で作った exact-SHA product stage を取得して packages に使います。

手元でパッケージを作るスクリプトもあります。出力先は `dist/` です。

| OS | コマンド | できるもの |
|---|---|---|
| macOS | `./scripts/package-macos-app.sh` | pinned FFmpeg/liblzma をビルドした `Iris.app` と ZIP/PKG（[runtime/source/notice](../packaging/macos/ffmpeg-README.md) 同梱）。`IRIS_MACOS_SIGN_IDENTITY` と `IRIS_MACOS_NOTARY_PROFILE` でアプリを、`IRIS_MACOS_INSTALLER_IDENTITY` と `IRIS_MACOS_INSTALLER_NOTARY_PROFILE` で PKG を署名・公証する |
| Linux | `./scripts/package-linux-appdir.sh` | `Iris.AppDir`（`appimagetool` があれば AppImage にできる） |
| Windows | `./scripts/package-windows.ps1` | ビルドせず、検証済みの product stage から `Iris.exe` を含む zip を作成。`IRIS_PRODUCT_STAGE` / `IRIS_PRODUCT_INVENTORY` / `IRIS_PRODUCT_ZIP` / `IRIS_RELEASE_OUTPUT_DIR` が必須。リリース workflow は検証済みの asset をダウンロードして同じスクリプトの成果物をそのまま stage する |

macOS の外側の `Iris-macos.pkg` は `IRIS_MACOS_INSTALLER_IDENTITY` に Developer ID Installer を設定したときだけ `productsign` で署名し、`IRIS_MACOS_INSTALLER_NOTARY_PROFILE`（未設定なら `IRIS_MACOS_NOTARY_PROFILE`）で公証・staple します。未設定なら PKG は署名なし・未公証のままです。`scripts/macos-installer-trust.py verify` は macOS 上で `pkgutil --check-signature`、`xcrun stapler validate`、`spctl -a -vvv -t install` を実行し、証明書名・keychain profile・パスを除いた `audit/macos-installer-trust.json` を残します。公開ジョブは `release-policy.py` の直後に `check` でダウンロード済み PKG のサイズ・SHA-256 と証拠を照合し、検証済みインストーラの信頼がなければ Release を公開しません。

タグ push の `package-macos` は `scripts/macos-signing-provision.py` で一時 keychain を作り、base64 の `.p12` を import してから `notarytool store-credentials` でアプリ用と installer 用の profile を保存します。operator secret は `IRIS_MACOS_SIGNING_P12_BASE64`、`IRIS_MACOS_SIGNING_P12_PASSWORD`、`IRIS_MACOS_KEYCHAIN_PASSWORD`、`IRIS_MACOS_APPLE_ID`、`IRIS_MACOS_TEAM_ID`、`IRIS_MACOS_APP_PASSWORD` です。値は repo・ログ・証拠に書き込まず、2 つの profile 名が同じでも構いません。不足していればタグのパッケージングは失敗し、trust gate は緩められません。`workflow_dispatch` は署名なしのままです。

Windows のリリースでは、ZIP とライセンス関連の検証に加えて、インストーラの smoke で
`Iris-windows-setup.exe` を空のディレクトリにインストールし、インストール済みファイルを export 済み
`product-inventory.json` と名前・サイズ・SHA-256 で照合します（`scripts/windows-installed-smoke.py`）。
Inno Setup が生成する `unins000.exe` / `unins000.dat` / `unins000.msg` のみをインストーラ管理の
エントリとして別枠で記録し、それ以外のファイルやディレクトリは失敗にします。照合の前に export 済み
`release-package-result.json` の `installer_sha256` と実際の setup の SHA-256 を照合し、結果は
`audit/windows-installer-smoke.json` に残します。この workflow にインストール済み preview の
smoke はないため、同じインストールはアンインストールまでの一生命周期のままです
（preview の smoke は検証用 reusable workflow の stage 上で実行します）。

### 公開しないパッケージ監査

`Release` の **Run workflow** で監査機能を含む workflow revision を選択します。
監査対象はその `workflow_dispatch` run の `github.sha` に固定され、全 OS の package job が同じ SHA を checkout します。
独立した revision 指定欄はなく、実行後に監査対象を別の commit へ差し替えることはできません。
手動実行はタグを選んでも `publish` を実行せず、macOS の署名・公証 secret も使いません（ad-hoc 署名）。
read-only GitHub token も渡さないため、手動 run は PDFium の固定サイズ・digest のみ確認し、upstream attestation は未検証と JSON に記録します。
タグ push は PDFium `chromium/8138` の `pdfium-attestation.json` を取得し、18,267 bytes と
SHA-256 `7526f4b934ab498d6206ac7f3d78dd4ce8e0f7d3175227ab3f4ecc255d950ad7` の一致を必須とします。
`gh attestation verify --bundle` は固定 archive の subject、upstream repository、signer workflow
`github.com/bblanchon/pdfium-binaries/.github/workflows/build-all.yml`、signer/source digest
`0eb94b28c2252d2baae3dfe15dc6b237131d8cd8`、source ref `refs/heads/master` を検証します。
失敗時は stage せず終了し、取得した bundle は一時 directory とともに削除します。
手動 run は attestation の取得も `gh` の呼び出しも行わず、bundle digest/size は JSON の `null`、
status は `not_attempted` です。タグ push の `main` 到達性・バージョン一致チェックと公開ファイル名・チェックサム形式は変わりません。

実行の Actions artifacts に `release-macos` / `release-linux` / `release-windows` と、
別名の `audit-macos` / `audit-linux` / `audit-windows`（JSON）が残ります。
公開ジョブは `release-*` と各 OS の audit artifact を取得して照合します。監査は検証失敗時にも可能な範囲で実行します。
監査追加はリリース公開やライセンス・脆弱性対応の承認ではありません。

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
- Windows: リリース成果物の product variant、staged executable の SHA-256 と payload files、
  利用可能なら LLVM / Visual Studio ツールによる PE imports を記録します。
  FFmpeg/PDFium は検証済み exact-SHA product stage 由来で、動画・PDF preview はその stage の evidence に基づきます。

各 OS の report は `rustc --version --verbose` の host を使い、
`cargo tree -p iris-ui-shell --target <host> --offline --locked --prefix indent --charset ascii --format '{p}|{f}' -e normal,build`
を実行します（3 OS とも Cargo defaults の product graph です）。
product graph、有効 features、target filtering 後の normal/build edges はこの package-rooted tree が正とし、
`cargo metadata --format-version 1 --locked --offline --filter-platform <host> --manifest-path crates/ui-shell/Cargo.toml`
（同じ Cargo defaults）は package/license カタログにのみ使います。Cargo tree の build group から edge kind を読み、
metadata の未評価 target predicates は graph に載せません。
`cargo metadata` 単体は workspace 全体を解決するため per-node features が水増しされる
（dev/bench 由来の image/avif、tokio/test-util、iris-thumb-preview/test-fixtures 等）ので、
resolve の features は記録しません。`iris-ui-shell` を起点とする normal/build closure だけを記録し、
dev-only・未到達の workspace package・他 target の依存は含めません。package ID/source/version、
tree の有効 features は同じ package ID の全出現から union して保持します（runtime/build context ごとの feature set 自体は個別に出しません）。宣言された license/license-file（未指定は null と `missing_metadata`）、
依存 edges を保持します。`dependency_kind` は normal/build/ambiguous（同一 package edge が両方に出た場合）を区別し、
分類を `dependency_kinds` に残します。`proc_macro` は依存先の proc-macro target を表し、
package の `contexts` は runtime/build/proc-macro を区別します。build tool・proc-macro の
normal 依存も build-time として伝播します。同じ package が複数 context に現れることがあります。
これは Cargo tree/metadata の宣言情報であり、license 式の互換性判定、Iris の license の推定、
native code の網羅性の保証ではありません。path source は null（`source_kind=path`）です。

`python scripts/test-release-audit.py` と `python scripts/test-release-policy.py` は軽量・offline の回帰検証です。
レポートは `python scripts/release-audit.py <macos|linux|windows> --output <file>` でも生成できます。
既定値は `IRIS_RELEASE_AUDIT_MAX_REPORT_BYTES=2097152`、
`IRIS_RELEASE_AUDIT_MAX_COMMAND_BYTES=32768`、`IRIS_RELEASE_AUDIT_MAX_METADATA_BYTES=8388608`、
`IRIS_RELEASE_AUDIT_MAX_CARGO_PACKAGES=1024`、`IRIS_RELEASE_AUDIT_MAX_NATIVE_FILES=256`、
`IRIS_RELEASE_AUDIT_MAX_NATIVE_COMPONENTS=4096`、
`IRIS_RELEASE_AUDIT_COMMAND_SECONDS=30` と policy JSON の byte limit
`IRIS_RELEASE_POLICY_MAX_BYTES=65536` です。すべて同名の GitHub repository variables
で調整でき、release の全 audit job に適用されます。個別実行では `--max-report-bytes`、`--max-command-bytes`、
`--max-metadata-bytes`、`--max-cargo-packages`、`--max-native-files`、`--max-native-components`、`--command-seconds`、
`--max-policy-bytes` で上書きできます。すべて正の整数で、report budget は
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

### Windows ネイティブプレビュー検証（非公開）

`CI` の手動 dispatch で `windows-ffmpeg-validation / validate` を呼ぶと、
`workflow_call` 専用の `Windows FFmpeg native validation` が `windows-2022` runner で
呼び出し元と同じ commit（exact SHA）だけを checkout して実行します。
署名検証済み FFmpeg 8.1.3 source と pinned vcpkg overlay から product をビルドし、
PE import closure、PDFium の固定 pin/`gh attestation verify` と staging、
インストール済み preview の video/PDF/RAW smoke を検査して JSON evidence に残します。
permission は read-only で、通常の dispatch では JSON evidence だけを upload します。
`export_release_assets: true` を指定した release workflow からの呼び出しでは、検証済みの
zip・インストーラー・package result・product inventory だけを gated に upload し、
release 側で SHA-256 と収録ファイルを再検証してから stage します。
同じ gated 経路で、PE import closure・validation inputs・product inventory から作った
reviewed な native evidence（JSON のみ、manifest と入力/asset の hash）を別 artifact として
export します。release 側は checksum sidecar を含む staged bytes と export 済み bytes を
照合して manifest を seal し、audit の `--native-evidence` に渡します。
manifest が欠落・不正な場合は `--require-native-evidence` が audit を失敗させます。
これは evidence の受け渡しであり、法的承認や release readiness の主張ではありません。

### Release policy gate（法的承認ではない）

`scripts/release-policy.json` は監査の収集機構とは別の保守的な公開ポリシーです。
タグ push 専用の `publish` job は同一 run の `audit-macos` / `audit-linux` / `audit-windows`
を個別に取得し、次の offline validator が成功してから Release を公開します。
package job がレポートを upload するだけでは公開を許可しません。

```sh
python3 scripts/test-release-policy.py
python3 scripts/release-policy.py audit/macos/macos.json audit/linux/linux.json audit/windows/windows.json
```

出力は必ず **policy check only**（legal/compliance approval ではない）です。
3 platform の schema、欠落・切り詰め、selected Cargo graph の完全性・edge/context の整合性を検査します。
Iris workspace の manifest と正規化 package ID が一致する path package は第三者の判定から除外し、
Iris の license を仮定しません。registry/git の runtime package（runtime+build を含む）は
宣言 SPDX 式を評価します。`WITH`、括弧、`AND`、`OR` の順で結合し、`AND` は全項、`OR` は
有効な選択肢の少なくとも一つがポリシーを満たす必要があります。例えば `MIT OR GPL-3.0-only` は
MIT を選べますが、GPL 単独や `MIT AND GPL-3.0-only` は不可です。全 branch を構文検査するため、
未知 ID・未対応 exception・旧 slash 表記・欠落 metadata は OR で隠せません。
build/proc-macro 専用 package の license は runtime 互換性の判定対象外です。

LGPL は、動的リンク/dlopen の native shared library に限り、external attestation なしで受理します。
必須なのは schema 2 の完全な native closure、source/notice/relinking の全 evidence、
hash と結び付いた既知の license 名、禁止 FFmpeg flags の不在です。外部の法的承認は要求されず、提供もされません。
`lgpl_legal_approvals` は既定で空で、Cargo の LGPL crate と static/embedded の native LGPL を
scope 付きで止めるために残しています（現行 gate が参照する `domain` は `cargo` / `native`）。
追加する場合は `lgpl_legal_approvals` に `subject`（完全 Cargo package ID または native component の
`<artifact-path>#sha256=<digest>#component=<id>`）、`license`（review 対象 ID）、`review`（法的 review の参照）を指定します。
これは明示的な legal-review approval とその参照を記録する欄であり、このツールが法的承認を与えるものではありません。
FFmpeg は exact library の hash と probe の path を結び付け、全 license/configuration pair を検査します。
さらに probe された library と出荷される全 `native-file` は closure の component に結び付いていなければならず、
probe された LGPL library は review license/disposition の component に結び付き、完全な source/notice/relinking
obligation が検証されます。component を落としたり、allowed license に付け替えたり、未掲載の native file を
追加した report は受理されません。
product の Cargo flags、必要な native component と許可 license 名は JSON で定義し、GPL/version3/nonfree flags、
未知・欠落の native evidence、動的リンクが明確でない LGPL（static/embedded）は公開を阻止します。
`ffmpeg-next` の license や別の `ffmpeg` executable では代用できません。
Linux PDFium は `provenance-check.attestation_verified == true` が必須です。
この値は staged `PROVENANCE.json` の archive pin、attestation bundle digest/size、signer/source identity、
subject と実際の staged library hash が一致する場合に限ります。audit は記録を検査するもので、
暗号学的検証は staging 時の `gh` が担当します。pin は fetch と audit で一致することを offline test で検査します。
staged `PROVENANCE.json` schema 2 は、tag-only 検証で実際に使った `gh` の X.Y.Z と実行ファイル SHA-256 も記録し、
report-only の場合は両値を `null` にします。audit はこの verifier identity を表示しますが、それだけで TUF trust root の内容や法的承認を証明しません。
TUF bootstrap の挙動は、記録された verifier binary に対応する upstream source で別途確認する必要があります。
Windows も build と graph の `product` variant が必須で、公開する zip・インストーラーは検証済み exact-SHA product stage 由来でなければなりません。
この gate は package 内容・build flags を変更しません。

#### Native closure evidence（audit schema 2）

collector は audit schema 2 を生成します。`--native-evidence <sidecar.json>` で別途作成・review した
構造化 evidence を渡すと、staged bytes を再 hash した inventory と照合して `native-closure` record に
格納します。sidecar と `--output` は `dist/<platform>` の外に置き、検査中に stage を変更しないでください。
macOS/Linux はリリース stage の後、`scripts/posix-native-evidence.py generate` が
`packaging/native-evidence/{macos,linux}-mapping.json` の review 済み rule と
`packaging/{linux,macos}` の pin を使って、`audit/native-evidence/{macos,linux}.json`（sidecar）と
review report を生成します。producer は stage 内の全 file を再 hash し、
`ldd` / `otool` で見つかった stage 外の native dependency も実 bytes に束ねます。
どの staged file にも一致する rule がなければ、また native file に review 済み license component が
なければ Sidecar を書かずに失敗します。FFmpeg は review disposition の LGPL component として
notice/source/relinking の義務を伴い、OS 提供ライブラリは review 済み mapping に載った
license bucket だけを使います（未掲載なら拒否）。macOS では pinned な static liblzma を
0BSD の allowed component として追加します。FFmpeg の macOS dylib は exported-symbols list で
liblzma シンボルを隠すため `nm -gU` では含有を証明できません。代わりに review 済み pin の
source hash と staged `liblzma-build.json` の static archive hash を、全 libav*/libsw* dylib の
staged bytes hash に束ねた build provenance を evidence とします。runtime report の
`static_libraries` は全 dylib を網羅し、各 entry が staged bytes/manifest と一致し、
動的 liblzma dependency が無いことを検証します。pin/manifest の hash が一致しない場合、
report が欠ける場合、static component が 1 つも無い場合は sidecar を書きません。
sidecar は `dist/<platform>` の外に置き、
audit は `--native-evidence audit/native-evidence/<platform>.json --require-native-evidence` で
これを要求します。未知・改ざん・欠落は producer と audit の両方で fail closed です。
Windows は checked-in path を使わず、validation workflow が gated に export した
`windows-native-evidence`（JSON のみ）を release 側で staged/exported bytes に seal してから
`--native-evidence` に渡します。audit が `native-closure` record を作れない場合は失敗し、
`missing` のまま通過することはありません。
prepare は reviewed product inventory のすべての `.dll`/`.exe` を PE import closure と照合し、
review 済み license component のない native file は文件名を添えて拒否します。未知の binary に
license を推測することはありません。
旧 audit schema 1、sidecar の欠落、`complete: true` のような宣言だけでは closure 要件を満たしません。
この入力は package・runtime composition を変更せず、既存の公開 policy gate を迂回しません。

sidecar の schema は独立した整数 `1` で、top-level は `schema`, `artifacts`, `components` のみです。
未知 field/schema、重複 JSON key、重複 identity、欠落、曖昧な値は拒否します。
manifest の reference は `{path}` で指定できます。staged/native artifact と supporting staged evidence の
path は `dist/<platform>` からの exact relative path（`/` 区切り、例: `Iris.AppDir/usr/bin/Iris`）です。
`..`、`.`、重複 `/`、backslash、drive prefix は不可で、basename/glob による検索はしません。
stage 外の native dependency は収集済み `native-file` の exact 正規化 path を指定します。
supporting evidence は staged file のほか、collector が既に hash/size を記録した `file` の exact path も参照できます
（例: `/usr/share/doc/libgcc-s1/copyright`）。manifest の path を使って任意の filesystem file を読むことはありません。
collector は実 bytes の SHA-256 と size を inventory に保存し、各 reference を正規化済み絶対 path と
SHA-256 に展開して report に保持します。事前に build artifact の hash を知る必要はありません。
`{path, sha256}`（lowercase SHA-256）で pin することもでき、その場合は実 digest との一致が必須です。
policy validator は path-only 入力を受理せず、report 内の hash と size の整合性を引き続き検査します。
外部 URL や license filename だけを supporting evidence として渡すことはできません。

- collector の `staged-file` は `dist/<platform>` の全 file（最終 archive/installer と supporting docs を含む）、
  `native-file` は既存 native scan が読んだ実行 file/library です。native file は通常の `file` hash とも一致が必要です。
  `artifacts` はこの和集合を過不足なく一度ずつ列挙し、各項目は `file` reference と `coverage` を持ちます。
- `coverage` は `static`, `embedded`, `dynamic`, `dlopen` の全四区分です。各区分は
  `{components: [component-id, ...], evidence: [reference, ...]}`。
  link map、embedded/vendor inventory、loader/import inventory、dlopen/plugin manifest 等を stage に保存し、
  その digest を指定します。空の component list にも「該当依存なし」を裏付ける evidence が必要です。
  archive や docs のような非 native artifact も明示的に分類し、空 list の根拠を残します。
- `components` の各 record は `id`, `artifact`, `mode`, `name`, `source`, `modifications`, `license`,
  `obligations` のみです。id は report 内で一意、`artifact` は exact artifact reference、`mode` は上記四区分の一つ。
  coverage と component record は双方向に一致させます。同一 artifact/mode/name の重複も不可です。
  dynamic と dlopen-only の依存を静的 code の記録や Cargo graph で代用できません。
- `source` は `{origin, revision, evidence}`。正確な source の由来・revision と、それを裏付ける inventoried evidence を指定します。
  `modifications` は `{status, evidence}`。status は `unmodified` または `modified` のみで、
  前者にも source 比較の根拠、後者には vendor patch set/変更内容の evidence が必要です。unknown は不可です。
- `license` は `{id, disposition, evidence}`。id は policy の単一 license ID、または
  `native_license_exceptions` に完全一致する `WITH` 式です。一般の SPDX 複合式は受理しません。
  disposition は `allowed` または `review` で、対応する policy list または native 専用例外の scope と一致が必要です。
  唯一の例外は Linux の OS 提供 `libgcc_s.so.1`、`GPL-3.0-or-later WITH GCC-exception-3.1`、
  disposition `allowed` です。component name と artifact の basename が完全一致し、mode は dynamic/dlopen、
  `artifact_scope: external`（exact `native-file` hash と一致し、`staged-file` には存在しない）が必須です。
  collector の `dpkg-query -S` がその exact path（`/usr` merge の別表記を含む）の owner を確認した
  Linux `system-dependency` record も必要です。record の path/hash と `system_package: libgcc-s1` が一致し、
  raw `owner` も同じ package（任意の `:architecture` 付き）であることを照合します。package identity は architecture suffix
  のみを除いた完全一致で、filename から owner を推測しません。Iris はこの runtime を同梱しません。
  staged/bundled copy はこの例外の対象外です。GCC Runtime Library Exception の適用根拠、distro package の
  source/revision・変更状況、license と各 obligation の根拠は引き続き hash-bound inventoried evidence で必要です。
  例外は evidence の真実性・法的十分性の承認ではありません。
  GPL 単独、別の exception、他 component/platform/mode、Cargo license にはこの許可は適用しません。
  それ以外の GPL/AGPL・nonfree・未知 license は通りません。LGPL は dynamic/dlopen の native component なら
  artifact digest と component id に結び付いた source/notice/relinking evidence と完全な closure で受理され、
  外部 review は不要です。static/embedded の native LGPL と Cargo LGPL は scope 付き `native` / `cargo` review が
  必要で、相互に代用できません。FFmpeg の exact-library/probe 検査も別途必要です。
- `obligations` は `notice`, `source`, `relinking` の全三項目で、それぞれ `{status, evidence}`。
  status は `provided` または `not-applicable` のみ。非該当にも review 済みの根拠文書が必要です。
  `native_required_obligations` の既定 policy は allowed に notice、review に notice/source/relinking の
  `provided` を要求します。OS runtime 例外だけは scoped `required_obligations: []` で上書きします。
  Iris が library を配布・変更せず OS package が別途提供するため、Iris の配布物としての notice/source/relinking は
  `not-applicable` と記録できます。それでも三項目すべての明示的な status と、外部 OS 提供・非同梱を説明する
  review 済みの根拠文書の参照は必須です。他 component の義務は緩和しません。
  `provided` の source archive、notice、relink 用 object/instructions 等の実際の提供物は stage して参照します。
  すべての evidence list は非空で、重複なく、非空の `staged-file` または収集済み `file` の exact digest に結び付きます。

sidecar の読込は report byte limit、component 数は `--max-native-components` で制限します。
`--max-native-files` は既存 native scan に加え stage の走査 entry 数（directory を含む）と
artifact 和集合の数も制限します。stage 内への file symlink は hash 検査できますが、directory symlink、
stage 外への symlink、特殊 file は未対応として blocked です。超過・読込失敗・切り詰め・omission も通りません。
この共通 schema 検証は collector と validator で共有され、collector は入力側の自己申告 hash を inventory として採用しません。
validator は report 内の exact inventory digest と evidence reference を照合します。report は staged bytes を読んだ
信頼できる collector から取得する必要があり、この gate は署名・証明書や任意の report の真実性を検証する仕組みではありません。

**PASS は構造・digest binding と設定した policy の検査成功だけで、法的承認ではありません。**
文書の内容が完全・真正であること、source/notice/relinking が法的に十分であること、
upstream の権限、Iris 自体の license、配布全体の互換性を推定しません。
実際の evidence の収集・内容 review と必要な法的判断は別途必要です。
synthetic positive regression は到達可能な PASS を確認するだけで、production の license evidence や approval ではありません。


### Paired preview parity 計測（release readiness の主張ではない）

`scripts/parity_source_compare.py` は baseline/candidate を (B/C, C/B)×4 の順に 8 round 交互に測定し、各 round で corpus と J2K の入力・出力検証をやり直します。片側 3 サンプル × 8 round = 24 サンプルを時刻順に残し、各 timing に 8 個の block median（round_medians_ns）を添えます。paired record は round ごとの candidate−baseline 差（round_deltas_ns）と、隣り合う逆順ペアの和 4 件（balanced_delta_sums_ns = [d0+d1, d2+d3, d4+d5, d6+d7]）を持ち、pooled median 差は記録のみで失敗させません。`scripts/release-parity.py` は各 block median を対応する 3 サンプル片から再計算して round_deltas_ns と balanced 和を検証し、4 個の drift-free 推定（和/2）の平均 effect_ns と不偏標準偏差（n−1=3）から t = effect/(sd/2) を求め、effect > 0 かつ t >= t_critical_df3（df=3 の片側生存率 P[T3 >= t] <= 0.05/18、約 7.184868893978111）のときだけ失敗します。大きさが効くため 10% 程度の中央値劣化は数 round のばらつきがあっても検出し、単調な runner drift は逆順ペア和で相殺されるため変化のない path を noise で失敗させません。この判定は same-source の preview diagnostic の比較であり、release readiness や絶対性能を保証しません。

## CI

| ワークフロー | いつ | 内容 |
|---|---|---|
| `ci.yml` | push・プルリクエスト・週 1 回 | 整形、lint、依存の監査、ビルド、バイナリサイズの上限、テスト、SFTP / SMB の結合テスト、fuzz の短時間実行。CI の Windows ビルドは minimal 構成だが、リリースは検証済み product stage を使用 |
| `release.yml` | `main` 上の version 一致 `v<version>` タグ / 選択した run ref での手動監査 | 3 OS のパッケージ・チェックサムと別 artifact の監査レポート。Windows は検証用 reusable workflow の成果物を検証して stage し、インストール済みペイロードを product inventory と照合。Release 公開はタグ push のみ |
| `fuzz-long.yml` | 週 1 回 | fuzz を長時間実行 |

`ci.yml` の `workflow-contracts` は、checksum を検証した固定版 actionlint 1.7.12 で `.github/workflows/` を検査します。指摘は JSON Lines にして `.github/actionlint-baseline.jsonl` と並べ替えて比較し、追加・解消のどちらも差分として失敗します。
