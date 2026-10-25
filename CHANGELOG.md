# 更新日志（Changelog）

本项目遵循 [语义化版本](https://semver.org/lang/zh-CN/)；
格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [Unreleased]

## [0.1.0] — 首个开源版本

### 新增

- Rust + [dashu](https://crates.io/crates/dashu) 任意精度计算内核：
  - 精确整数：加、减、乘、取余、阶乘、非负整数指数乘方
  - 高精度：除法（附带精确整数商与余数）、自然/常用/任意底对数、无理数常数、数值导数
  - 符号求和 `sum(通项, 变量, 起点, 终点)`
  - 数值积分 `integ(函数, 变量, 下界, 上界)`：tanh-sinh 双指数求积，支持 `inf` 无穷限
  - 超大阶乘：超过精确路径上限时自动切换到 Stirling 对数阶乘与对数域运算
- tao 0.37 + wry 0.57 + WebView2 桌面外壳：单文件免安装、运行时不联网
- 前端：屏幕键盘/键盘混合输入、有效数字 1–5000、首尾预览（千万位结果不撑爆 WebView）、
  耗时与进度显示、复制全部/导出 .txt、本地历史记录
- 静态 CRT（`+crt-static`），目标机无需安装 VC++ 运行库
- 30 项单元测试（24 引擎 + 6 外壳），覆盖 300 万位乘方、阶乘、Stirling 对数阶乘、
  积分、求和、求导、错误路径与安全边界

### 修复（开源前代码审阅）

- **稳定性**：表达式长度限制 4096 字符、解析深度 256 层 / 求值深度 512 层，
  修复超长或深嵌套表达式导致工作线程栈溢出、进程 abort 的问题
- **数值正确性**：Real 指数溢出不再输出伪数值（改为“结果超出可表示范围”错误）；
  修复对数域减法结果符号反转；修复求和边界的 i64 回绕；修复 `0^-1` 返回 0、
  负数对数域取对数不报错、求和循环变量泄漏、科学计数法指数下溢
- **资源保护**：乘法结果统一遵守 1 亿位上限；求和时间预算逐项检查；
  嵌套积分共享 20 万次求值 / 120 秒预算；对数域量级的项可正常参与求和
- **安全加固**：WebView2 导航白名单、关闭外部文件拖放、IPC 来源校验、CSP 与
  `X-Content-Type-Options: nosniff`；IPC 表达式长度硬校验
- **健壮性**：worker panic 兜底与前端 300 秒无进度看门狗；Mutex 中毒容错；
  历史记录保存完整表达式并对本地数据做 schema 校验
- **工程质量**：修复全部 clippy 告警（3×`approx_constant`、1×`manual_is_multiple_of`），
  `cargo fmt --check` 与 `cargo clippy -- -D warnings` 通过

### 已知限制

- 仅支持 Windows 10/11 x64
- 超大计算不可中途取消
- 除法/对数/导数/积分为近似值（按有效数字），界面明确标注
- 详见 README「已知限制」一节

### 安全

- 无网络通信、不写注册表；用户数据仅存于 `%LOCALAPPDATA%\BIGNUM\`
- 第三方组件与许可证声明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)

[Unreleased]: https://github.com/lcr12512/Bignum_calc_in_rust/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/lcr12512/Bignum_calc_in_rust/releases/tag/v0.1.0
