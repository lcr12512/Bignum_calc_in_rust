# vendor/cookie —— 第三方代码修改记录（VENDORED）

本目录是 **`cookie 0.18.2`** 的本地副本，用于规避特定构建环境问题。
它不是本项目原创代码，**不适用本项目的 MIT 许可证**，其许可仍为上游的 **MIT OR Apache-2.0**。

## 1. 上游身份

| 项 | 值 |
|---|---|
| crate | `cookie` |
| 版本 | `0.18.2`（crates.io 当前最新版本，未 yanked） |
| 许可证 | MIT OR Apache-2.0 |
| 上游仓库 | <https://github.com/SergioBenitez/cookie-rs> |
| 上游 VCS 提交 | `1ec8fded5654679482a91c784e7ff2874b0fdeff`（见 `.cargo_vcs_info.json`） |
| 上游 `.crate` SHA256 | `1a373e3602691c3cdea496d2f0ee5935151e6168fe87739483c463db1b2f2f87` |
| 上游 `.crate` 大小 | 47,534 字节 |

## 2. 本项目所做的修改

与上游 0.18.2 逐文件 SHA256 比对后，差异**仅有两处**（另有一个 cargo 解包标记文件）：

| 变化 | 文件 | 具体内容 |
|---|---|---|
| 删除 | `build.rs` | 上游该文件仅 134 字节，内容仅为检测 nightly `doc_cfg` 特性（`version_check::supports_feature("doc_cfg")`），**不影响任何运行时行为** |
| 修改 | `Cargo.toml` 第 21 行 | `build = "build.rs"` → `build = false` |
| 新增 | `.cargo-ok` | 内容 `{"v":1}`，是 cargo 从 registry 缓存解包时留下的标记，**不是上游文件** |

其余 24 个文件（含 `LICENSE-MIT`、`LICENSE-APACHE`、`Cargo.toml.orig`、`src/**`、
`Cargo.lock`、`.cargo_vcs_info.json`、`README.md`、`CHANGELOG.md` 等）与上游**逐字节一致**。

> 说明：`Cargo.toml.orig` 是 crates.io 打包时保留的上游原始清单，其中仍写
> `build = "build.rs"`；实际生效的是本目录中被修改过的 `Cargo.toml`。
> 保留 `Cargo.toml.orig` 有利于溯源，请勿修改。

## 3. 修改原因

`cookie` 的上游 `build.rs` 会在构建期编译并执行一个小程序。在部分国产安全软件
（如 360 主动防御）的启发式拦截策略下，该构建期可执行文件会被拦截，导致
`cargo build` 失败（错误通常表现为构建脚本无法生成/执行）。

本项目通过 `Cargo.toml` 中的 `[patch.crates-io]` 把 `cookie` 指向本目录，
从而在**不改变 cookie 功能与许可**的前提下绕开该问题：

```toml
[patch.crates-io]
cookie = { path = "vendor/cookie" }
```

删除的构建脚本只影响 `doc_cfg`（文档构建），与 cookie 的解析、签名、Cookie Jar 等
运行时功能无关。

## 4. 为什么必须提交本目录

`Cargo.lock` 中 `cookie` 条目**没有 `source` 与 `checksum` 字段**（因为它是 path patch）。
这意味着 `Cargo.lock` 与本目录是绑定的：若删除 `vendor/cookie`，`cargo build`
会直接解析失败。因此 **`vendor/cookie` 必须随仓库提交**，不要加入 `.gitignore`。

## 5. 许可与声明

- 本目录保留上游 `LICENSE-MIT` 与 `LICENSE-APACHE` 原件，请勿删除。
- 本项目选择 **MIT** 分支遵守，同时按 Apache-2.0 §4(b) 的要求在此**声明修改**。
- 在 `THIRD_PARTY_NOTICES.md` 第 5 节中同步记录了本修改。

## 6. 升级 / 重新 vendor 流程

当上游发布新版本，或本机安全软件问题消失后：

1. 从 crates.io 下载新版 `.crate`，解包到临时目录；
2. 与本目录做递归 SHA256 比对，确认差异仍只有 `build.rs` / `Cargo.toml:build` / `.cargo-ok`；
3. 更新 `Cargo.toml` 版本与 `[patch.crates-io]`，`cargo update -p cookie`；
4. 更新本文件中的版本、SHA256、VCS 提交与修改日期；
5. 运行 `cargo test --locked` 与一次 GUI 冒烟测试。

可选：若上游修复或安全软件放行，可整体移除 `[patch.crates-io]` 与 `vendor/cookie`，
改用 crates.io 版本（需相应更新 `Cargo.lock` 并重新验证构建）。

## 7. 修改记录

| 日期 | 修改人 | 说明 |
|---|---|---|
| 2026-10（vendor 时点） | BIGNUM 项目维护者 | 初始 vendor：删除 `build.rs`、`build = false` |
