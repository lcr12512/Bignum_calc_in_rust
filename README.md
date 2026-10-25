# BIGNUM · 任意精度大数计算器

[![CI](https://github.com/lcr12512/Bignum_calc_in_rust/actions/workflows/ci.yml/badge.svg)](https://github.com/lcr12512/Bignum_calc_in_rust/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-0078D4)](https://github.com/lcr12512/Bignum_calc_in_rust/releases)
[![Rust](https://img.shields.io/badge/rust-1.99.0-orange)](rust-toolchain.toml)

> **Windows 10/11 x64 · 单文件免安装 · 运行时不联网**。Rust + [dashu](https://crates.io/crates/dashu) 计算内核，WebView2 界面。以 **MIT** 许可证开源，欢迎 Issue 与 PR。

一个运行于 Windows 的 GUI 大数计算器。核心卖点是**真正的任意精度**：一次算出 `10^3000000`（300 万位十进制整数）只需约 0.25 秒；`10000!` 秒出 35,660 位精确整数。加、减、乘、取余、阶乘与正整数乘方均为**精确整数结果**；除法、对数、无理数常数与导数结果精确到用户指定的有效数字（1–5000 位，默认 60 位）。

界面为中文，单窗口、免安装；运行时不联网（仅当目标机缺少 WebView2 运行时时，需联网安装该运行时）。

---

## 1. 技术栈与总体架构

| 层 | 选型 | 说明 |
|---|---|---|
| 计算内核 | **Rust + dashu 0.6** | 纯 Rust 任意精度整数/浮点库（原生替代 GMP/MPFR），自带快速乘法、Chudnovsky π 与快速十进制转换；MSVC 工具链直接编译，无需 GMP/MSYS2 |
| 窗口外壳 | **tao 0.37 + wry 0.57** | tao 负责 Windows 窗口与事件循环，wry 嵌入系统 **WebView2** 渲染前端 |
| 前端界面 | 原生 HTML/CSS/JS | 三个文件（index.html / style.css / app.js）通过 `include_str!` 编译进 exe，自定义 `bignum://` 协议加载，零网络依赖 |
| 前后端通信 | WebView2 IPC（JSON） | `window.ipc.postMessage` 上行；Rust 用 `evaluate_script` 下行推送阶段进度与结果 |
| 便捷能力 | arboard / rfd | 系统剪贴板复制、原生“另存为”导出 .txt |
| 便携性 | 静态 CRT | `.cargo/config.toml` 开启 `+crt-static`，目标机器无需安装 VC++ 运行库 |

### 运行流程

```
用户按下“＝ 计算”
      │  JSON: {cmd:"eval", expr:"sum(k^2, k, 1, 1000)", precision:60}
      ▼
Rust 主线程（tao 事件循环）──► 新建工作线程执行 engine::eval
      │                              │  词法 → 语法树 → 求值（阶段进度回调）
      │◄────────── EventLoopProxy ◄──┘
      ▼
webview.evaluate_script("window.__bignum_recv({...})")
      ▼
前端渲染：位数巨幕、科学计数法、首尾预览、精确商/余、耗时
```

### 表达式引擎实现（`src/engine.rs`）

1. **词法分析**：归一化输入（全角数字/符号、`×÷−`、`π`、`Σ` 等映射），扫描出数字字面量、标识符与运算符。
2. **递归下降语法分析**，优先级从低到高：
   `expr → term → unary → power → postfix → primary`
   支持右结合幂 `2^3^2`、后缀阶乘 `n!`、隐式乘法 `2π`、`2(3+4)`、科学计数法 `1e3000000`。
3. **求值器**采用整/浮双态值 `Val::Int(IBig) | Val::Dec(Real)`：
   - 两侧都是整数且运算封闭（`+ - × % ^(非负整数指数) !`）时走**精确整数路径**；
   - 否则提升为高精度二进制浮点 `Real`（工作精度 = 请求位数 × 3.322 + 40 位保护），最终按请求位数四舍五入并以十进制输出。
4. **超大阶乘的 Stirling 路径**：当 `n!` 的精确十进制会超过 1 亿位（`n` 约大于 1350 万，或 `n` 超出 64 位整数）时，改用
   `ln(n!) = n·ln n − n + ½·ln(2πn) + Σ B₂ₖ / (2k(2k−1)·n^(2k−1))`
   计算对数：Bernoulli 数按需用精确有理数递推生成并缓存（最多 B₃₂₀），修正项加到小于目标精度为止；随后换算出科学计数法尾数、十进制位数与 log₁₀/ln 值。`π` 由 dashu 的 Chudnovsky 算法按需精度提供。
5. **进度回调**：乘方逐位平方/乘、求和逐项累加都会把比例回传前端，界面显示实时进度条与耗时。
6. **数值积分**使用 tanh-sinh（双指数）求积：有限区间做 `x = c + s·tanh((π/2)·sinh t)` 变换；无穷限先做 `x = a ± e^s` 变换，再对 `s ∈ (−∞, ∞)` 做双指数求积；每层把步长减半并比较相邻结果，误差小于目标精度即收敛，节点截断以项幅值低于目标精度为准，并输出估计误差与函数求值次数。

### 大结果的内存策略

- 计算在工作线程完成，完整结果字符串保存在 Rust 侧（`Arc<Mutex<Store>>`）。
- 下发到网页的只有**首 2400 字符 + 尾 2400 字符 + 被省略位数**，因此上千万位结果也不会撑爆 WebView。
- “复制全部 / 导出 .txt”通过 IPC 回到 Rust，由 arboard / 文件系统直接处理完整字符串（实测把 3,000,001 字符完整写入剪贴板）。

### 平台支持

| 平台 | 状态 |
|---|---|
| Windows 11 x64 | ✅ 主要支持 |
| Windows 10 x64（22H2 及以上） | ✅ 需 Microsoft Edge WebView2 Runtime（安装包内附官方引导程序） |
| Windows ARM64 | ⚠️ 未测试（可按 x64 模拟运行） |
| macOS / Linux | ❌ 暂不支持（外壳依赖 tao + wry，可移植但未验证） |

### 许可证

本项目以 [MIT License](LICENSE) 发布：可自由使用、修改、分发（含商业用途），仅需保留版权声明与许可声明。**MIT 仅覆盖本项目自身代码**（`src/`、`ui/` 等）；`vendor/cookie` 是第三方代码（cookie 0.18.2，MIT OR Apache-2.0，经本项目小幅修改），其许可不变。

第三方依赖的完整清单、许可证全文与修改声明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)；`vendor/cookie` 的修改记录见 [vendor/cookie/VENDORED.md](vendor/cookie/VENDORED.md)。

### 第三方组件与隐私

- **WebView2 Runtime** 由 Microsoft 提供并自动更新（Evergreen 模式），受 Microsoft 的 WebView2 Runtime 许可条款约束；本程序随包附带的 `MicrosoftEdgeWebview2Setup.exe` 是 Microsoft 官方引导程序（未做修改），仅在目标机缺少运行时时用于在线安装。
- **Microsoft Defender SmartScreen 默认开启**。Windows 可能依据 Microsoft 隐私声明收集/发送关于应用启动的信息：<https://aka.ms/privacy> 。
- 本程序本身不收集任何数据、不联网上报；用户数据仅存于 `%LOCALAPPDATA%\BIGNUM\`（WebView2 缓存与界面历史记录），删除该目录即可清除。

### 发布与下载

发布产物（exe、便携包 zip、WebView2 引导程序）**不进入 git 历史**，统一通过 [GitHub Releases](https://github.com/lcr12512/Bignum_calc_in_rust/releases) 分发，并提供 SHA256 校验文件。

---

## 2. 运算能力与语义

### 运算符

| 运算 | 语义 | 结果性质 |
|---|---|---|
| `＋` `－` `×` | 任意精度整数四则；含小数时高精度浮点 | **精确**（全整数时） |
| `÷` | 整除时精确；否则按“有效数字”给出近似值 | 近似值 + 同时给出**精确的整数商 Q 与余数 R**（A = Q×B + R） |
| `%` | 整数取余，截断语义（余数与被除数同号） | **精确** |
| `^` | 指数为非负整数：二进制快速幂；负整数/小数：A^B = e^(B·ln A) | 整数指数**精确**；其余按有效数字近似 |
| `!` | 阶乘（二分乘积）。n 不大时给出精确整数；**n 任意大**（如 `6.02e23!`）时自动切换到 Stirling 对数阶乘，并支持**对数域四则运算** | 小 n **精确**；超大 n 在对数域中保持有效数字精度 |

### 函数与常数

| 语法 | 说明 |
|---|---|
| `ln(x)` | 自然对数 |
| `log(x)` / `log(x, b)` | 常用对数 / 任意底对数 |
| `sum(通项, 变量, 起点, 终点)` | 求和 `Σ`，如 `sum(1/k^2, k, 1, 10000)`；起点大于终点时递减 |
| `diff(函数, 变量, 求值点)` | 数值微分 `d/dx`，如 `diff(x^3, x, 2)`；中心差分（5 点格式，步长 1e-20 × max(1,|x|)） |
| `integ(函数, 变量, 下界, 上界)` | **数值积分** `∫`，如 `integ(e^(-x^2), x, -inf, inf)`；tanh-sinh 双指数求积，支持 `inf`/`-inf` 无穷限 |
| `e` | 自然常数 |
| `π` | 圆周率（dashu Chudnovsky 算法按需精度计算） |
| `inf` | 正无穷，仅可用作积分的上下限 |

### 数值积分（∫）

- 语法 `integ(函数, 变量, 下界, 上界)`（`int` 亦可）；上下限可以是表达式、`inf`、`-inf`。
- 算法：**tanh-sinh（双指数）求积**，对端点奇异性与被积函数的急剧变化鲁棒；每层细分后比较相邻结果，达到目标精度即停，并给出**估计误差**与**函数求值次数**。
- 无穷限处理：`[a, ∞)` 用 `x = a + e^s` 变换后对 `s ∈ ℝ` 做双指数求积，`(−∞, b]` 对称处理，`(−∞, ∞)` 在 0 处拆分；对高斯型、指数衰减、幂律衰减（如 `1/x^2`）均有效。
- 精度上限 **80 位有效数字**（更高精度设置会自动按 80 位计算并在结果附注说明）；另有 20 万次求值与 120 秒时间预算保护。
- 结果可与其它运算组合，如 `2*integ(e^(-x), x, 0, inf)`、`integ(x^2, x, 0, 1)^0.5`。

| 示例 | 结果 |
|---|---|
| `integ(x^2, x, 0, 1)` | 0.333333…（精确值 1/3，40 位误差 < 1e-58） |
| `integ(1/x, x, 1, e)` | 1 |
| `integ(e^(-x^2), x, -inf, inf)` | 1.772453850905516027298167483341145182798（=√π） |
| `integ(1/x^2, x, 1, inf)` | 1 |
| `integ(x, x, 1, 0)` | −0.5（自动交换上下限并取负） |

### 超大阶乘之间的运算（对数域）

精确路径之外，超大阶乘以**对数域数值**（`10^L`，L 为高精度十进制对数）参与表达式，因此可以直接做物理化学中常见的组合运算：

| 表达式 | 含义 | 结果 |
|---|---|---|
| `6.02e23! / 6.02e22!` | 阶乘之比 | `1.3038…×10^12708684628668307502231578` |
| `(6.02e23!)^2` | 阶乘的幂 | 位数约 2.81×10^25 |
| `1e20!/(1e19!*1e19!)` | 多项式系数（组合数） | log₁₀ ≈ 1.585×10^21 |
| `ln(6.02e23!)` | 对数（统计力学熵项） | 3.2360…×10^25 |
| `N! / (n1! n2! n3!)` | 多组分配（multinomial） | 任意规模的因子之比 |
| `sum(k!, k, 1, 5000)` | 阶乘求和 | 逐项对数累加 |

语义说明：

- **乘、除、乘方**在对数域中**等价精确**（受所选有效数字约束，默认 60 位）。
- **加、减**按主项合成：量级相差超过 p 位有效数字的小项会被舍入忽略（结果附注说明）；两项量级过于接近时，会提示无法在对数域分辨其差，需改用更高有效数字或改写表达式。
- 精确整数与对数域数值可以混用（如 `10000! / 6.02e23!`），精确值会先取对数再参与运算。

### 输入容错与保护

- **普通计算器输入方式**：屏幕键盘直接输入（数字、运算符、函数键），同时也支持键盘直接敲表达式；`Enter` 或 `Ctrl+Enter` 计算。
- 支持 `e` 记法（`1e3000000`）、小数科学计数法（`2.5e3`）、隐式乘法（`2π`、`2(3+4)`）、全角数字与全角符号。
- 单次输出上限 **1 亿位**（超出时给出预计位数并拒绝，防止内存耗尽）；指数超过 2^64 直接拒绝。
- 求和保护：项数上限 500 万项、累计位数上限 1 亿位、时间预算 120 秒；阶乘的精确路径要求 `n ≤ 2000 万` 且位数不超过 1 亿位，超出自动切换到 Stirling 对数阶乘。

---

## 3. 性能实测

本机（Windows / MSVC Release / `lto=thin, codeunits=1`）实测：

| 场景 | 耗时 |
|---|---|
| `10^3000000` 全流程（解析 + 计算 + 300 万位格式化 + 界面） | 约 0.25–0.3 s |
| `10000!`（35,660 位精确整数） | 约 2 ms |
| `6.02×10^23!`（Stirling 对数阶乘，位数约 1.4×10^25） | 约 10 ms |
| `integ(x^2, x, 0, 1)`（40 位，约 590 次求值） | 毫秒级 |
| `integ(e^(-x^2), x, -inf, inf)`（40 位 / 60 位，约 4 千 / 8 千次求值） | 约 0.5 s / 约 4 s |
| `3^3000000`（1,431,364 位） | 约 40 ms |
| 143 万位 × 95 万位乘法 | 约 33 ms |
| 百万位级除法 / 取余 | 约 64 ms |
| 大数 log₁₀（60 位精度） | 约 0.25 ms |
| `sum(k^2, k, 1, 10000)`（1 万项精确累加） | 毫秒级 |

---

## 4. 界面设计

前端遵循统一的动效与可访问性规范（静止帧优先、真实物理动效、完整状态矩阵、`prefers-reduced-motion` 降级）：

- **静止帧优先**：三区仪表盘（01 输入台 / 02 结果台 / 03 轨道记录）、技术制图网格底衬、角落刻度、超大字号对比（结果位数 84px vs 正文 14px）、单一暖铜色 accent + 中性冷灰底，无紫渐变/无假 UI 金属框等 AI 塑料感特征。
- **真实物理动效**：
  - 结果位数用弹簧计数（原生 rAF 弹簧，stiffness 68、阻尼比 0.88，`dt` 钳制到 1/30 保证帧率无关）；
  - 按键按压 `scale(.97)` ≤ 90 ms；计算主键有按压反馈与标签原地替换的等待态；
  - 结果面板 `clip-path` 揭幕 520 ms；错误状态 220 ms 轻微抖动；数字面板入场后用 scale 回弹安定。
- **完整状态矩阵**：hover（仅指针设备）、`:focus-visible` 可见焦点环、`:active` 真按压、加载中（标签原地替换 + 宽度锁定 + 不确定/确定进度条）、禁用、错误（紧贴原因、不只靠颜色）、空状态（设计过的引导文案）。
- **持久化成功回执**：复制/导出成功不会弹过期即消失的 toast，而是在结果区留下带时间的持久回执。
- **无障碍与降级**：`prefers-reduced-motion` 下全部动画降级为直接落位；`tabular-nums` 等宽数字；`role=status` / `aria-live` 状态播报；`lang="zh-Hans"`。
- 历史记录（轨道记录）持久化在本地，点击可回填该次计算的表达式与精度。

---

## 5. 使用方法

1. 双击 `bignumcalc.exe` 启动（首次启动约 1–2 秒加载 WebView2）。
2. 用屏幕键盘或键盘输入表达式（两者可混用），按 `＝ 计算` / `Enter` / `Ctrl+Enter`。
3. 典型示例：
   - `10^3000000` → 3,000,001 位精确整数；
   - `10000!` → 35,660 位精确阶乘；
   - `6.02e23!` → 阿伏伽德罗常数量级的阶乘：约 1.405×10^25 位，输出科学计数法 `5.6265…×10^14053871809631452780257320`、精确位数与对数；
   - `6.02e23! / 6.02e22!` → 阶乘之比 `1.3038…×10^12708684628668307502231578`（对数域运算）；
   - `ln(6.02e23!)` → 统计力学中直接可用的对数阶乘；
   - `integ(e^(-x^2), x, -inf, inf)` → √π ≈ 1.772453850905516…（支持无穷限积分）；
   - `sum(1/k^2, k, 1, 10000)` → 高精度逼近 π²/6；
   - `diff(x^3, x, 2)` → 切线斜率 12；
   - `ln(2)`（有效数字 80）→ 0.693147…（80 位）；
   - `1e3000000 ÷ 7` → 近似小数 + 精确整数商 Q 与余数 R；
   - `2π`、`e^10`、`log(8, 2)`、`10 % 3`、`2^-10`。
4. 结果区操作：`复制全部`（直接写入系统剪贴板）、`导出 .txt`（原生另存为对话框）；结果位数过大时界面只显示首尾，“省略 N 位”提示确切的位数。
5. 底部“轨道记录”保留最近 12 次计算，点击任意一条可回填表达式与精度。

### 屏幕键盘

```
 C    ⌫    (    )    π    ln
 7    8    9    ÷    e    log
 4    5    6    ×    ^    n!
 1    2    3    −    %    Σ
 0    .    k    +    x    d/dx
 ＝ 计算（整行）
```

### 开发者冒烟钩子（可选）

设置环境变量 `BIGNUM_SMOKE` 启动时可自动执行一次算例用于回归验证：

```
set BIGNUM_SMOKE=1      & bignumcalc.exe   # 10^3000000
set BIGNUM_SMOKE=fact   & bignumcalc.exe   # 10000!
set BIGNUM_SMOKE=huge   & bignumcalc.exe   # 6.02e23!（Stirling 对数阶乘）
set BIGNUM_SMOKE=hugediv & bignumcalc.exe  # 6.02e23! / 6.02e22!（对数域除法）
set BIGNUM_SMOKE=sum    & bignumcalc.exe   # sum(k^2, k, 1, 10000)
set BIGNUM_SMOKE=diff   & bignumcalc.exe   # diff(x^3, x, 2)
set BIGNUM_SMOKE=integ  & bignumcalc.exe   # integ(e^(-x^2), x, -inf, inf)
set BIGNUM_SMOKE=ln     & bignumcalc.exe   # ln(2)，80 位
set BIGNUM_SMOKE=div    & bignumcalc.exe   # 1e3000000 / 7，80 位
set BIGNUM_SMOKE=log    & bignumcalc.exe   # log(2)，80 位
```

---

## 6. 复制到其他电脑使用（便携包）

从 [Releases](https://github.com/lcr12512/Bignum_calc_in_rust/releases) 下载 `BIGNUM-calculator-vX.Y.Z-win64.zip`，
解压后目录为：

```
BIGNUM-calculator-vX.Y.Z-win64\
├── bignumcalc.exe                 主程序（静态 CRT，免安装）
├── MicrosoftEdgeWebview2Setup.exe WebView2 运行时在线安装引导（约 1.8 MB，仅在目标机缺失时需要）
├── README.md                      本说明
├── LICENSE                        MIT 许可证
└── THIRD_PARTY_NOTICES.md         第三方组件声明
```

步骤：

1. 把整个 `BIGNUM-calculator-vX.Y.Z-win64` 文件夹复制到目标电脑任意位置（U 盘、桌面均可）。
2. 直接双击 `bignumcalc.exe`。
   - **Windows 11** 与更新过的 **Windows 10** 通常已内置 WebView2，可直接使用；
   - 若程序弹窗提示“缺少 WebView2 运行时”，双击同目录的 `MicrosoftEdgeWebview2Setup.exe` 联机安装（约 2 MB），完成后重新打开即可。
3. 程序不写注册表、不改系统设置；唯一的用户数据（WebView2 缓存与历史记录）写入 `%LOCALAPPDATA%\BIGNUM\`，删除该目录即可彻底清理。

系统要求：64 位 Windows 10/11。

---

## 7. 从源码构建

```powershell
cargo fmt --all -- --check                     # 格式检查
cargo clippy --all-targets --locked -- -D warnings   # 静态检查
cargo test --locked                            # 单元测试（30 项：24 引擎 + 6 外壳；1 项 #[ignore]）
cargo build --release --locked                 # 产物：target\release\bignumcalc.exe
cargo run --release                            # 开发运行
```

- 前置条件：Rust MSVC 工具链（rustup 默认安装即可，版本要求 ≥ 1.88）；不需要 GMP、CMake、Node 等额外环境。
- 首次构建需要联网拉取 crates.io 依赖；依赖缓存预热后可 `cargo build --release --offline --locked`。
- `.cargo/config.toml` 已为目标开启 `+crt-static`，保证目标机无需 VC++ 运行库。
- **`vendor/cookie` 目录**：wry 依赖的 `cookie` crate 原带一个仅用于 nightly `doc_cfg` 检测的构建脚本，在部分国产安全软件（如 360 主动防御）下会因拦截新编译的可执行文件而导致构建失败。此处把该 crate 本地 vendor、移除该构建脚本（功能不受影响）并通过 `[patch.crates-io]` 指向，详见 `vendor/cookie/VENDORED.md`。
- 该目录**必须随仓库提交**：`Cargo.lock` 中 cookie 为 path 依赖，删除后无法构建。
- 如果安全软件仍然拦截，请把项目目录（以及 `%USERPROFILE%\.cargo`）加入信任区，或在构建/运行时临时关闭主动防御。

### 目录结构

```
bignumcalc\
├── Cargo.toml              依赖与编译配置（含 [patch.crates-io]）
├── .cargo\config.toml      静态 CRT 配置（便携关键）
├── src\
│   ├── main.rs             窗口、WebView2、IPC、剪贴板/导出、冒烟钩子
│   └── engine.rs           表达式词法/语法/求值内核与结果格式化（含单元测试）
├── ui\
│   ├── index.html          界面结构（表达式输入 + 计算器键盘 + 三区仪表盘）
│   ├── style.css           设计令牌、布局、状态与动效
│   └── app.js              键盘输入、交互、弹簧动画、历史记录、IPC 消息处理
├── .github\workflows\      CI 与 Release 自动化
├── scripts\package-release.ps1  本地打包脚本（生成 dist-release\*.zip + SHA256）
├── THIRD_PARTY_NOTICES.md  第三方许可证与声明
├── CHANGELOG.md            版本记录
└── vendor\cookie\          构建拦截规避（见 vendor\cookie\VENDORED.md）
```

> 发布产物（exe、便携包 zip、WebView2 引导程序）不进入 git 历史，统一通过 [GitHub Releases](https://github.com/lcr12512/Bignum_calc_in_rust/releases) 分发。
> 本地打包：`powershell -ExecutionPolicy Bypass -File .\scripts\package-release.ps1 -Tag v0.1.0`

---

## 8. 验证记录

- **单元测试**：`cargo test --locked` 共 **30 项**（24 项引擎 + 6 项外壳），**29 通过 / 1 项 `#[ignore]`**（大规模乘法上限测试，需手动 `cargo test --release -- --ignored` 运行）。覆盖 300 万位乘方、阶乘（含 1000! 已知前缀）、**Stirling 对数阶乘与精确值逐位比对（100000!）**、**6.02e23! 位数与对数前缀校验**、**超大阶乘对数域算术（乘除还原、阶乘比、乘方、ln、加减主项合成、相同量级求差报错、多项式系数）**、**定积分（多项式、1/x、无穷限高斯 √π、幂律衰减 1/x²、区间反向取负、错误路径）**、精确与近似求和、数值导数、自然/常用/任意底对数、e 与 π 常数、隐式乘法与科学计数法、带余除法的精确商余、错误路径（除零、负数阶乘、超出上限、未绑定变量）；另有 **2026-10 新增的 12 项回归测试**：嵌套深度守卫、2 MiB 线程下长链/阶乘链不崩溃、表达式长度上限、Real 溢出拒绝、对数域减法符号、求和 i64 边界、0 的负次幂、负数对数域、对数域求和、求和变量作用域、科学计数法指数下溢、乘法上限。
- **安全与健壮性加固（2026-10）**：WebView2 导航白名单 + 关闭外部拖放 + IPC 来源校验 + CSP/`nosniff`；IPC 表达式 4096 字符硬上限；worker panic 兜底与 300 秒无进度看门狗；Mutex 中毒容错；历史记录保存完整表达式并对本地数据做 schema 校验。
- **端到端 GUI 冒烟**（每项均自动复制结果到剪贴板后校验）：
  - `10^3000000` → 3,000,001 位精确整数；
  - `10000!` → 35,660 位精确整数；
  - `6.02e23!` → 科学计数法 `5.626538398562816991407278291160629932654119640240968208376…×10^14053871809631452780257320`，位数 14053871809631452780257321；
  - `6.02e23! / 6.02e22!` → `1.303820474071904448216987950831130838348590174046705882031…×10^12708684628668307502231578`；
  - `integ(e^(-x^2), x, -inf, inf)`（60 位）→ 1.77245385090551602729816748334114518279754945612238712821381（√π），估计误差 ≤ 2×10^-78；
  - `sum(k^2, k, 1, 10000)` → 333,383,335,000（精确）；
  - `diff(x^3, x, 2)` → 12；
  - `ln(2)`（80 位）→ 0.69314718055994530941723212145817656807550013436025525412068000949339362196969472。
- **剪贴板实测**：复制 3,000,001 字符结果后读取系统剪贴板，长度与结果位数一致。

## 9. 已知限制

- **输入上限**：表达式最多 **4096 个字符**；解析器递归深度上限 **256**（约 40 层括号嵌套，更深的括号会给出“嵌套过深”错误）、求值深度上限 **512**（约 500 个连续运算项，更长的 `1+1+…` 链也会报错；请改用 `sum`）。
- **数值范围**：指数运算结果超出内部可表示范围时返回“结果超出可表示范围”，不会输出伪造数值。
- 超大计算（如 10^1e8 级、千万项求和）目前不可中途取消，需要等待完成或结束进程；乘方与求和过程有进度提示；求和过程**逐项检查** 120 秒时间预算。
- 除法、对数、无理数常数与导数为近似值（按有效数字），界面会明确标注“近似”，整除/整数乘方/阶乘/整数求和标注“精确”。
- 导数为数值微分（中心差分），在函数剧烈振荡或不可导点附近精度下降；有效数字设置越高结果越精细。
- 超大阶乘（如 `6.02e23!`）的精确十进制有约 10^25 位，任何存储都无法容纳，因此程序自动改用 Stirling 对数阶乘与对数域运算，只输出科学计数法、位数与对数（标注为近似值）；`n` 接近精确路径上限且要求极高有效数字（约 2000 位以上）时，Stirling 级数可能无法收敛到目标精度，此时会提示降低有效数字。
- 对数域中的加/减是主项合成：量级差超过有效数字的小项会被忽略（结果附注会说明）；两项量级过于接近时求差会报错，需提高有效数字或改写表达式。乘、除、乘方则不受此限，在对数域中保持精度；对数域量级的项**可以参与求和**（结果保留所选有效数字）。
- 数值积分为近似值：tanh-sinh 对光滑函数与端点奇异性收敛极快，但对剧烈振荡（如 `sin(1/x)`）或不可积函数可能在层数/时间预算内不收敛并给出明确报错；积分精度上限 80 位有效数字；**一次计算内的所有积分共享** 20 万次求值 / 120 秒预算（嵌套积分不再各自重置）。
- 求和/乘方/乘法结果上限为 1 亿位，超出会给出预计位数并拒绝执行。

---

## 10. 贡献

欢迎提交 Issue 与 Pull Request！开发环境、提交前检查（fmt / clippy / test / build）与代码规范见
[CONTRIBUTING.md](CONTRIBUTING.md)。参与即表示同意 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)，
并以 MIT 许可证授权你的贡献。

## 11. 安全

请勿用公开 Issue 报告安全漏洞，流程见 [SECURITY.md](SECURITY.md)。
程序不联网上报、不写注册表，仅使用 `%LOCALAPPDATA%\BIGNUM\` 存放 WebView2 缓存与历史记录。

## 12. 致谢

- [dashu](https://github.com/cmpute/dashu)：纯 Rust 任意精度数值库，本项目的计算内核
- [tao](https://github.com/tauri-apps/tao) / [wry](https://github.com/tauri-apps/wry)：窗口与 WebView2 外壳
- [arboard](https://github.com/1Password/arboard) / [rfd](https://github.com/PolyMeilex/rfd)：剪贴板与原生文件对话框
- 以及 `Cargo.lock` 中全部开源依赖的作者（见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)）
