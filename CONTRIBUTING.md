# 贡献指南（Contributing）

感谢你愿意改进 BIGNUM！本项目以 **MIT** 许可证开源，欢迎 issue 与 pull request。

## 行为准则

- 保持友善、就事论事；技术分歧用数据和复现步骤说话。
- 不接受任何形式的骚扰、歧视或人身攻击。

## 开发环境

| 项目 | 要求 |
|---|---|
| 操作系统 | Windows 10/11 x64（当前仅支持 Windows） |
| Rust | 1.99.0（见 `rust-toolchain.toml`；edition 2024 最低要求 1.85） |
| 工具链 | MSVC（rustup 默认 `stable-x86_64-pc-windows-msvc` 即可） |
| WebView2 | Windows 11 自带；Windows 10 需安装 Evergreen Runtime |
| 其他 | 不需要 GMP / CMake / Node.js |

```powershell
git clone https://github.com/lcr12512/Bignum_calc_in_rust.git
cd bignumcalc
cargo run --release      # 开发运行
cargo test               # 引擎单元测试
```

> `vendor/cookie` 是刻意 vendored 的依赖（绕过部分安全软件对构建脚本的拦截），
> 修改依赖前请先阅读 `THIRD_PARTY_NOTICES.md`，不要随手删除该目录。

## 提交前自检

请在提交 PR 前本地跑通以下命令（CI 也会跑同样的检查）：

```powershell
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

## PR 要求

1. 一个 PR 只做一件事，标题用中文或英文均可，建议遵循 `类型: 简述`（如 `fix: 修复 sum 边界溢出`）。
2. 涉及计算语义的改动必须新增或更新 `src/engine.rs` 中的单元测试，并说明数学依据。
3. 涉及 UI 的改动请附截图（亮/暗色各一张更佳），并确认键盘操作、焦点态、`prefers-reduced-motion` 未被破坏。
4. 不要提交 `target/`、`dist/`、exe、zip 等产物（已在 `.gitignore` 中忽略）。
5. 提交即表示你同意以 **MIT** 许可证授权你的贡献（inbound = outbound）。

## 报告问题

- 功能缺陷请使用 **Bug report** 模板，附表达式、期望结果、实际结果与「有效数字」设置。
- 安全问题请勿公开 issue，按 `SECURITY.md` 的流程私下报告。

## 分支与发布

- `main` 为稳定分支，通过 PR 合入。
- 版本遵循语义化版本（SemVer）；打 `vX.Y.Z` 标签会自动触发 Release 工作流。
