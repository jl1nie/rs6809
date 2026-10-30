# Rustの最適化が効く形への変更

これは最初の最適化段階の記録。現行版の再検証は [実装パターンの比較](pattern-validation.md) を参照。

第0版を保持したまま、ゲストの命令・メモリマップを変更せずに描画を約1.47倍高速化した。
変更後もPure Rust・標準ライブラリのみ・unsafeなし。

## 反映した実装

### 頻繁な処理を小さくする

通常のメモリアクセスから、ACIA・SD・マッピング制御などの分岐を別関数へ移した。

```rust
impl Bus for Machine {
    #[inline]
    fn read(&mut self, addr: u16) -> u8 {
        if self.is_io(addr) {
            return self.read_io(addr);
        }
        self.read_memory(addr)
    }
}
```

`read_memory` と `write_memory` は小さい `#[inline]` 関数にし、ROM・MMU・保護の意味は元のまま共有する。
I/O側の `read_io` と `write_io` は `#[cold] #[inline(never)]` とした。
大きなI/O処理まで通常の命令実行へ展開せず、よく使う部分の呼び出しを削減しやすくする構成。
アドレス計算の `reg` / `set_reg` / `index` / `ea` と `Machine::step` にも `#[inline]` を付けた。

RustのCPUはもともと `&mut impl Bus` を使っており、`dyn Bus` による動的ディスパッチではない。
今回の焦点は、コンパイラに関数の中身を見せ、短いアクセス経路を展開・整理しやすくすることだった。

### release設定

```toml
[profile.release]
lto = "thin"
codegen-units = 1
```

LTOとcodegen-unitsはまとめて比較したため、それぞれ単独の寄与はこの結果からは分からない。
`target-cpu=native`、PGO、unsafe、MMUの変換表、命令デコードの作り直しは使用していない。
開発時のdebug設定は変更していない。

## 測定

Ryzen 9 9900X / WSL2 / rustc 1.99.1。
Cソース・ROM・ディスク・入力スクリプトは固定し、起動・コンパイルを除いた描画区間を測定した。
標準出力はパイプ回収。全候補で出力SHA-256、描画490,703,662サイクル、総563,528,763サイクルが一致した。

最初に各候補をウォームアップ後3回ずつ測った：

| 候補 | 描画中央値 |
|---|---:|
| ThinLTO + codegen-units=1だけ | 1.347秒 |
| インライン指定だけ | 1.326秒 |
| 両方 | 1.207秒 |
| 両方 + RAM/I/O分離の試作 | 0.932秒 |

採用時にはROM・MMU・保護の重複実装を共通のメモリアクセス関数へ整理した。
この最終コードと保存済み第0版を改めてウォームアップ1回＋計測7回で比較した：

| 最終比較 | 描画中央値 | MHz相当 |
|---|---:|---:|
| 第0版を再測定 | 1.448394秒 | 338.8 |
| **採用コード** | **0.984152秒** | **498.6** |

処理速度は **1.4717倍**、時間は約32%短縮。
試作値0.932秒は採用コードの値ではない。また、別実行の比較にはホストの負荷・周波数変動が入る。

小さいアクセス関数群を `#[inline(always)]` にした別試作も3回測ったが、中央値1.006秒で更なる改善を確認できなかった。
`.text` も349,424バイトから353,280バイトへ増えたため採用しなかった。
属性を多く、強く付ければ速くなるとは限らない。

採用後のシンボルでは `Machine::step` の独立した関数がなくなり、インライン展開の変化も確認した。
ただし、Busのすべての呼び出しが展開されるわけではなく、`Cpu::index` などは独立した関数としても残る。

## 検証と再現

18件のテストに成功。新たに通常RAM全アドレス、可変I/O位置、アドレス折り返し、ROM切り替え、
MMUバンク・保護・タスク切り替え、固定上位RAM、SDセクタの読み書きを確認した。
既存のNitrOS-9・BASIC09・Cコンパイル・マンデルブローの検証も成功。Clippy警告なし、整形確認済み。

```sh
cargo test --release -- --include-ignored
cargo build --release
python3 scripts/benchmark-emulator.py --workload fixed
```

隔離した試作を作り直す場合：

```sh
python3 scripts/experiment-rust-optimization.py
```

この試作スクリプトは `artifacts/baselines/v0/source.tar.gz` を元に別ディレクトリへ構築する。
作業中の `src/` は変更しない。採用コードの計測結果・ハッシュ・全候補の記録は `docs/rust-optimization.json`、
生データと採用ソースのアーカイブは `artifacts/optimization/rust/` に保存した。

今回の速度向上はマンデルブローでの値。I/O中心のプログラムなどは、別の固定負荷で性能確認する必要がある。
第0版のアーカイブと記録は変更していない。

## 根拠となるRustの仕様

- [Rust Reference: inline / cold](https://doc.rust-lang.org/reference/attributes/codegen.html)：属性はコンパイラへのヒントであり、インライン展開が性能を悪化させる場合もある。
- [Cargo Book: profiles](https://doc.rust-lang.org/cargo/reference/profiles.html)：LTO、codegen-unitsなどの設定と、実測での比較を参照。
