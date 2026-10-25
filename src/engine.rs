use dashu::base::BitTest;
use dashu::integer::{IBig, UBig};
use dashu::{Decimal, Real};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const DEFAULT_PRECISION: usize = 60;
pub const MAX_PRECISION: usize = 5000;
pub const MAX_DIGITS: f64 = 100_000_000.0;
const GUARD_DIGITS: usize = 40;
const PREVIEW_SIDE: usize = 2400;
const MAX_SUM_TERMS: u64 = 5_000_000;
const MAX_SUM_SECONDS: u64 = 120;
const MAX_FACTORIAL: u64 = 20_000_000;
const MAX_LITERAL_EXP: i64 = 1_000_000;
const BERNOULLI_MAX_M: usize = 320;
const MAX_INTEG_LEVEL: usize = 12;
const INTEG_DIGITS_CAP: usize = 80;
const INTEG_MAX_NODES: i64 = 20_000;
const INTEG_MAX_EVALS: u64 = 200_000;
const INTEG_SECONDS: u64 = 120;
// 解析器与求值器的显式深度上限（替代对工作线程栈大小的依赖）。
// 求值深度取 512：dev 构建（opt-level=1、带调试信息）下 2 MiB 工作线程栈
// 在约 1024 层递归时会溢出，512 留出足够余量；外壳输入上限 4096 字符，
// 更深的链一律走“嵌套过深”错误而不是 abort。
const MAX_PARSE_DEPTH: usize = 256;
const MAX_EVAL_DEPTH: usize = 512;
// 表达式字符数上限，与外壳输入框上限一致；用于兜底 Node 递归析构的栈深度。
const MAX_EXPR_CHARS: usize = 4096;
// dashu 0.6 溢出饱和端点的指数（isize::MAX 附近的伪有限值），合法值不会自然到达。
const REAL_SATURATION_EXPONENT: isize = isize::MAX - 32;

pub enum Stage<'a> {
    Label(&'a str),
    Ratio(&'a str, u64, u64),
}

#[derive(Debug)]
pub struct Extra {
    pub label: &'static str,
    pub full: String,
    pub head: String,
    pub tail: String,
    pub omitted: usize,
}

#[derive(Debug)]
pub struct Outcome {
    pub kind: &'static str,
    pub exact: bool,
    pub full: String,
    pub head: String,
    pub tail: String,
    pub omitted: usize,
    pub digits: usize,
    pub scientific: Option<String>,
    pub extras: Vec<Extra>,
    pub note: Option<String>,
    pub ms: u128,
    pub headline: Option<String>,
    pub unit: Option<&'static str>,
}

pub fn eval(expr: &str, precision: usize, prog: &mut dyn FnMut(Stage)) -> Result<Outcome, String> {
    let started = Instant::now();
    if expr.chars().count() > MAX_EXPR_CHARS {
        return Err(format!(
            "表达式过长（上限 {MAX_EXPR_CHARS} 个字符），请缩短后重试。"
        ));
    }
    let p = precision.clamp(1, MAX_PRECISION);
    let work_bits = digits_to_bits(p + GUARD_DIGITS);

    prog(Stage::Label("解析表达式"));
    let root = parse(expr, work_bits)?;

    prog(Stage::Label("求值"));
    let mut ev = Eval {
        vars: HashMap::new(),
        work_bits,
        p,
        prog: &mut *prog,
        depth: 0,
        top_qr: None,
        note: None,
        started,
        integ_extras: None,
        integ_evals: 0,
        integ_error: zero(work_bits),
        integ_level: 0,
    };
    let value = ev.eval(&root)?;
    let note = ev.note.take();
    let top_qr = ev.top_qr.take();
    let integ_extras = ev.integ_extras.take();
    drop(ev);

    prog(Stage::Label("格式化结果"));
    let mut out = match value {
        Val::Int(v) => int_outcome(v, started, Some("精确整数结果。".into())),
        Val::Dec(d) => {
            // dashu 在指数超出 isize 时会产出“指数饱和”的伪有限值，必须先拦截。
            check_real_range(&d)?;
            let rounded = d.with_precision(digits_to_bits(p)).value();
            let mut o = approx_outcome(&rounded, p, "表达式求值", started);
            o.note = Some(format!(
                "结果为 {p} 位有效数字的近似值（涉及除法、对数、无理数或数值微分）。"
            ));
            o
        }
        Val::Log(l) => log_outcome(&l, p, started)?,
    };
    if let Some(note) = note {
        out.note = Some(note);
    }
    if let Some((q, r)) = top_qr {
        out.extras = vec![
            make_extra("整数商 Q", q.to_string()),
            make_extra("余数 R", r.to_string()),
        ];
    }
    if let Some(kv) = integ_extras {
        out.extras = kv.into_iter().map(|(l, v)| make_extra(l, v)).collect();
    }
    Ok(out)
}

fn digits_to_bits(digits: usize) -> usize {
    digits.saturating_mul(3322) / 1000 + 32
}

fn real_of(n: &IBig, bits: usize) -> Real {
    Real::from(n.clone()).with_precision(bits).value()
}

fn ten_pow(e: u64) -> IBig {
    IBig::from(UBig::from(10u8).pow(e as usize))
}

enum Val {
    Int(IBig),
    Dec(Real),
    Log(LogVal),
}

#[derive(Clone)]
struct LogVal {
    sign: bool,
    log10: Real,
}

impl Val {
    fn into_dec(self, bits: usize) -> Result<Real, String> {
        match self {
            Val::Int(v) => Ok(real_of(&v, bits)),
            Val::Dec(d) => Ok(d.with_precision(bits).value()),
            Val::Log(_) => {
                Err("该中间结果的规模超出普通数值范围，请使用乘法、除法、乘方或对数处理它。".into())
            }
        }
    }
}

fn to_log(v: Val, bits: usize) -> Result<LogVal, String> {
    match v {
        Val::Log(l) => Ok(l),
        Val::Int(i) => {
            if i == IBig::ZERO {
                return Err("0 不能取对数。".into());
            }
            let sign = i < IBig::ZERO;
            let mag = if sign { -i } else { i };
            let log10 = real_of(&mag, bits).log10().with_precision(bits).value();
            Ok(LogVal { sign, log10 })
        }
        Val::Dec(d) => {
            if d == zero(bits) {
                return Err("0 不能取对数。".into());
            }
            let sign = d < zero(bits);
            let log10 = abs_real(&d).log10().with_precision(bits).value();
            Ok(LogVal { sign, log10 })
        }
    }
}

fn log_add(
    a: &LogVal,
    b: &LogVal,
    subtract: bool,
    bits: usize,
    p: usize,
) -> Result<LogVal, String> {
    let bsign = b.sign != subtract;
    let tiny = (&one(bits) / &real_of(&ten_pow((p + 30) as u64), bits))
        .with_precision(bits)
        .value();
    if a.sign == bsign {
        let (big, small) = if a.log10 >= b.log10 { (a, b) } else { (b, a) };
        let diff = (&small.log10 - &big.log10).with_precision(bits).value();
        if diff <= -tiny {
            return Ok(LogVal {
                sign: bsign,
                log10: big.log10.clone(),
            });
        }
        let t = (&diff * &real_of(&IBig::from(10u8), bits).ln())
            .exp()
            .with_precision(bits)
            .value();
        let one_plus = (&one(bits) + &t).with_precision(bits).value();
        let ln10 = real_of(&IBig::from(10u8), bits).ln();
        let log10 = (&big.log10 + &(one_plus.ln() / ln10))
            .with_precision(bits)
            .value();
        Ok(LogVal { sign: bsign, log10 })
    } else {
        // 异号相减：结果符号取“有效值更大”的那个操作数的真实符号。
        // 若 big 是 b，其有效符号是 bsign（b 本身符号经 subtract 翻转后）；
        // 若 big 是 a，则取 a.sign。
        let big_is_b = b.log10 >= a.log10;
        let (big, small) = if big_is_b { (b, a) } else { (a, b) };
        let diff = (&small.log10 - &big.log10).with_precision(bits).value();
        if diff >= -tiny {
            return Err(
                "两项量级过于接近，无法在对数域中分辨它们的差；请改用更高有效数字或改写表达式。"
                    .into(),
            );
        }
        let ln10 = real_of(&IBig::from(10u8), bits).ln();
        let t = (&diff * &ln10).exp().with_precision(bits).value();
        let one_minus = (&one(bits) - &t).with_precision(bits).value();
        let log10 = (&big.log10 + &(one_minus.ln() / ln10))
            .with_precision(bits)
            .value();
        Ok(LogVal {
            sign: if big_is_b { bsign } else { a.sign },
            log10,
        })
    }
}

enum Bin {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
}

enum Node {
    Int(IBig),
    Dec(Real),
    ConstE,
    ConstPi,
    ConstInf,
    Var(String),
    Neg(Box<Node>),
    Bin(Bin, Box<Node>, Box<Node>),
    Fact(Box<Node>),
    Ln(Box<Node>),
    Log10(Box<Node>),
    LogB(Box<Node>, Box<Node>),
    Sum {
        body: Box<Node>,
        var: String,
        from: Box<Node>,
        to: Box<Node>,
    },
    Diff {
        body: Box<Node>,
        var: String,
        at: Box<Node>,
    },
    Integ {
        body: Box<Node>,
        var: String,
        from: Bound,
        to: Bound,
    },
}

enum Bound {
    Expr(Box<Node>),
    Inf(bool),
}

enum Lim {
    NegInf,
    Val(Real),
    PosInf,
}

struct Eval<'a> {
    vars: HashMap<String, Val>,
    work_bits: usize,
    p: usize,
    prog: &'a mut dyn FnMut(Stage),
    depth: usize,
    top_qr: Option<(IBig, IBig)>,
    note: Option<String>,
    started: Instant,
    integ_extras: Option<Vec<(&'static str, String)>>,
    integ_evals: u64,
    integ_error: Real,
    integ_level: usize,
}

impl<'a> Eval<'a> {
    fn eval(&mut self, node: &Node) -> Result<Val, String> {
        // 左深树（1+1+…）与 ! 链的求值/析构都按节点递归，必须在递归前显式限深。
        if self.depth >= MAX_EVAL_DEPTH {
            return Err(format!("表达式嵌套过深（上限 {MAX_EVAL_DEPTH} 层）。"));
        }
        self.depth += 1;
        let r = self.eval_inner(node);
        self.depth -= 1;
        r
    }

    fn eval_inner(&mut self, node: &Node) -> Result<Val, String> {
        match node {
            Node::Int(v) => Ok(Val::Int(v.clone())),
            Node::Dec(d) => Ok(Val::Dec(d.clone())),
            Node::ConstE => Ok(Val::Dec(Real::e(self.work_bits))),
            Node::ConstPi => Ok(Val::Dec(Real::pi(self.work_bits))),
            Node::ConstInf => {
                Err("inf 只能用作积分上下限，例如 integ(e^(-x^2), x, -inf, inf)。".into())
            }
            Node::Var(name) => self
                .vars
                .get(name)
                .map(|v| match v {
                    Val::Int(i) => Val::Int(i.clone()),
                    Val::Dec(d) => Val::Dec(d.clone()),
                    Val::Log(l) => Val::Log(l.clone()),
                })
                .ok_or_else(|| format!("未知变量“{name}”（求和用 k，导数用 x）")),
            Node::Neg(x) => match self.eval(x)? {
                Val::Int(v) => Ok(Val::Int(-v)),
                Val::Dec(d) => Ok(Val::Dec(-d)),
                Val::Log(mut l) => {
                    l.sign = !l.sign;
                    Ok(Val::Log(l))
                }
            },
            Node::Bin(op, a, b) => self.eval_bin(op, a, b),
            Node::Fact(x) => {
                let v = self.eval(x)?;
                let n = match v {
                    Val::Int(i) => i,
                    Val::Dec(_) => return Err("阶乘只支持非负整数。".into()),
                    Val::Log(_) => {
                        return Err("阶乘的参数必须是普通整数，不能是对数域的量级。".into());
                    }
                };
                if n < IBig::ZERO {
                    return Err("阶乘只支持非负整数。".into());
                }
                let exact_ok = match u64::try_from(&n) {
                    Ok(n64) => n64 <= MAX_FACTORIAL && estimate_factorial_digits(n64) <= MAX_DIGITS,
                    Err(_) => false,
                };
                if exact_ok {
                    let n64 = u64::try_from(&n).unwrap_or(0);
                    self.note = Some(format!("阶乘：{n64}!（二分乘积，精确整数）。"));
                    return Ok(Val::Int(factorial(n64)));
                }
                (self.prog)(Stage::Label("Stirling 对数阶乘"));
                Ok(Val::Log(log_factorial(&n, self.p)?))
            }
            Node::Ln(x) => {
                let v = self.eval(x)?;
                if let Val::Log(l) = v {
                    if l.sign {
                        return Err("ln 要求参数大于 0。".into());
                    }
                    let ln10 = real_of(&IBig::from(10u8), self.work_bits).ln();
                    return Ok(Val::Dec(
                        (&l.log10 * ln10).with_precision(self.work_bits).value(),
                    ));
                }
                let d = v.into_dec(self.work_bits)?;
                if d <= zero(self.work_bits) {
                    return Err("ln 要求参数大于 0。".into());
                }
                Ok(Val::Dec(d.ln()))
            }
            Node::Log10(x) => {
                let v = self.eval(x)?;
                if let Val::Log(l) = v {
                    if l.sign {
                        return Err("log 要求参数大于 0。".into());
                    }
                    return Ok(Val::Dec(l.log10));
                }
                let d = v.into_dec(self.work_bits)?;
                if d <= zero(self.work_bits) {
                    return Err("log 要求参数大于 0。".into());
                }
                Ok(Val::Dec(d.log10()))
            }
            Node::LogB(x, b) => {
                let vx = self.eval(x)?;
                let vb = self.eval(b)?;
                if let Val::Log(l) = vx {
                    if l.sign {
                        return Err("log 要求真数大于 0。".into());
                    }
                    let db = vb.into_dec(self.work_bits)?;
                    if db <= zero(self.work_bits) || db == one(self.work_bits) {
                        return Err("log 的底数必须大于 0 且不等于 1。".into());
                    }
                    let ln10 = real_of(&IBig::from(10u8), self.work_bits).ln();
                    let r = ((&l.log10 * &ln10) / db.ln())
                        .with_precision(self.work_bits)
                        .value();
                    return Ok(Val::Dec(r));
                }
                let dx = vx.into_dec(self.work_bits)?;
                let db = vb.into_dec(self.work_bits)?;
                if dx <= zero(self.work_bits) {
                    return Err("log 要求真数大于 0。".into());
                }
                if db <= zero(self.work_bits) || db == one(self.work_bits) {
                    return Err("log 的底数必须大于 0 且不等于 1。".into());
                }
                Ok(Val::Dec(dx.ln() / db.ln()))
            }
            Node::Sum {
                body,
                var,
                from,
                to,
            } => self.eval_sum(body, var, from, to),
            Node::Diff { body, var, at } => self.eval_diff(body, var, at),
            Node::Integ {
                body,
                var,
                from,
                to,
            } => self.eval_integ(body, var, from, to),
        }
    }

    fn eval_bin(&mut self, op: &Bin, a: &Node, b: &Node) -> Result<Val, String> {
        let va = self.eval(a)?;
        let vb = self.eval(b)?;
        let any_log = matches!(va, Val::Log(_)) || matches!(vb, Val::Log(_));
        match op {
            Bin::Add | Bin::Sub => {
                if any_log {
                    return self.add_sub_log(va, vb, matches!(op, Bin::Sub));
                }
                match (va, vb) {
                    (Val::Int(x), Val::Int(y)) => {
                        let r = if matches!(op, Bin::Add) {
                            &x + &y
                        } else {
                            &x - &y
                        };
                        Ok(Val::Int(r))
                    }
                    (x, y) => {
                        let x = x.into_dec(self.work_bits)?;
                        let y = y.into_dec(self.work_bits)?;
                        let r = if matches!(op, Bin::Add) {
                            &x + &y
                        } else {
                            &x - &y
                        };
                        Ok(Val::Dec(r))
                    }
                }
            }
            Bin::Mul => {
                if any_log {
                    if is_zero_val(&va, self.work_bits) || is_zero_val(&vb, self.work_bits) {
                        return Ok(Val::Int(IBig::ZERO));
                    }
                    let la = to_log(va, self.work_bits)?;
                    let lb = to_log(vb, self.work_bits)?;
                    let log10 = (&la.log10 + &lb.log10)
                        .with_precision(self.work_bits)
                        .value();
                    return Ok(Val::Log(LogVal {
                        sign: la.sign != lb.sign,
                        log10,
                    }));
                }
                match (va, vb) {
                    (Val::Int(x), Val::Int(y)) => {
                        let est = bit_digits(&x) + bit_digits(&y);
                        if est as f64 > MAX_DIGITS {
                            return Err(format!(
                                "预计乘法结果约 {est} 位，超过上限（{} 位）。",
                                MAX_DIGITS as u64
                            ));
                        }
                        Ok(Val::Int(&x * &y))
                    }
                    (x, y) => {
                        let x = x.into_dec(self.work_bits)?;
                        let y = y.into_dec(self.work_bits)?;
                        let r = &x * &y;
                        check_real_range(&r)?;
                        Ok(Val::Dec(r))
                    }
                }
            }
            Bin::Div => {
                if any_log {
                    if is_zero_val(&vb, self.work_bits) {
                        return Err("除数不能为 0。".into());
                    }
                    let la = to_log(va, self.work_bits)?;
                    let lb = to_log(vb, self.work_bits)?;
                    let log10 = (&la.log10 - &lb.log10)
                        .with_precision(self.work_bits)
                        .value();
                    return Ok(Val::Log(LogVal {
                        sign: la.sign != lb.sign,
                        log10,
                    }));
                }
                match (va, vb) {
                    (Val::Int(x), Val::Int(y)) => {
                        if y == IBig::ZERO {
                            return Err("除数不能为 0。".into());
                        }
                        let q = &x / &y;
                        let r = &x % &y;
                        if r == IBig::ZERO {
                            Ok(Val::Int(q))
                        } else {
                            if self.depth == 1 {
                                self.top_qr = Some((q, r));
                            }
                            let dx = real_of(&x, self.work_bits);
                            let dy = real_of(&y, self.work_bits);
                            Ok(Val::Dec(dx / dy))
                        }
                    }
                    (x, y) => {
                        let y = y.into_dec(self.work_bits)?;
                        if y == zero(self.work_bits) {
                            return Err("除数不能为 0。".into());
                        }
                        let x = x.into_dec(self.work_bits)?;
                        Ok(Val::Dec(x / y))
                    }
                }
            }
            Bin::Mod => match (va, vb) {
                (Val::Int(x), Val::Int(y)) => {
                    if y == IBig::ZERO {
                        return Err("取余的除数不能为 0。".into());
                    }
                    let q = &x / &y;
                    if self.depth == 1 {
                        self.top_qr = Some((q, &x % &y));
                    }
                    Ok(Val::Int(&x % &y))
                }
                _ => Err("取余（%）只支持普通整数，不能用于超大阶乘的量级。".into()),
            },
            Bin::Pow => self.eval_pow_with(va, vb),
        }
    }

    fn add_sub_log(&mut self, va: Val, vb: Val, subtract: bool) -> Result<Val, String> {
        if is_zero_val(&va, self.work_bits) {
            return neg_val(vb, subtract);
        }
        if is_zero_val(&vb, self.work_bits) {
            return Ok(va);
        }
        let la = to_log(va, self.work_bits)?;
        let lb = to_log(vb, self.work_bits)?;
        Ok(Val::Log(log_add(
            &la,
            &lb,
            subtract,
            self.work_bits,
            self.p,
        )?))
    }

    fn eval_pow_with(&mut self, va: Val, vb: Val) -> Result<Val, String> {
        let base_is_log = matches!(va, Val::Log(_));
        let exp_is_log = matches!(vb, Val::Log(_));
        if base_is_log || exp_is_log {
            let bits = self.work_bits;
            let ln10 = real_of(&IBig::from(10u8), bits).ln();
            let base = to_log(va, bits)?;
            if let Val::Log(le) = &vb {
                let f = le.log10.to_f64().value();
                if !f.is_finite() || f.abs() > 1.0e12 {
                    return Err("指数（对数域）过大，无法处理。".into());
                }
                if base.sign {
                    return Err("底数为负时，指数不能是对数域中的数值。".into());
                }
                let e10 = (&le.log10 * &ln10).exp().with_precision(bits).value();
                check_real_range(&e10)?;
                let log10 = (&base.log10 * &e10).with_precision(bits).value();
                check_real_range(&log10)?;
                return Ok(Val::Log(LogVal { sign: false, log10 }));
            }
            let (e_real, _) = match &vb {
                Val::Int(i) => (real_of(i, bits), true),
                Val::Dec(d) => (d.clone().with_precision(bits).value(), false),
                Val::Log(_) => unreachable!(),
            };
            let sign = if base.sign {
                match &vb {
                    Val::Int(i) => i.clone().into_parts().1.bit(0),
                    _ => {
                        return Err("负数的非整数次幂未定义（对数域）。".into());
                    }
                }
            } else {
                false
            };
            let log10 = (&base.log10 * &e_real).with_precision(bits).value();
            check_real_range(&log10)?;
            return Ok(Val::Log(LogVal { sign, log10 }));
        }
        let (base_int, exp_int) = match (&va, &vb) {
            (Val::Int(x), Val::Int(y)) => (Some(x.clone()), Some(y.clone())),
            _ => (None, None),
        };
        if let (Some(x), Some(y)) = (base_int, exp_int) {
            if y >= IBig::ZERO {
                if y == IBig::ZERO {
                    if x == IBig::ZERO {
                        return Err("0^0 未定义。".into());
                    }
                    return Ok(Val::Int(IBig::ONE));
                }
                if x == IBig::ZERO {
                    return Ok(Val::Int(IBig::ZERO));
                }
                if x == IBig::ONE {
                    return Ok(Val::Int(IBig::ONE));
                }
                if x == -IBig::ONE {
                    let (_s, e) = y.clone().into_parts();
                    return Ok(Val::Int(if e.bit(0) { -IBig::ONE } else { IBig::ONE }));
                }
                let (_, e) = y.clone().into_parts();
                if e.bit_len() > 64 {
                    return Err("指数超过 2^64，无法处理。".into());
                }
                (self.prog)(Stage::Label("估算结果规模"));
                let est = estimate_pow_digits(&x, &e)?;
                if est > MAX_DIGITS {
                    return Err(format!(
                        "预计结果约 {est:.3e} 位，超过上限（{} 位）。请降低指数或底数。",
                        MAX_DIGITS as u64
                    ));
                }
                (self.prog)(Stage::Ratio("精确乘方", 0, e.bit_len() as u64));
                let r = pow_int(&x, &e, &mut *self.prog)?;
                return Ok(Val::Int(r));
            }
            let dx = real_of(&x, self.work_bits);
            let dy = real_of(&y, self.work_bits);
            if is_integral(&dy) {
                let yf = dy.to_f64().value();
                if yf != 0.0 && yf.abs() <= 9.0e18 {
                    if dx == zero(self.work_bits) {
                        return Err("0 不能取负次幂。".into());
                    }
                    let r = pow_real_int(&dx, yf as i64, self.work_bits);
                    check_real_range(&r)?;
                    return Ok(Val::Dec(r));
                }
            }
            if dx <= zero(self.work_bits) {
                return Err("指数为小数时，底数必须大于 0。".into());
            }
            let r = (dx.ln() * dy).exp();
            check_real_range(&r)?;
            return Ok(Val::Dec(r));
        }
        let dx = va.into_dec(self.work_bits)?;
        let dy = vb.into_dec(self.work_bits)?;
        if is_integral(&dy) {
            let yf = dy.to_f64().value();
            if yf == 0.0 {
                if dx == zero(self.work_bits) {
                    return Err("0^0 未定义。".into());
                }
                return Ok(Val::Dec(one(self.work_bits)));
            }
            if yf.abs() <= 9.0e18 {
                if dx == zero(self.work_bits) {
                    if yf < 0.0 {
                        return Err("0 不能取负次幂。".into());
                    }
                    return Ok(Val::Dec(zero(self.work_bits)));
                }
                let r = pow_real_int(&dx, yf as i64, self.work_bits);
                check_real_range(&r)?;
                return Ok(Val::Dec(r));
            }
        }
        if dx <= zero(self.work_bits) {
            return Err("指数为小数时，底数必须大于 0。".into());
        }
        let r = (dx.ln() * dy).exp();
        check_real_range(&r)?;
        Ok(Val::Dec(r))
    }

    fn eval_sum(&mut self, body: &Node, var: &str, from: &Node, to: &Node) -> Result<Val, String> {
        let f = self.eval(from)?;
        let t = self.eval(to)?;
        let f = int_of(&f).ok_or("求和的起点必须是整数。")?;
        let t = int_of(&t).ok_or("求和的终点必须是整数。")?;
        let f = i64::try_from(&f).map_err(|_| "求和起点超出范围。".to_string())?;
        let t = i64::try_from(&t).map_err(|_| "求和终点超出范围。".to_string())?;
        // 用 i128 计算项数：i64::MIN..=i64::MAX 时 (t - f) 在 i64 下会溢出。
        let total = (i128::from(t) - i128::from(f)).unsigned_abs() + 1;
        if total > u128::from(MAX_SUM_TERMS) {
            return Err(format!("求和项数为 {total}，超过上限 {MAX_SUM_TERMS} 项。"));
        }
        let total = total as u64;
        let step: i64 = if t >= f { 1 } else { -1 };
        let mut acc = Val::Int(IBig::ZERO);
        let report_every = (total / 100).max(1);
        // 保存外层同名变量，循环结束后恢复；否则 sum(k,k,1,3)+k 会泄漏循环变量。
        let saved = self.vars.remove(var);
        for i in 0..total {
            // 由 f + step*i 直接计算（i128 中转后必然落在 i64 内），避免 i64 自增回绕。
            let k = i64::try_from(i128::from(f) + i128::from(step) * i128::from(i))
                .map_err(|_| "求和循环变量越界。".to_string())?;
            self.vars.insert(var.to_string(), Val::Int(IBig::from(k)));
            let v = self.eval(body)?;
            acc_add(&mut acc, v, self.work_bits, self.p)?;
            if i % report_every == 0 {
                (self.prog)(Stage::Ratio("求和", i + 1, total));
            }
            if i.is_multiple_of(64) && !matches!(acc, Val::Log(_)) {
                // Val::Log 是紧凑的对数表示，不参与 1e8 位十进制上限判定。
                if bit_digits_of_val(&acc) as f64 > MAX_DIGITS {
                    return Err(format!(
                        "求和结果超过上限（{} 位），请缩小范围。",
                        MAX_DIGITS as u64
                    ));
                }
            }
            // 时间预算逐次迭代检查：单次通项可能极贵，每 4096 次检查会让 120 秒形同虚设。
            if self.started.elapsed() > Duration::from_secs(MAX_SUM_SECONDS) {
                return Err(format!("求和超过 {MAX_SUM_SECONDS} 秒时间预算，已中止。"));
            }
        }
        if let Some(v) = saved {
            self.vars.insert(var.to_string(), v);
        } else {
            self.vars.remove(var);
        }
        let exact = matches!(acc, Val::Int(_));
        self.note = Some(format!(
            "求和：{var} 从 {f} 到 {t}，共 {total} 项，{}。",
            if exact {
                "逐项精确累加"
            } else {
                "高精度累加（近似）"
            }
        ));
        Ok(acc)
    }

    fn eval_diff(&mut self, body: &Node, var: &str, at: &Node) -> Result<Val, String> {
        let a = self.eval(at)?.into_dec(self.work_bits)?;
        let h0 = (&one(self.work_bits) / &real_of(&ten_pow(20), self.work_bits))
            .with_precision(self.work_bits)
            .value();
        let scale = if abs_real(&a) < one(self.work_bits) {
            one(self.work_bits)
        } else {
            abs_real(&a)
        };
        let h = (&h0 * &scale).with_precision(self.work_bits).value();
        let two_h = (&h + &h).with_precision(self.work_bits).value();
        let xm2 = (&a - &two_h).with_precision(self.work_bits).value();
        let xm1 = (&a - &h).with_precision(self.work_bits).value();
        let xp1 = (&a + &h).with_precision(self.work_bits).value();
        let xp2 = (&a + &two_h).with_precision(self.work_bits).value();
        let fm2 = self.eval_body_at(var, body, &xm2)?;
        let fm1 = self.eval_body_at(var, body, &xm1)?;
        let fp1 = self.eval_body_at(var, body, &xp1)?;
        let fp2 = self.eval_body_at(var, body, &xp2)?;
        let twelve_h = (&real_of(&IBig::from(12u8), self.work_bits) * &h)
            .with_precision(self.work_bits)
            .value();
        let diff_far = &fm2 - &fp2;
        let diff_near = &fp1 - &fm1;
        let eight = real_of(&IBig::from(8u8), self.work_bits);
        let numerator = diff_far + &eight * &diff_near;
        let deriv = numerator / twelve_h;
        self.note = Some(format!(
            "导数：d/d{var} 的中心差分数值微分，结果为 {} 位有效数字近似值。",
            self.p
        ));
        Ok(Val::Dec(deriv.with_precision(self.work_bits).value()))
    }

    fn eval_body_at(&mut self, var: &str, body: &Node, x: &Real) -> Result<Real, String> {
        let prev = self.vars.insert(var.to_string(), Val::Dec(x.clone()));
        let r = self.eval(body);
        match prev {
            Some(v) => {
                self.vars.insert(var.to_string(), v);
            }
            None => {
                self.vars.remove(var);
            }
        }
        r?.into_dec(self.work_bits)
    }

    fn eval_integ(
        &mut self,
        body: &Node,
        var: &str,
        from: &Bound,
        to: &Bound,
    ) -> Result<Val, String> {
        let lo = self.limit(from)?;
        let hi = self.limit(to)?;
        let p_eff = self.p.min(INTEG_DIGITS_CAP);
        let value = self.integrate(body, var, &lo, &hi, p_eff)?;
        let rounded = value.with_precision(digits_to_bits(p_eff)).value();
        let mut desc = format!(
            "定积分（tanh-sinh 双指数求积，{p_eff} 位有效数字，共 {} 次函数求值）",
            self.integ_evals
        );
        if self.p > INTEG_DIGITS_CAP {
            desc.push_str(&format!(
                "；数值积分精度上限为 {INTEG_DIGITS_CAP} 位有效数字"
            ));
        }
        self.note = Some(format!(
            "{}：估计误差 ≤ {}。",
            desc,
            real_text(&self.integ_error, 3)
        ));
        self.integ_extras = Some(vec![
            ("函数求值次数", self.integ_evals.to_string()),
            ("估计误差", real_text(&self.integ_error, 6)),
            ("自适应层数", self.integ_level.to_string()),
        ]);
        Ok(Val::Dec(rounded))
    }

    fn limit(&mut self, b: &Bound) -> Result<Lim, String> {
        match b {
            Bound::Inf(neg) => Ok(if *neg { Lim::NegInf } else { Lim::PosInf }),
            Bound::Expr(node) => {
                let d = self.eval(node)?.into_dec(self.work_bits)?;
                Ok(Lim::Val(d.with_precision(self.work_bits).value()))
            }
        }
    }

    fn integrate(
        &mut self,
        body: &Node,
        var: &str,
        lo: &Lim,
        hi: &Lim,
        p: usize,
    ) -> Result<Real, String> {
        // 不在内层重置 integ_evals：整次 eval 的所有积分共享 20 万次/120 秒预算，
        // 否则嵌套积分会把计数器反复打回小值（P1-06）。
        self.integ_error = zero(self.work_bits);
        self.integ_level = 0;
        match (lo, hi) {
            (Lim::NegInf, Lim::NegInf) | (Lim::PosInf, Lim::PosInf) => {
                Err("积分区间无效：两个端点不能同时为 inf。".into())
            }
            (Lim::Val(a), Lim::Val(b)) => {
                if a == b {
                    return Ok(zero(self.work_bits));
                }
                if a < b {
                    self.tanh_sinh(body, var, a, b, p)
                } else {
                    let r = self.tanh_sinh(body, var, b, a, p)?;
                    Ok(-r)
                }
            }
            (Lim::NegInf, Lim::Val(b)) => self.de_halfline(body, var, b, true, p),
            (Lim::Val(a), Lim::PosInf) => self.de_halfline(body, var, a, false, p),
            (Lim::NegInf, Lim::PosInf) => {
                let z = zero(self.work_bits);
                let l = self.de_halfline(body, var, &z, true, p)?;
                let r = self.de_halfline(body, var, &z, false, p)?;
                Ok((&l + &r).with_precision(self.work_bits).value())
            }
            (Lim::Val(a), Lim::NegInf) => {
                let r = self.de_halfline(body, var, a, true, p)?;
                Ok(-r)
            }
            (Lim::PosInf, Lim::Val(b)) => {
                let r = self.de_halfline(body, var, b, false, p)?;
                Ok(-r)
            }
            (Lim::PosInf, Lim::NegInf) => {
                let z = zero(self.work_bits);
                let l = self.de_halfline(body, var, &z, true, p)?;
                let r = self.de_halfline(body, var, &z, false, p)?;
                Ok(-(&l + &r).with_precision(self.work_bits).value())
            }
        }
    }

    fn integ_term(&mut self, body: &Node, var: &str, x: &Real) -> Result<Real, String> {
        self.integ_evals += 1;
        if self.integ_evals > INTEG_MAX_EVALS {
            return Err(format!(
                "数值积分超过 {INTEG_MAX_EVALS} 次函数求值，已中止；请检查函数或缩小积分区间。"
            ));
        }
        if self.integ_evals.is_multiple_of(4096)
            && self.started.elapsed() > Duration::from_secs(INTEG_SECONDS)
        {
            return Err(format!("数值积分超过 {INTEG_SECONDS} 秒时间预算，已中止。"));
        }
        self.eval_body_at(var, body, x)
    }

    fn tanh_sinh(
        &mut self,
        body: &Node,
        var: &str,
        a: &Real,
        b: &Real,
        p: usize,
    ) -> Result<Real, String> {
        let bits = self.work_bits;
        let pi_half = (&Real::pi(bits) / &real_of(&IBig::from(2u8), bits))
            .with_precision(bits)
            .value();
        let half = (&one(bits) / &real_of(&IBig::from(2u8), bits))
            .with_precision(bits)
            .value();
        let c = ((a + b) * &half).with_precision(bits).value();
        let s = ((b - a) * &half).with_precision(bits).value();
        let tail = (&one(bits) / &real_of(&ten_pow((p + 12) as u64), bits))
            .with_precision(bits)
            .value();
        let eps = (&one(bits) / &real_of(&ten_pow((p + 4) as u64), bits))
            .with_precision(bits)
            .value();
        let mut prev: Option<Real> = None;
        for level in 0..=MAX_INTEG_LEVEL {
            let two_real = real_of(&IBig::from(2u8), bits);
            let h = (&one(bits) / &pow_real_int(&two_real, level as i64, bits))
                .with_precision(bits)
                .value();
            let mut sum = zero(bits);
            let mut scale = zero(bits);
            let mut flat = 0u32;
            let mut j: i64 = 0;
            while j <= INTEG_MAX_NODES {
                let t = (real_of(&IBig::from(j as u64), bits) * &h)
                    .with_precision(bits)
                    .value();
                let sh = t.sinh();
                let arg = (&pi_half * &sh).with_precision(bits).value();
                if arg.to_f64().value().abs() > 1000.0 {
                    break;
                }
                let xj = arg.tanh();
                let ch = t.cosh();
                let w = ((&pi_half * &ch).with_precision(bits).value()
                    / (&arg.cosh() * &arg.cosh()).with_precision(bits).value())
                .with_precision(bits)
                .value();
                let plus = (&c + &(&s * &xj)).with_precision(bits).value();
                let f_plus = self.integ_term(body, var, &plus)?;
                let accounted = if j == 0 {
                    (&w * &f_plus).with_precision(bits).value()
                } else {
                    let minus = (&c - &(&s * &xj)).with_precision(bits).value();
                    let f_minus = self.integ_term(body, var, &minus)?;
                    let f_pair = (&f_plus + &f_minus).with_precision(bits).value();
                    (&w * &f_pair).with_precision(bits).value()
                };
                sum = (&sum + &accounted).with_precision(bits).value();
                if abs_real(&accounted) > scale {
                    scale = abs_real(&accounted);
                }
                let scale_ref = if scale < one(bits) {
                    one(bits)
                } else {
                    scale.clone()
                };
                let tiny = abs_real(&accounted) < (&tail * &scale_ref).with_precision(bits).value();
                if j > 0 && tiny {
                    flat += 1;
                    if flat >= 3 {
                        break;
                    }
                } else {
                    flat = 0;
                }
                j += 1;
            }
            let integral = (&sum * &h * &s).with_precision(bits).value();
            (self.prog)(Stage::Ratio(
                "数值积分",
                level as u64 + 1,
                MAX_INTEG_LEVEL as u64 + 1,
            ));
            self.integ_level = level + 1;
            if let Some(pv) = &prev {
                let d = abs_real(&(&integral - pv));
                let denom = abs_real(&integral);
                let denom = if denom < one(bits) { one(bits) } else { denom };
                let rel = (&d / &denom).with_precision(bits).value();
                self.integ_error = d;
                if rel < eps {
                    return Ok(integral);
                }
            }
            prev = Some(integral);
        }
        Err(format!(
            "数值积分在 {MAX_INTEG_LEVEL} 层内未收敛（估计误差 {}），请检查被积函数或降低有效数字。",
            real_text(&self.integ_error, 3)
        ))
    }

    fn de_halfline(
        &mut self,
        body: &Node,
        var: &str,
        base: &Real,
        left: bool,
        p: usize,
    ) -> Result<Real, String> {
        let bits = self.work_bits;
        let pi_half = (&Real::pi(bits) / &real_of(&IBig::from(2u8), bits))
            .with_precision(bits)
            .value();
        let tail = (&one(bits) / &real_of(&ten_pow((p + 12) as u64), bits))
            .with_precision(bits)
            .value();
        let eps = (&one(bits) / &real_of(&ten_pow((p + 4) as u64), bits))
            .with_precision(bits)
            .value();
        let s_cap = (p as f64 + 20.0) * std::f64::consts::LN_10;
        let u_cap = s_cap.asinh() + 1.0;
        let mut prev: Option<Real> = None;
        for level in 0..=MAX_INTEG_LEVEL {
            let two_real = real_of(&IBig::from(2u8), bits);
            let h = (&one(bits) / &pow_real_int(&two_real, level as i64, bits))
                .with_precision(bits)
                .value();
            let mut sum = zero(bits);
            let mut scale = zero(bits);
            let mut flat = 0u32;
            let mut j: i64 = 0;
            while j <= INTEG_MAX_NODES {
                let t = (real_of(&IBig::from(j as u64), bits) * &h)
                    .with_precision(bits)
                    .value();
                let sh = t.sinh();
                let u = (&pi_half * &sh).with_precision(bits).value();
                if u.to_f64().value().abs() > u_cap {
                    break;
                }
                let svar = u.sinh();
                let ch = t.cosh();
                let cosh_u = u.cosh();
                let w = ((&pi_half * &ch).with_precision(bits).value() * &cosh_u)
                    .with_precision(bits)
                    .value();
                let accounted = if j == 0 {
                    let e = one(bits);
                    let x = if left {
                        (base - &e).with_precision(bits).value()
                    } else {
                        (base + &e).with_precision(bits).value()
                    };
                    let fv = self.integ_term(body, var, &x)?;
                    (&w * &(&fv * &e).with_precision(bits).value())
                        .with_precision(bits)
                        .value()
                } else {
                    let e_pos = svar.exp();
                    let e_neg = (-svar.clone()).exp();
                    let x_pos = if left {
                        (base - &e_pos).with_precision(bits).value()
                    } else {
                        (base + &e_pos).with_precision(bits).value()
                    };
                    let x_neg = if left {
                        (base - &e_neg).with_precision(bits).value()
                    } else {
                        (base + &e_neg).with_precision(bits).value()
                    };
                    let f_pos = self.integ_term(body, var, &x_pos)?;
                    let f_neg = self.integ_term(body, var, &x_neg)?;
                    let g_pos = (&f_pos * &e_pos).with_precision(bits).value();
                    let g_neg = (&f_neg * &e_neg).with_precision(bits).value();
                    (&w * &(&g_pos + &g_neg).with_precision(bits).value())
                        .with_precision(bits)
                        .value()
                };
                sum = (&sum + &accounted).with_precision(bits).value();
                if abs_real(&accounted) > scale {
                    scale = abs_real(&accounted);
                }
                let scale_ref = if scale < one(bits) {
                    one(bits)
                } else {
                    scale.clone()
                };
                let tiny = abs_real(&accounted) < (&tail * &scale_ref).with_precision(bits).value();
                if j > 0 && tiny {
                    flat += 1;
                    if flat >= 3 {
                        break;
                    }
                } else {
                    flat = 0;
                }
                j += 1;
            }
            let integral = (&sum * &h).with_precision(bits).value();
            (self.prog)(Stage::Ratio(
                "数值积分",
                level as u64 + 1,
                MAX_INTEG_LEVEL as u64 + 1,
            ));
            self.integ_level = level + 1;
            if let Some(pv) = &prev {
                let d = abs_real(&(&integral - pv));
                let denom = abs_real(&integral);
                let denom = if denom < one(bits) { one(bits) } else { denom };
                let rel = (&d / &denom).with_precision(bits).value();
                self.integ_error = d;
                if rel < eps {
                    return Ok(integral);
                }
            }
            prev = Some(integral);
        }
        Err(format!(
            "数值积分（无穷限）在 {MAX_INTEG_LEVEL} 层内未收敛（估计误差 {}），请检查被积函数或降低有效数字。",
            real_text(&self.integ_error, 3)
        ))
    }
}

fn acc_add(acc: &mut Val, v: Val, bits: usize, p: usize) -> Result<(), String> {
    if is_zero_val(&v, bits) {
        return Ok(());
    }
    match acc {
        Val::Int(a) if a == &IBig::ZERO => {
            *acc = v;
            return Ok(());
        }
        Val::Dec(a) if a == &zero(bits) => {
            *acc = v;
            return Ok(());
        }
        _ => {}
    }
    if matches!(acc, Val::Log(_)) || matches!(v, Val::Log(_)) {
        let la = to_log(std::mem::replace(acc, Val::Int(IBig::ZERO)), bits)?;
        let lb = to_log(v, bits)?;
        *acc = Val::Log(log_add(&la, &lb, false, bits, p)?);
        return Ok(());
    }
    match v {
        Val::Int(b) => match acc {
            Val::Int(a) => {
                *a = &*a + &b;
            }
            Val::Dec(a) => {
                let bd = real_of(&b, bits);
                *a = (&*a + &bd).with_precision(bits).value();
            }
            _ => unreachable!(),
        },
        Val::Dec(b) => {
            let bd = b.with_precision(bits).value();
            match acc {
                Val::Int(a) => {
                    let ad = real_of(a, bits);
                    *acc = Val::Dec((ad + bd).with_precision(bits).value());
                }
                Val::Dec(a) => {
                    *a = (&*a + &bd).with_precision(bits).value();
                }
                _ => unreachable!(),
            }
        }
        Val::Log(_) => unreachable!(),
    }
    Ok(())
}

fn is_zero_val(v: &Val, bits: usize) -> bool {
    match v {
        Val::Int(i) => i == &IBig::ZERO,
        Val::Dec(d) => d == &zero(bits),
        Val::Log(_) => false,
    }
}

fn neg_val(v: Val, neg: bool) -> Result<Val, String> {
    if !neg {
        return Ok(v);
    }
    Ok(match v {
        Val::Int(i) => Val::Int(-i),
        Val::Dec(d) => Val::Dec(-d),
        Val::Log(mut l) => {
            l.sign = !l.sign;
            Val::Log(l)
        }
    })
}

fn bit_digits_of_val(v: &Val) -> usize {
    match v {
        Val::Int(i) => bit_digits(i),
        Val::Dec(d) => {
            (d.repr().significand().bit_len() as f64 * std::f64::consts::LOG10_2) as usize + 1
        }
        Val::Log(l) => {
            let f = l.log10.to_f64().value();
            if !f.is_finite() || f > 9.0 {
                MAX_DIGITS as usize + 1
            } else {
                10f64.powf(f) as usize + 1
            }
        }
    }
}

fn int_of(v: &Val) -> Option<IBig> {
    match v {
        Val::Int(i) => Some(i.clone()),
        _ => None,
    }
}

fn zero(bits: usize) -> Real {
    real_of(&IBig::ZERO, bits)
}

fn one(bits: usize) -> Real {
    real_of(&IBig::ONE, bits)
}

fn abs_real(x: &Real) -> Real {
    if x < &Real::ZERO {
        -x.clone()
    } else {
        x.clone()
    }
}

/// Real 是否处于安全可表示范围。
///
/// dashu 0.6 的 FBig（toward-zero 舍入）在指数溢出时会静默饱和成“指数 = isize::MAX
/// 的伪有限值”（is_infinite() 仍为 false），下溢则饱和为精确 0。合法输入不可能自然
/// 到达该饱和端点，因此直接以端点作为溢出判据。
///
/// 注意：不能用固定小阈值（如 1e12）判定，`exp(-2e12)` 这类合法中间值本身指数可达
/// 1e12 量级（数值积分等路径实测会因误杀而失败）；真正合法的指数上界只受 isize 约束。
fn real_in_range(x: &Real) -> bool {
    let repr = x.repr();
    !repr.is_infinite() && repr.exponent() < REAL_SATURATION_EXPONENT
}

/// 数值结果的范围守卫：dashu 溢出时会静默饱和为“伪有限值”，此处在进入 Val::Dec 前报错。
fn check_real_range(x: &Real) -> Result<(), String> {
    if real_in_range(x) {
        Ok(())
    } else {
        Err("结果超出可表示范围（指数溢出），请缩小数值或改写表达式。".into())
    }
}

fn is_integral(d: &Real) -> bool {
    let f = d.to_f64().value();
    f.is_finite() && f.fract() == 0.0
}

fn bit_digits(n: &IBig) -> usize {
    let abs = if n < &IBig::ZERO {
        -n.clone()
    } else {
        n.clone()
    };
    (abs.bit_len() as f64 * std::f64::consts::LOG10_2).ceil() as usize + 1
}

fn estimate_pow_digits(base: &IBig, exp: &UBig) -> Result<f64, String> {
    let abs = if base < &IBig::ZERO {
        -base.clone()
    } else {
        base.clone()
    };
    let d = Real::from(abs).with_precision(48).value();
    let lg = d.log10().to_f64().value();
    let e = u64::try_from(exp).map_err(|_| "指数过大。".to_string())? as f64;
    Ok(lg * e + 1.0)
}

fn estimate_factorial_digits(n: u64) -> f64 {
    if n < 2 {
        return 1.0;
    }
    let nf = n as f64;
    (nf * nf.ln() - nf + 0.5 * (2.0 * std::f64::consts::PI * nf).ln()) / std::f64::consts::LN_10
        + 1.0
}

fn factorial(n: u64) -> IBig {
    fn product(lo: u64, hi: u64) -> IBig {
        if lo > hi {
            return IBig::ONE;
        }
        if lo == hi {
            return IBig::from(lo);
        }
        if hi - lo == 1 {
            return IBig::from(lo) * IBig::from(hi);
        }
        let mid = lo + (hi - lo) / 2;
        &product(lo, mid) * &product(mid + 1, hi)
    }
    product(2, n)
}

fn log_factorial(n: &IBig, p: usize) -> Result<LogVal, String> {
    let internal = p + 50;
    let bits = digits_to_bits(internal);
    let nf = real_of(n, bits);
    let ln_n = nf.ln();
    let pi = Real::pi(bits);
    let two_pi_n = (((real_of(&IBig::from(2u8), bits) * &pi)
        .with_precision(bits)
        .value())
        * &nf)
        .with_precision(bits)
        .value();
    let half = (&one(bits) / &real_of(&IBig::from(2u8), bits))
        .with_precision(bits)
        .value();
    let mut ln_fact = ((&ln_n * &nf).with_precision(bits).value() - &nf)
        + (&half * &two_pi_n.ln()).with_precision(bits).value();
    let inv_n = (&one(bits) / &nf).with_precision(bits).value();
    let inv_n2 = (&inv_n * &inv_n).with_precision(bits).value();
    let threshold = (&one(bits) / &real_of(&ten_pow((p + 15) as u64), bits))
        .with_precision(bits)
        .value();
    let mut pow = inv_n.clone();
    let mut prev_mag = zero(bits);
    let mut converged = false;
    for k in 1..=(BERNOULLI_MAX_M / 2) {
        let (num, den) = bernoulli_term_pair(k)?;
        let c = (real_of(&num, bits) / real_of(&den, bits))
            .with_precision(bits)
            .value();
        let term = (&c * &pow).with_precision(bits).value();
        let mag = abs_real(&term);
        if k > 1 && mag > prev_mag {
            break;
        }
        prev_mag = mag.clone();
        ln_fact = (&ln_fact + &term).with_precision(bits).value();
        if mag < threshold {
            converged = true;
            break;
        }
        pow = (&pow * &inv_n2).with_precision(bits).value();
    }
    if !converged {
        return Err(format!(
            "该参数规模下 Stirling 级数无法达到 {p} 位有效数字，请降低有效数字后重试。"
        ));
    }
    let ln10 = real_of(&IBig::from(10u8), bits).ln();
    let log10_fact = (&ln_fact / &ln10).with_precision(bits).value();
    Ok(LogVal {
        sign: false,
        log10: log10_fact,
    })
}

fn log_outcome(lv: &LogVal, p: usize, started: Instant) -> Result<Outcome, String> {
    let internal = p + 50;
    let bits = digits_to_bits(internal);
    check_real_range(&lv.log10)?;
    let log10 = lv.log10.clone().with_precision(bits).value();
    let floor_val = log10.floor();
    let mut exp_int = integer_of_real(&floor_val)?;
    let frac = (&log10 - &floor_val).with_precision(bits).value();
    let ln10 = real_of(&IBig::from(10u8), bits).ln();
    let mant = (&frac * &ln10).exp().with_precision(bits).value();
    let (mant_text, carry) = mantissa_text(&mant, p, bits)?;
    if carry {
        exp_int = &exp_int + IBig::ONE;
    }
    let sign = if lv.sign { "-" } else { "" };
    let sci = format!("{sign}{}×10^{}", mant_text, exp_int);
    let log10_text = real_text(&log10, p);
    let ln_text = real_text(&(&log10 * &ln10).with_precision(bits).value(), p);
    let digits = &exp_int + IBig::ONE;
    let huge_positive = !lv.sign && exp_int >= IBig::from(6u8);
    let mut extras = Vec::new();
    if huge_positive {
        extras.push(make_extra("十进制位数（约）", digits.to_string()));
    }
    extras.push(make_extra("log10(|值|)", log10_text));
    extras.push(make_extra("ln(|值|)", ln_text));
    let note = if huge_positive {
        format!(
            "对数域结果：约 {digits} 位十进制数字。乘、除、乘方在对数域中保持 {p} 位有效数字；若表达式中含加/减，量级相差超过 {p} 位的小项会被舍入忽略。"
        )
    } else {
        format!("对数域结果（{p} 位有效数字）：乘、除、乘方保持精度；加/减按主项合成。")
    };
    Ok(Outcome {
        kind: "approx",
        exact: false,
        full: sci.clone(),
        head: sci.clone(),
        tail: String::new(),
        omitted: 0,
        digits: p,
        scientific: Some(sci),
        extras,
        note: Some(note),
        ms: started.elapsed().as_millis(),
        headline: if huge_positive {
            Some(digits.to_string())
        } else {
            None
        },
        unit: if huge_positive {
            Some("位十进制数字（约）")
        } else {
            None
        },
    })
}

fn real_text(x: &Real, p: usize) -> String {
    let dec: Decimal = x.to_decimal().value().with_precision(p).value();
    let (plain, sci) = fmt_decimal(&dec);
    plain.unwrap_or(sci)
}

fn mantissa_text(mant: &Real, p: usize, bits: usize) -> Result<(String, bool), String> {
    let text_of = |value: &Real| -> String {
        let dec: Decimal = value.to_decimal().value().with_precision(p).value();
        let repr = dec.repr();
        let sig = repr.significand().to_string();
        let neg = sig.starts_with('-');
        let ds: String = sig.trim_start_matches('-').to_string();
        let text = if ds.len() > 1 {
            format!("{}.{}", &ds[..1], &ds[1..])
        } else {
            ds
        };
        if neg { format!("-{text}") } else { text }
    };
    let dec: Decimal = mant.to_decimal().value().with_precision(p).value();
    let repr = dec.repr();
    let sig_len = repr.significand().to_string().trim_start_matches('-').len() as isize;
    let point = sig_len + repr.exponent();
    if point >= 2 {
        let shifted = (mant.clone() / real_of(&ten_pow((point - 1) as u64), bits))
            .with_precision(bits)
            .value();
        Ok((text_of(&shifted), true))
    } else if point <= 0 {
        let shifted = (mant.clone() * real_of(&ten_pow((1 - point) as u64), bits))
            .with_precision(bits)
            .value();
        Ok((text_of(&shifted), false))
    } else {
        Ok((text_of(mant), false))
    }
}

fn integer_of_real(x: &Real) -> Result<IBig, String> {
    let repr = x.repr();
    let sig = repr.significand().clone();
    let exp = repr.exponent();
    if sig == IBig::ZERO {
        return Ok(IBig::ZERO);
    }
    if exp >= 0 {
        Ok(&sig << (exp as usize))
    } else {
        let sh = (-exp) as usize;
        let mask = (IBig::ONE << sh) - IBig::ONE;
        if (&sig & &mask) != IBig::ZERO {
            return Err("内部错误：整数转换失败。".into());
        }
        Ok(&sig >> sh)
    }
}

#[derive(Clone, Debug)]
struct Frac {
    n: IBig,
    d: IBig,
}

fn gcd_ibig(a: &IBig, b: &IBig) -> IBig {
    let mut x = if a < &IBig::ZERO {
        -a.clone()
    } else {
        a.clone()
    };
    let mut y = if b < &IBig::ZERO {
        -b.clone()
    } else {
        b.clone()
    };
    while y != IBig::ZERO {
        let r = &x % &y;
        x = y;
        y = r;
    }
    if x == IBig::ZERO { IBig::ONE } else { x }
}

fn frac_reduce(n: IBig, d: IBig) -> Frac {
    let mut n = n;
    let mut d = d;
    if d < IBig::ZERO {
        n = -n;
        d = -d;
    }
    if n == IBig::ZERO {
        return Frac { n, d: IBig::ONE };
    }
    let g = gcd_ibig(&n, &d);
    if g != IBig::ONE {
        n = &n / &g;
        d = &d / &g;
    }
    Frac { n, d }
}

fn frac_add(a: &Frac, b: &Frac) -> Frac {
    frac_reduce(&a.n * &b.d + &b.n * &a.d, &a.d * &b.d)
}

fn frac_mul_int(a: &Frac, k: &IBig) -> Frac {
    frac_reduce(&a.n * k, a.d.clone())
}

static BERNOULLI_CACHE: OnceLock<Mutex<Vec<Frac>>> = OnceLock::new();

fn bernoulli(m: usize) -> Result<Frac, String> {
    if m > BERNOULLI_MAX_M {
        return Err("Bernoulli 数表超出范围。".into());
    }
    let cache = BERNOULLI_CACHE.get_or_init(|| {
        Mutex::new(vec![Frac {
            n: IBig::ONE,
            d: IBig::ONE,
        }])
    });
    let mut v = cache
        .lock()
        .map_err(|_| "内部错误：缓存锁失败。".to_string())?;
    while v.len() <= m {
        let mm = v.len();
        let mut sum = Frac {
            n: IBig::ZERO,
            d: IBig::ONE,
        };
        let mut c = IBig::ONE;
        for k in 0..mm {
            sum = frac_add(&sum, &frac_mul_int(&v[k], &c));
            if k + 1 < mm {
                c = (&c * IBig::from(mm + 1 - k)) / IBig::from(k + 1);
            }
        }
        let b = frac_reduce(-sum.n, &sum.d * IBig::from(mm + 1));
        v.push(b);
    }
    Ok(v[m].clone())
}

fn bernoulli_term_pair(k: usize) -> Result<(IBig, IBig), String> {
    let b = bernoulli(2 * k)?;
    let den = &b.d * IBig::from((2 * k * (2 * k - 1)) as u64);
    Ok((b.n, den))
}

fn pow_real_int(base: &Real, exp: i64, bits: usize) -> Real {
    let neg = exp < 0;
    let mut e = exp.unsigned_abs();
    let mut result = one(bits);
    let mut b = base.clone().with_precision(bits).value();
    while e > 0 {
        if e & 1 == 1 {
            result = (&result * &b).with_precision(bits).value();
        }
        e >>= 1;
        if e > 0 {
            b = (&b * &b).with_precision(bits).value();
        }
    }
    if neg {
        (&one(bits) / &result).with_precision(bits).value()
    } else {
        result
    }
}

fn pow_int(base: &IBig, exp: &UBig, prog: &mut dyn FnMut(Stage)) -> Result<IBig, String> {
    if exp == &UBig::ZERO {
        return Ok(IBig::ONE);
    }
    let bits = exp.bit_len();
    let neg = base < &IBig::ZERO && exp.bit(0);
    let abs_base = if base < &IBig::ZERO {
        -base.clone()
    } else {
        base.clone()
    };
    let mut result = IBig::ONE;
    for i in (0..bits).rev() {
        prog(Stage::Ratio("精确乘方", (bits - 1 - i) as u64, bits as u64));
        result = &result * &result;
        if exp.bit(i) {
            result = &result * &abs_base;
        }
    }
    Ok(if neg { -result } else { result })
}

fn parse(src: &str, work_bits: usize) -> Result<Node, String> {
    let toks = tokenize(src)?;
    if toks.is_empty() {
        return Err("表达式为空。".into());
    }
    let mut p = Parser {
        toks,
        pos: 0,
        work_bits,
        depth: 0,
    };
    let node = p.parse_expr()?;
    if p.pos < p.toks.len() {
        return Err(format!("第 {} 列：多余的输入。", p.toks[p.pos].1 + 1));
    }
    Ok(node)
}

#[derive(Clone, PartialEq)]
enum Tok {
    Num(String),
    Ident(String),
    Sym(char),
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
    work_bits: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.0)
    }

    fn col(&self) -> usize {
        self.toks
            .get(self.pos)
            .map(|t| t.1 + 1)
            .unwrap_or(usize::MAX)
    }

    fn eat_sym(&mut self, c: char) -> bool {
        if matches!(self.peek(), Some(Tok::Sym(s)) if *s == c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_sym(&mut self, c: char) -> Result<(), String> {
        if self.eat_sym(c) {
            Ok(())
        } else {
            Err(format!("第 {} 列：缺少“{c}”。", self.col()))
        }
    }

    /// 递归下降的深度守卫：每个递归入口在进入时计数，超过上限返回错误而非耗尽栈。
    fn with_depth<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            return Err(format!("表达式嵌套过深（上限 {MAX_PARSE_DEPTH} 层）。"));
        }
        let r = f(self);
        self.depth -= 1;
        r
    }

    fn parse_expr(&mut self) -> Result<Node, String> {
        self.with_depth(|p| p.parse_expr_inner())
    }

    fn parse_expr_inner(&mut self) -> Result<Node, String> {
        let mut left = self.parse_term()?;
        loop {
            if self.eat_sym('+') {
                let right = self.parse_term()?;
                left = Node::Bin(Bin::Add, Box::new(left), Box::new(right));
            } else if self.eat_sym('-') {
                let right = self.parse_term()?;
                left = Node::Bin(Bin::Sub, Box::new(left), Box::new(right));
            } else {
                return Ok(left);
            }
        }
    }

    fn starts_value(&self) -> bool {
        matches!(self.peek(), Some(Tok::Num(_)) | Some(Tok::Ident(_)))
            || matches!(self.peek(), Some(Tok::Sym('(')))
    }

    fn parse_term(&mut self) -> Result<Node, String> {
        self.with_depth(|p| p.parse_term_inner())
    }

    fn parse_term_inner(&mut self) -> Result<Node, String> {
        let mut left = self.parse_unary()?;
        loop {
            let node = if self.eat_sym('*') {
                let right = self.parse_unary()?;
                Node::Bin(Bin::Mul, Box::new(left), Box::new(right))
            } else if self.eat_sym('/') {
                let right = self.parse_unary()?;
                Node::Bin(Bin::Div, Box::new(left), Box::new(right))
            } else if self.eat_sym('%') {
                let right = self.parse_unary()?;
                Node::Bin(Bin::Mod, Box::new(left), Box::new(right))
            } else if self.starts_value() {
                let right = self.parse_unary()?;
                Node::Bin(Bin::Mul, Box::new(left), Box::new(right))
            } else {
                return Ok(left);
            };
            left = node;
        }
    }

    fn parse_unary(&mut self) -> Result<Node, String> {
        self.with_depth(|p| p.parse_unary_inner())
    }

    fn parse_unary_inner(&mut self) -> Result<Node, String> {
        if self.eat_sym('-') {
            let inner = self.parse_unary()?;
            return Ok(Node::Neg(Box::new(inner)));
        }
        if self.eat_sym('+') {
            return self.parse_unary();
        }
        self.parse_power()
    }

    fn parse_power(&mut self) -> Result<Node, String> {
        self.with_depth(|p| p.parse_power_inner())
    }

    fn parse_power_inner(&mut self) -> Result<Node, String> {
        let base = self.parse_postfix()?;
        if self.eat_sym('^') {
            let exp = self.parse_unary()?;
            return Ok(Node::Bin(Bin::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn parse_postfix(&mut self) -> Result<Node, String> {
        self.with_depth(|p| p.parse_postfix_inner())
    }

    fn parse_postfix_inner(&mut self) -> Result<Node, String> {
        let mut node = self.parse_primary()?;
        while self.eat_sym('!') {
            node = Node::Fact(Box::new(node));
        }
        Ok(node)
    }

    fn parse_primary(&mut self) -> Result<Node, String> {
        self.with_depth(|p| p.parse_primary_inner())
    }

    fn parse_primary_inner(&mut self) -> Result<Node, String> {
        let col = self.col();
        match self.peek().cloned() {
            Some(Tok::Num(text)) => {
                self.pos += 1;
                parse_number(&text, self.work_bits, col)
            }
            Some(Tok::Ident(name)) => {
                self.pos += 1;
                if self.eat_sym('(') {
                    return self.parse_call(&name, col);
                }
                match name.as_str() {
                    "e" => Ok(Node::ConstE),
                    "pi" => Ok(Node::ConstPi),
                    "inf" => Ok(Node::ConstInf),
                    _ if name.len() == 1 => Ok(Node::Var(name)),
                    _ => Err(format!("第 {col} 列：未知标识“{name}”。")),
                }
            }
            Some(Tok::Sym('(')) => {
                self.pos += 1;
                let inner = self.parse_expr()?;
                self.expect_sym(')')?;
                Ok(inner)
            }
            _ => Err(format!("第 {col} 列：缺少数值或括号。")),
        }
    }

    fn parse_call(&mut self, name: &str, col: usize) -> Result<Node, String> {
        self.with_depth(|p| p.parse_call_inner(name, col))
    }

    fn parse_call_inner(&mut self, name: &str, col: usize) -> Result<Node, String> {
        match name {
            "ln" => {
                let x = self.parse_expr()?;
                self.expect_sym(')')?;
                Ok(Node::Ln(Box::new(x)))
            }
            "log" => {
                let x = self.parse_expr()?;
                if self.eat_sym(',') {
                    let b = self.parse_expr()?;
                    self.expect_sym(')')?;
                    Ok(Node::LogB(Box::new(x), Box::new(b)))
                } else {
                    self.expect_sym(')')?;
                    Ok(Node::Log10(Box::new(x)))
                }
            }
            "sum" => {
                let body = self.parse_expr()?;
                self.expect_sym(',')?;
                let var = self.parse_var_name()?;
                self.expect_sym(',')?;
                let from = self.parse_expr()?;
                self.expect_sym(',')?;
                let to = self.parse_expr()?;
                self.expect_sym(')')?;
                Ok(Node::Sum {
                    body: Box::new(body),
                    var,
                    from: Box::new(from),
                    to: Box::new(to),
                })
            }
            "diff" => {
                let body = self.parse_expr()?;
                self.expect_sym(',')?;
                let var = self.parse_var_name()?;
                self.expect_sym(',')?;
                let at = self.parse_expr()?;
                self.expect_sym(')')?;
                Ok(Node::Diff {
                    body: Box::new(body),
                    var,
                    at: Box::new(at),
                })
            }
            "integ" | "int" => {
                let body = self.parse_expr()?;
                self.expect_sym(',')?;
                let var = self.parse_var_name()?;
                self.expect_sym(',')?;
                let from = classify_bound(self.parse_expr()?);
                self.expect_sym(',')?;
                let to = classify_bound(self.parse_expr()?);
                self.expect_sym(')')?;
                Ok(Node::Integ {
                    body: Box::new(body),
                    var,
                    from,
                    to,
                })
            }
            _ => Err(format!("第 {col} 列：未知函数“{name}”。")),
        }
    }

    fn parse_var_name(&mut self) -> Result<String, String> {
        match self.peek().cloned() {
            Some(Tok::Ident(name)) if name.len() == 1 => {
                self.pos += 1;
                Ok(name)
            }
            _ => Err(format!("第 {} 列：此处应为变量名。", self.col())),
        }
    }
}

fn classify_bound(node: Node) -> Bound {
    match node {
        Node::ConstInf => Bound::Inf(false),
        Node::Neg(inner) => match *inner {
            Node::ConstInf => Bound::Inf(true),
            other => Bound::Expr(Box::new(Node::Neg(Box::new(other)))),
        },
        other => Bound::Expr(Box::new(other)),
    }
}

fn parse_number(text: &str, work_bits: usize, col: usize) -> Result<Node, String> {
    let (mant_text, exp) = match text.find(['e', 'E']) {
        Some(pos) => {
            let e: i64 = text[pos + 1..]
                .parse()
                .map_err(|_| format!("第 {col} 列：数值指数无效。"))?;
            (&text[..pos], e)
        }
        None => (text, 0),
    };
    let (digits, frac_len) = match mant_text.find('.') {
        Some(dot) => {
            let int_part = &mant_text[..dot];
            let frac_part = &mant_text[dot + 1..];
            if int_part.is_empty() && frac_part.is_empty() {
                return Err(format!("第 {col} 列：数值无效。"));
            }
            (format!("{int_part}{frac_part}"), frac_part.len() as i64)
        }
        None => {
            if mant_text.is_empty() {
                return Err(format!("第 {col} 列：数值无效。"));
            }
            (mant_text.to_string(), 0)
        }
    };
    let e = exp
        .checked_sub(frac_len)
        .ok_or_else(|| format!("第 {col} 列：数值指数过大。"))?;
    if e.unsigned_abs() > MAX_LITERAL_EXP as u64 {
        return Err(format!("第 {col} 列：数值指数过大。"));
    }
    let mant = IBig::from_str(&digits).map_err(|_| format!("第 {col} 列：数值无效。"))?;
    if e >= 0 {
        let v = if e == 0 {
            mant
        } else {
            mant * ten_pow(e as u64)
        };
        Ok(Node::Int(v))
    } else {
        let p = ten_pow((-e) as u64);
        let q = &mant / &p;
        let r = &mant % &p;
        if r == IBig::ZERO {
            Ok(Node::Int(q))
        } else {
            let v = real_of(&mant, work_bits) / real_of(&p, work_bits);
            Ok(Node::Dec(v.with_precision(work_bits).value()))
        }
    }
}

fn tokenize(src: &str) -> Result<Vec<(Tok, usize)>, String> {
    let chars: Vec<char> = src.chars().collect();
    let normalized = normalize(src);
    let nchars: Vec<char> = normalized.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < nchars.len() {
        let c = nchars[i];
        if c == '.' && i + 1 < nchars.len() && nchars[i + 1].is_ascii_digit() {
            let (tok, next) = lex_number(&nchars, i);
            out.push((tok, i));
            i = next;
            continue;
        }
        if c.is_ascii_digit() {
            let (tok, next) = lex_number(&nchars, i);
            out.push((tok, i));
            i = next;
            continue;
        }
        if c.is_ascii_alphabetic() {
            let start = i;
            while i < nchars.len() && nchars[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word: String = nchars[start..i].iter().collect::<String>().to_lowercase();
            out.push((Tok::Ident(word), start));
            continue;
        }
        match c {
            '+' | '-' | '*' | '/' | '%' | '^' | '!' | '(' | ')' | ',' => {
                out.push((Tok::Sym(c), i));
                i += 1;
            }
            _ => {
                return Err(format!(
                    "第 {} 列：无法识别的字符“{c}”（原字符“{}”）。",
                    i + 1,
                    chars.get(i).copied().unwrap_or(c)
                ));
            }
        }
    }
    Ok(out)
}

fn lex_number(chars: &[char], start: usize) -> (Tok, usize) {
    let mut i = start;
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    if i < chars.len() && chars[i] == '.' {
        i += 1;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
        let mut j = i + 1;
        if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
            j += 1;
        }
        if j < chars.len() && chars[j].is_ascii_digit() {
            i = j;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    let text: String = chars[start..i].iter().collect();
    (Tok::Num(text), i)
}

fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let c = match ch {
            '０'..='９' => char::from_u32(ch as u32 - 0xFEE0).unwrap_or(ch),
            '－' | '−' => '-',
            '＋' => '+',
            '．' => '.',
            '×' | '✕' | '✖' => '*',
            '÷' => '/',
            '＾' => '^',
            '！' => '!',
            '％' => '%',
            '（' => '(',
            '）' => ')',
            '，' => ',',
            '　' => ' ',
            'π' => {
                out.push_str("pi");
                continue;
            }
            'Σ' => {
                out.push_str("sum");
                continue;
            }
            other => other,
        };
        if c.is_whitespace() || c == '_' || c == '\'' {
            continue;
        }
        out.push(c);
    }
    out
}

fn int_outcome(value: IBig, started: Instant, note: Option<String>) -> Outcome {
    let full = value.to_string();
    let neg = full.starts_with('-');
    let digits = if neg { full.len() - 1 } else { full.len() };
    let (head, tail, omitted) = split_preview(&full);
    let scientific = if digits > 41 {
        let ds = if neg { &full[1..] } else { &full[..] };
        let take = ds.len().min(22);
        let m = if take > 1 {
            format!("{}.{}", &ds[..1], &ds[1..take])
        } else {
            ds.to_string()
        };
        Some(format!(
            "{}{}×10^{}",
            if neg { "-" } else { "" },
            m,
            digits - 1
        ))
    } else {
        None
    };
    Outcome {
        kind: "int",
        exact: true,
        full,
        head,
        tail,
        omitted,
        digits,
        scientific,
        extras: Vec::new(),
        note,
        ms: started.elapsed().as_millis(),
        headline: None,
        unit: None,
    }
}

fn approx_outcome(value: &Real, p: usize, desc: &'static str, started: Instant) -> Outcome {
    let dec: Decimal = value.to_decimal().value().with_precision(p).value();
    let (plain, sci) = fmt_decimal(&dec);
    let text = plain.unwrap_or_else(|| sci.clone());
    let (head, tail, omitted) = split_preview(&text);
    Outcome {
        kind: "approx",
        exact: false,
        full: text,
        head,
        tail,
        omitted,
        digits: p,
        scientific: Some(sci),
        extras: Vec::new(),
        note: Some(format!("{desc}（近似值）")),
        ms: started.elapsed().as_millis(),
        headline: None,
        unit: None,
    }
}

fn fmt_decimal(d: &Decimal) -> (Option<String>, String) {
    let repr = d.repr();
    let sig = repr.significand().to_string();
    let (neg, ds) = match sig.strip_prefix('-') {
        Some(rest) => (true, rest.to_string()),
        None => (false, sig.clone()),
    };
    let sign = if neg { "-" } else { "" };
    let point = ds.len() as isize + repr.exponent();
    let mantissa = if ds.len() > 1 {
        format!("{}.{}", &ds[..1], &ds[1..])
    } else {
        ds.clone()
    };
    let sci = format!("{}{}×10^{}", sign, mantissa, point - 1);
    let plain = if point <= 0 {
        if point >= -6 {
            Some(format!("{}0.{}{}", sign, "0".repeat((-point) as usize), ds))
        } else {
            None
        }
    } else if point <= 40 {
        if point as usize >= ds.len() {
            Some(format!(
                "{}{}{}",
                sign,
                ds,
                "0".repeat(point as usize - ds.len())
            ))
        } else {
            Some(format!(
                "{}{}.{}",
                sign,
                &ds[..point as usize],
                &ds[point as usize..]
            ))
        }
    } else {
        None
    };
    (plain, sci)
}

fn make_extra(label: &'static str, full: String) -> Extra {
    let (head, tail, omitted) = split_preview(&full);
    Extra {
        label,
        full,
        head,
        tail,
        omitted,
    }
}

fn split_preview(s: &str) -> (String, String, usize) {
    let n = s.len();
    if n <= PREVIEW_SIDE * 2 + 64 {
        (s.to_string(), String::new(), 0)
    } else {
        let head = s[..PREVIEW_SIDE].to_string();
        let tail = s[n - PREVIEW_SIDE..].to_string();
        (head, tail, n - PREVIEW_SIDE * 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(expr: &str, p: usize) -> Outcome {
        let mut noop = |_s: Stage| {};
        eval(expr, p, &mut noop).expect("eval failed")
    }

    fn run_err(expr: &str, p: usize) -> String {
        let mut noop = |_s: Stage| {};
        eval(expr, p, &mut noop).expect_err("expected error")
    }

    #[test]
    fn pow_three_million_digits() {
        let out = run("10^3000000", 60);
        assert_eq!(out.kind, "int");
        assert_eq!(out.digits, 3_000_001);
        assert!(out.full.starts_with('1'));
        assert!(out.full[1..].chars().all(|c| c == '0'));
        assert!(out.omitted > 0);
    }

    #[test]
    fn factorial_small_and_large() {
        let out = run("10!", 60);
        assert_eq!(out.full, "3628800");
        let out = run("1000!", 60);
        assert_eq!(out.digits, 2568);
        assert!(out.full.starts_with("40238726007709377354"));
        let out = run("0!", 60);
        assert_eq!(out.full, "1");
    }

    #[test]
    fn summation_exact_and_approx() {
        let out = run("sum(k^2, k, 1, 100)", 60);
        assert_eq!(out.full, "338350");
        let out = run("sum(1/k, k, 1, 4)", 40);
        assert!(out.head.starts_with("2.0833333"), "got {}", out.head);
        let out = run("sum(k, k, 5, 1)", 40);
        assert_eq!(out.full, "15");
        let out2 = run("sum(k, k, 1, 5)", 40);
        assert_eq!(out2.full, "15");
    }

    #[test]
    fn derivative_numeric() {
        let out = run("diff(x^3, x, 2)", 40);
        let v = parse_plain_f64(&out);
        assert!((v - 12.0).abs() < 1e-10, "got {v}");
        let out = run("diff(x^2, x, 0)", 40);
        let v = parse_plain_f64(&out);
        assert!(v.abs() < 1e-10, "got {v}");
    }

    #[test]
    fn logarithms_and_constants() {
        let out = run("ln(e)", 40);
        let v = parse_plain_f64(&out);
        assert!((v - 1.0).abs() < 1e-30, "got {v}");
        let out = run("log(8, 2)", 40);
        let v = parse_plain_f64(&out);
        assert!((v - 3.0).abs() < 1e-30, "got {v}");
        let out = run("pi", 40);
        assert!(out.head.starts_with("3.14159265358979"), "got {}", out.head);
        let out = run("2π", 40);
        assert!(out.head.starts_with("6.28318530717958"), "got {}", out.head);
        let out = run("ln(2)", 40);
        assert!(
            out.head.starts_with("0.693147180559945"),
            "got {}",
            out.head
        );
    }

    #[test]
    fn huge_factorial_uses_log_path() {
        let out = run("6.02e23!", 60);
        assert_eq!(out.kind, "approx");
        let headline = out.headline.clone().expect("headline");
        assert!(headline.starts_with("1405"), "got {headline}");
        assert!(headline.len() >= 24, "got {headline}");
        assert_eq!(out.extras[0].label, "十进制位数（约）");
        assert_eq!(out.extras[0].full, headline);
        let sci = out.scientific.clone().expect("sci");
        assert!(sci.starts_with("5.626"), "got {sci}");
        let exp_part = sci.split('×').nth(1).expect("exponent");
        assert!(exp_part.starts_with("10^14053"), "got {exp_part}");
        assert!(
            out.extras[1].full.starts_with("1405387180963145"),
            "got {}",
            out.extras[1].full
        );
        assert!(
            out.extras[2].full.starts_with("3236023572770663"),
            "got {}",
            out.extras[2].full
        );
        assert_eq!(out.unit, Some("位十进制数字（约）"));
    }

    #[test]
    fn log_factorial_matches_exact() {
        let n = IBig::from(100_000u64);
        let lv = log_factorial(&n, 60).expect("log factorial");
        let out = log_outcome(&lv, 60, Instant::now()).expect("log outcome");
        let exact = factorial(100_000);
        let bits = digits_to_bits(120);
        let exact_log10 = real_text(&real_of(&exact, bits).log10(), 30);
        let exact_ln = real_text(&real_of(&exact, bits).ln(), 30);
        let prefix = |s: &str, n: usize| s.chars().take(n).collect::<String>();
        assert_eq!(out.extras[0].full, exact.to_string().len().to_string());
        assert_eq!(prefix(&out.extras[1].full, 24), prefix(&exact_log10, 24));
        assert_eq!(prefix(&out.extras[2].full, 24), prefix(&exact_ln, 24));
    }

    #[test]
    fn log_domain_arithmetic() {
        let prefix = |s: &str, n: usize| s.chars().take(n).collect::<String>();
        let base = run("6.02e23!", 60);
        let round = run("6.02e23! * 2 / 2", 60);
        assert_eq!(
            prefix(base.extras[1].full.as_str(), 50),
            prefix(round.extras[1].full.as_str(), 50)
        );
        let ratio = run("6.02e23! / 6.02e22!", 60);
        assert!(
            ratio
                .headline
                .clone()
                .expect("headline")
                .starts_with("1270")
        );
        let square = run("(6.02e23!)^2", 60);
        assert!(
            square
                .headline
                .clone()
                .expect("headline")
                .starts_with("2810")
        );
        let ln = run("ln(6.02e23!)", 60);
        assert!(
            ln.full.starts_with("32360235727706635802228723"),
            "got {}",
            ln.full
        );
        let plus = run("6.02e23! + 1", 60);
        assert_eq!(
            prefix(base.scientific.as_deref().unwrap(), 40),
            prefix(plus.scientific.as_deref().unwrap(), 40)
        );
        let same = run("6.02e23! / 6.02e23!", 60);
        assert!(same.full.starts_with('1'), "got {}", same.full);
        assert!(same.full.ends_with("×10^0"), "got {}", same.full);
        assert!(run_err("6.02e23! - 6.02e23!", 60).contains("无法"));
        let multinomial = run("1e20!/(1e19!*1e19!)", 60);
        let lg: f64 = multinomial.extras[1].full.parse().expect("parse log10");
        assert!((1.4e21..1.7e21).contains(&lg), "got {lg}");
    }

    #[test]
    fn definite_integrals() {
        let out = run("integ(x^2, x, 0, 1)", 40);
        let v = parse_plain_f64(&out);
        assert!((v - 1.0 / 3.0).abs() < 1e-12, "got {v}");
        let out = run("integ(x^3, x, 0, 2)", 40);
        let v = parse_plain_f64(&out);
        assert!((v - 4.0).abs() < 1e-10, "got {v}");
        let out = run("integ(1/x, x, 1, e)", 40);
        let v = parse_plain_f64(&out);
        assert!((v - 1.0).abs() < 1e-10, "got {v}");
        let out = run("integ(e^(-x^2), x, -inf, inf)", 40);
        let v = parse_plain_f64(&out);
        assert!((v - std::f64::consts::PI.sqrt()).abs() < 1e-10, "got {v}");
        let out = run("integ(1/x^2, x, 1, inf)", 40);
        let v = parse_plain_f64(&out);
        assert!((v - 1.0).abs() < 1e-8, "got {v}");
        let out = run("integ(x, x, 1, 0)", 40);
        let v = parse_plain_f64(&out);
        assert!((v + 0.5).abs() < 1e-10, "got {v}");
        assert!(run_err("integ(x, x, inf, inf)", 40).contains("区间"));
        assert!(run_err("inf + 1", 40).contains("积分"));
    }

    fn parse_plain_f64(out: &Outcome) -> f64 {
        let text = if out.tail.is_empty() {
            out.head.clone()
        } else {
            let mut s = out.head.clone();
            s.push_str(&out.tail);
            s
        };
        text.trim_start_matches('-')
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect::<String>()
            .parse()
            .unwrap_or(f64::NAN)
            * if text.starts_with('-') { -1.0 } else { 1.0 }
    }

    #[test]
    fn division_with_remainder_extras() {
        let out = run("10/3", 50);
        assert_eq!(out.kind, "approx");
        assert!(out.head.starts_with("3.3333333"));
        assert_eq!(out.extras[0].full, "3");
        assert_eq!(out.extras[1].full, "1");
        let out = run("10 % 3", 50);
        assert_eq!(out.full, "1");
    }

    #[test]
    fn implicit_multiplication_and_scientific() {
        let out = run("2(3+4)", 40);
        assert_eq!(out.full, "14");
        let out = run("1e6 + 1", 40);
        assert_eq!(out.full, "1000001");
        let out = run("2.5e3", 40);
        assert_eq!(out.full, "2500");
        let out = run("2^-10", 40);
        assert!(out.head.starts_with("0.0009765625"), "got {}", out.head);
    }

    #[test]
    fn errors_are_reported() {
        assert!(run_err("1/0", 40).contains("除数"));
        assert!(run_err("(-1)!", 40).contains("阶乘"));
        assert!(run_err("sum(k, k, 1, 99999999)", 40).contains("上限"));
        assert!(run_err("x+1", 40).contains("未知变量"));
    }

    #[test]
    fn parser_depth_guard_reports_errors() {
        // 括号嵌套 200/500/1000 层：必须返回错误，绝不能栈溢出 abort。
        for depth in [200usize, 500, 1000] {
            let expr = format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
            let err = run_err(&expr, 40);
            assert!(err.contains("嵌套过深"), "depth {depth}: got {err}");
        }
        // 一元负号链与 `^` 右结合链同样受限。
        assert!(run_err(&format!("{}1", "-".repeat(2000)), 40).contains("嵌套过深"));
        assert!(run_err(&format!("{}1", "2^".repeat(1000)), 40).contains("嵌套过深"));
        // 正常浅层嵌套不能被误杀。
        let ok = format!("{}1{}", "(".repeat(30), ")".repeat(30));
        assert_eq!(run(&ok, 40).full, "1");
    }

    #[test]
    fn deep_chains_do_not_abort_on_worker_stack() {
        // 复刻外壳 main.rs 的 2 MiB 工作线程栈：解析可以迭代建树，
        // 但求值与 Node 的递归析构都会消耗栈，必须在守卫/长度上限下安全返回。
        let handle = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let mut noop = |_s: Stage| {};
                let add_chain = format!("1{}", "+1".repeat(1999));
                assert!(add_chain.chars().count() <= MAX_EXPR_CHARS);
                assert!(
                    eval(&add_chain, 60, &mut noop).is_err(),
                    "深加法链应被求值深度守卫拦截"
                );
                // 允许的最长阶乘链（4096 字符），析构递归最深的情形。
                let bang_chain = format!("1{}", "!".repeat(MAX_EXPR_CHARS - 1));
                assert_eq!(bang_chain.chars().count(), MAX_EXPR_CHARS);
                assert!(
                    eval(&bang_chain, 60, &mut noop).is_err(),
                    "阶乘链应被求值深度守卫拦截"
                );
                let ok = eval("1+1+1", 40, &mut noop).expect("shallow add");
                assert_eq!(ok.full, "3");
            })
            .expect("spawn 2MiB worker thread");
        handle.join().expect("worker thread must not abort/panic");
    }

    #[test]
    fn expression_length_limit_is_enforced() {
        let long = "1".repeat(MAX_EXPR_CHARS + 1);
        let err = run_err(&long, 40);
        assert!(err.contains("过长"), "got {err}");
    }

    #[test]
    fn real_overflow_is_rejected() {
        for expr in [
            "2^(1e20/7)",
            "2^(1e20/7)+1",
            "2^(1e400/7)",
            "(1e400/7)^9000000000000000000",
            "1/(2^(1e20/7))",
        ] {
            let err = run_err(expr, 60);
            assert!(
                err.contains("超出可表示范围"),
                "{expr}: expected range error, got {err}"
            );
        }
        // 在表示范围内的大整数结果不应被误杀。
        let out = run("2^1000", 60);
        assert_eq!(out.kind, "int");
        assert_eq!(out.digits, 302);
    }

    #[test]
    fn log_domain_subtraction_signs() {
        let out = run("1e20!-1e21!", 60);
        assert!(
            out.full.starts_with('-'),
            "1e20!-1e21! 应为负, got {}",
            out.full
        );
        let out = run("1e20!-(-1e21!)", 60);
        assert!(
            !out.full.starts_with('-'),
            "1e20!-(-1e21!) 应为正, got {}",
            out.full
        );
        let out = run("-1e21!+1e20!", 60);
        assert!(
            out.full.starts_with('-'),
            "-1e21!+1e20! 应为负, got {}",
            out.full
        );
        // 加法路径与“无法分辨”的既有语义不回归。
        let out = run("1e20!+1e20!", 60);
        assert!(!out.full.starts_with('-'), "got {}", out.full);
        assert!(run_err("1e20!-1e20!", 60).contains("无法"));
    }

    #[test]
    fn sum_i64_boundaries_do_not_wrap() {
        // i64 全域：项数用 i128 计算，必须报“超过上限”，不能回绕成 2 项。
        let err = run_err("sum(k,k,-9223372036854775808,9223372036854775807)", 40);
        assert!(err.contains("超过上限"), "got {err}");
        // i64 边界单点/两点：循环变量由 f + step*i 计算，debug 下不得 panic。
        assert_eq!(
            run("sum(k,k,9223372036854775807,9223372036854775807)", 40).full,
            "9223372036854775807"
        );
        assert_eq!(
            run("sum(k,k,-9223372036854775808,-9223372036854775808)", 40).full,
            "-9223372036854775808"
        );
        assert_eq!(
            run("sum(k,k,-9223372036854775808,-9223372036854775807)", 40).full,
            "-18446744073709551615"
        );
        assert_eq!(
            run("sum(k,k,9223372036854775807,9223372036854775806)", 40).full,
            "18446744073709551613"
        );
    }

    #[test]
    fn zero_to_negative_power_is_error() {
        assert!(run_err("0^-1", 40).contains("0 不能取负次幂"));
        assert!(run_err("0^-2", 40).contains("0 不能取负次幂"));
        // 走到 Dec 基数的负整数指数分支同样要报错（ln(1) 为 0）。
        assert!(run_err("ln(1)^-1", 40).contains("0 不能取负次幂"));
        // 既有语义不回归。
        assert_eq!(run("2^0", 40).full, "1");
        assert_eq!(run("0^2", 40).full, "0");
        assert!(run_err("0^0", 40).contains("未定义"));
    }

    #[test]
    fn logarithm_rejects_negative_log_domain() {
        assert!(run_err("ln(-1e20!)", 60).contains("大于 0"));
        assert!(run_err("log(-1e20!)", 60).contains("大于 0"));
        assert!(run_err("log(-1e20!,10)", 60).contains("大于 0"));
        // 正的量级仍可正常取对数（ln(1e20!) ≈ 4.5e21）。
        let out = run("ln(1e20!)", 60);
        assert_eq!(out.kind, "approx");
        assert!(out.full.len() > 10, "got {}", out.full);
    }

    #[test]
    fn log_domain_values_can_be_summed() {
        let base = run("1e20!", 60);
        let out = run("sum(1e20!, k, 1, 1)", 60);
        assert_eq!(out.kind, "approx");
        assert_eq!(out.full, base.full, "单求和项应与直接求值一致");
        // 对数域多项累加：不能再误报“求和结果超过上限”。
        let out = run("sum(1e20!/k, k, 1, 3)", 60);
        assert!(out.headline.is_some(), "对数域求和应给出位数信息");
        assert_eq!(out.kind, "approx");
    }

    #[test]
    fn sum_loop_variable_does_not_leak() {
        assert!(run_err("sum(k,k,1,3)+k", 40).contains("未知变量"));
        assert!(run_err("sum(k,k,1,3)*k", 40).contains("未知变量"));
        // 嵌套求和与“求和体内再求和后继续使用外层 k”都必须正确。
        assert_eq!(run("sum(sum(k,k,1,2),k,1,2)", 40).full, "6");
        assert_eq!(run("sum(sum(k,k,1,2)*k,k,1,2)", 40).full, "9");
        // 其他变量作用域（导数的 x）不受求和影响。
        let v = parse_plain_f64(&run("diff(sum(k*x,k,1,2),x,3)", 40));
        assert!((v - 3.0).abs() < 1e-8, "got {v}");
    }

    #[test]
    fn literal_exponent_underflow_is_error() {
        // 指数减 frac_len 用 checked_sub：i64::MIN - 1 不得在 debug 下 panic。
        let err = run_err("1.5e-9223372036854775808", 40);
        assert!(err.contains("指数"), "got {err}");
        let out = run("1.5e-3", 40);
        assert!(out.full.starts_with("0.0015"), "got {}", out.full);
    }

    #[test]
    #[ignore = "构造两个约 6e7 位的操作数较慢/占内存，CI 默认跳过；手动验证乘法 1e8 位上限"]
    fn multiplication_limit_rejects_two_large_operands() {
        // 两个各约 6e7 位的精确整数，乘积约 1.2e8 位 > MAX_DIGITS：
        // 旧实现用 MAX_DIGITS * 2.0 会放行并真的去算，修复后应在乘法前报错。
        let err = run_err("(10^60000000)*(10^60000000)", 60);
        assert!(err.contains("超过上限"), "got {err}");
    }
}
