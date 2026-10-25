(function () {
  "use strict";

  var $ = function (id) { return document.getElementById(id); };

  var els = {
    clock: $("clock"),
    expr: $("expr"),
    exprLen: $("expr-len"),
    keypad: $("keypad"),
    keyEq: $("key-eq"),
    examples: $("examples"),
    precision: $("precision"),
    status: $("status"),
    idle: $("idle"),
    run: $("run"),
    runLabel: $("run-label"),
    runBar: $("run-bar"),
    elapsedNum: $("elapsed-num"),
    error: $("error"),
    errorMsg: $("error-msg"),
    result: $("result"),
    rDigits: $("r-digits"),
    rUnit: $("r-unit"),
    rSci: $("r-sci"),
    rNote: $("r-note"),
    rNumber: $("r-number"),
    rMore: $("r-more"),
    rOmitted: $("r-omitted"),
    rTail: $("r-tail"),
    rExtras: $("r-extras"),
    rMeta: $("r-meta"),
    copyMain: $("copy-main"),
    saveMain: $("save-main"),
    receipt: $("receipt"),
    loglist: $("loglist"),
    logEmpty: $("log-empty")
  };

  var MAX_EXPR_CHARS = 4096;       // 与 index.html 的 maxlength、Rust 侧上限一致
  var MAX_HISTORY = 12;
  var MAX_EXPR_DISPLAY = 28;       // 历史列表行内展示的截断长度
  var MAX_EXPR_TITLE = 300;        // 历史条目 title 提示的截断长度
  var WATCHDOG_MS = 300000;        // 看门狗：300 秒无任何 stage/done 事件视为无响应
  var reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  var smokeKind = window.__bignum_smoke || null;
  if (!smokeKind && location.search.indexOf("smoke=") !== -1) {
    smokeKind = location.search.split("smoke=")[1].split("&")[0] || "1";
  }
  var smokeMode = !!smokeKind;
  if (smokeMode) { document.title = "SMOKE:" + smokeKind; }

  var state = {
    busy: false,
    timer: 0,
    t0: 0,
    history: [],
    current: null,  // 发起计算时 { expr, precision } 的快照，供历史与结果元信息使用
    watchdog: 0
  };

  function send(obj) {
    if (window.ipc && window.ipc.postMessage) {
      window.ipc.postMessage(JSON.stringify(obj));
    }
  }

  function fmtInt(n) {
    return Math.round(n).toLocaleString("en-US");
  }

  function nowStamp() {
    var d = new Date();
    var p = function (x) { return String(x).padStart(2, "0"); };
    return p(d.getHours()) + ":" + p(d.getMinutes()) + ":" + p(d.getSeconds());
  }

  function tickClock() {
    els.clock.textContent = nowStamp();
  }

  function updateLen() {
    var n = els.expr.value.length;
    els.exprLen.textContent = fmtInt(n) + " 字符";
  }

  function insertAtCaret(text) {
    var el = els.expr;
    el.focus();
    var start = el.selectionStart == null ? el.value.length : el.selectionStart;
    var end = el.selectionEnd == null ? start : el.selectionEnd;
    el.setRangeText(text, start, end, "end");
    updateLen();
  }

  function deleteBack() {
    var el = els.expr;
    el.focus();
    var start = el.selectionStart == null ? el.value.length : el.selectionStart;
    var end = el.selectionEnd == null ? start : el.selectionEnd;
    if (start !== end) {
      el.setRangeText("", start, end, "end");
    } else if (start > 0) {
      el.setRangeText("", start - 1, start, "end");
    }
    updateLen();
  }

  function clearExpr() {
    els.expr.value = "";
    els.expr.focus();
    updateLen();
  }

  function setBusy(busy) {
    state.busy = busy;
    els.keyEq.dataset.loading = busy ? "true" : "false";
    els.keyEq.disabled = busy;
    els.keyEq.setAttribute("aria-busy", busy ? "true" : "false");
    els.keyEq.setAttribute("aria-label", busy ? "计算中" : "计算");
    if (!busy) { clearWatchdog(); }
  }

  function startRun() {
    state.t0 = performance.now();
    cancelAnimationFrame(state.timer);
    var tick = function () {
      els.elapsedNum.textContent = ((performance.now() - state.t0) / 1000).toFixed(2);
      state.timer = requestAnimationFrame(tick);
    };
    tick();
  }

  function stopRun() {
    cancelAnimationFrame(state.timer);
  }

  // CON-06 / CON-08：前端看门狗。只在 busy 且长时间没有任何 stage/done 事件时复位，
  // 持续上报进度的正常长算例（求和/积分）不会被误中断。
  function clearWatchdog() {
    if (state.watchdog) {
      clearTimeout(state.watchdog);
      state.watchdog = 0;
    }
  }

  function noteProgress() {
    clearWatchdog();
    if (!state.busy) { return; }
    state.watchdog = setTimeout(onWatchdogTimeout, WATCHDOG_MS);
  }

  function onWatchdogTimeout() {
    state.watchdog = 0;
    if (!state.busy) { return; }
    setBusy(false);
    stopRun();
    els.status.textContent = "计算超时 · 无响应";
    showView("error");
    els.errorMsg.textContent = "计算超时/无响应：超过 300 秒没有收到任何进度，已恢复操作。计算可能仍在后台运行，结果返回时会显示。";
  }

  function setStage(label, done, total) {
    els.runLabel.textContent = label;
    if (typeof done === "number" && typeof total === "number" && total > 0) {
      var bar = els.runBar;
      bar.classList.add("is-determinate");
      bar.classList.remove("is-indeterminate");
      void bar.offsetWidth;
      bar.style.width = Math.max(2, Math.min(100, (done / total) * 100)) + "%";
    } else {
      var b2 = els.runBar;
      b2.classList.remove("is-determinate");
      b2.style.width = "";
      b2.style.opacity = "";
      if (!reduceMotion) {
        b2.classList.add("is-indeterminate");
      } else {
        b2.style.width = "100%";
        b2.style.opacity = "0.3";
      }
    }
  }

  function resetResultChrome() {
    els.copyMain.dataset.done = "false";
    els.copyMain.style.minWidth = "";
    els.copyMain.querySelector(".btn__label").textContent = "复制全部";
    els.receipt.hidden = true;
    els.receipt.textContent = "";
  }

  function showView(name) {
    els.idle.hidden = name !== "idle";
    els.run.hidden = name !== "run";
    els.error.hidden = name !== "error";
    els.result.hidden = name !== "result";
  }

  function compute() {
    if (state.busy) { return; }
    var expr = els.expr.value;
    if (!expr.trim()) {
      els.expr.focus();
      renderError("表达式为空。");
      return;
    }
    if (expr.length > MAX_EXPR_CHARS) {
      els.expr.focus();
      renderError("表达式过长：最多 " + fmtInt(MAX_EXPR_CHARS) + " 个字符（当前 " + fmtInt(expr.length) + " 个）。");
      return;
    }
    // CON-03：发送时快照 {expr, precision}，完成时不再读取可能已被修改的输入框。
    var precision = parseInt(els.precision.value, 10) || 60;
    state.current = { expr: expr, precision: precision };
    resetResultChrome();
    els.rExtras.textContent = "";
    showView("run");
    setStage("解析表达式");
    setBusy(true);
    els.status.textContent = "计算中";
    startRun();
    noteProgress();
    send({ cmd: "eval", expr: expr, precision: precision });
  }

  function springCount(el, target) {
    cancelAnimationFrame(el._raf);
    if (reduceMotion) {
      el.textContent = fmtInt(target);
      el._x = target;
      el._v = 0;
      return;
    }
    var k = 68;
    var ratio = 0.88;
    var c = 2 * ratio * Math.sqrt(k);
    var x = typeof el._x === "number" ? el._x : 0;
    var v = typeof el._v === "number" ? el._v : 0;
    var last = performance.now();
    var step = function (now) {
      var dt = Math.min((now - last) / 1000, 1 / 30);
      last = now;
      var a = k * (target - x) - c * v;
      v += a * dt;
      x += v * dt;
      if (Math.abs(target - x) < 0.5 && Math.abs(v) < 0.5) {
        el.textContent = fmtInt(target);
        el._x = target;
        el._v = 0;
        return;
      }
      el.textContent = fmtInt(x);
      el._x = x;
      el._v = v;
      el._raf = requestAnimationFrame(step);
    };
    el._raf = requestAnimationFrame(step);
  }

  function renderSci(text) {
    els.rSci.textContent = "";
    var idx = text.indexOf("×10^");
    if (idx === -1) {
      els.rSci.textContent = text;
      return;
    }
    els.rSci.appendChild(document.createTextNode(text.slice(0, idx + 3)));
    var sup = document.createElement("sup");
    sup.textContent = text.slice(idx + 4);
    els.rSci.appendChild(sup);
  }

  function buildExtra(extra, index) {
    var box = document.createElement("div");
    box.className = "extra";

    var head = document.createElement("div");
    head.className = "extra__head";

    var label = document.createElement("span");
    label.className = "extra__label";
    label.textContent = extra.label;

    var acts = document.createElement("div");
    acts.className = "extra__acts";

    var copyBtn = document.createElement("button");
    copyBtn.type = "button";
    copyBtn.className = "extra__btn";
    copyBtn.dataset.index = String(index);
    copyBtn.dataset.role = "copy-extra";
    copyBtn.textContent = "复制";

    var saveBtn = document.createElement("button");
    saveBtn.type = "button";
    saveBtn.className = "extra__btn";
    saveBtn.dataset.index = String(index);
    saveBtn.dataset.role = "save-extra";
    saveBtn.textContent = "导出";

    acts.appendChild(copyBtn);
    acts.appendChild(saveBtn);
    head.appendChild(label);
    head.appendChild(acts);

    var value = document.createElement("div");
    value.className = "extra__value";
    value.textContent = extra.head;
    if (extra.omitted > 0) {
      var more = document.createElement("div");
      more.className = "extra__more";
      more.textContent = "⋯⋯ 中间省略 " + fmtInt(extra.omitted) + " 位 ⋯⋯";
      value.appendChild(more);
      var tail = document.createElement("div");
      tail.className = "extra__tail";
      tail.textContent = extra.tail;
      value.appendChild(tail);
    }

    box.appendChild(head);
    box.appendChild(value);
    return box;
  }

  function renderDone(d) {
    setBusy(false);
    stopRun();
    els.status.textContent = d.exact ? "完成 · 精确结果" : "完成 · 近似值";

    if (d.headline) {
      cancelAnimationFrame(els.rDigits._raf);
      els.rDigits.textContent = d.headline;
      els.rDigits.classList.toggle("is-wide", d.headline.length > 12);
    } else {
      els.rDigits.classList.remove("is-wide");
      springCount(els.rDigits, d.digits);
    }
    els.rUnit.textContent = d.unit || (d.kind === "int" ? "位十进制整数" : "位有效数字 · 近似");

    els.rNumber.textContent = d.head;
    if (d.tail) {
      els.rTail.textContent = d.tail;
      els.rTail.hidden = false;
      if (d.omitted > 0) {
        els.rOmitted.textContent = fmtInt(d.omitted);
        els.rMore.hidden = false;
      } else {
        els.rMore.hidden = true;
      }
    } else {
      els.rTail.hidden = true;
      els.rMore.hidden = true;
    }

    if (d.scientific) {
      renderSci(d.scientific);
      els.rSci.hidden = false;
    } else {
      els.rSci.hidden = true;
    }

    if (d.note) {
      els.rNote.textContent = d.note;
      els.rNote.hidden = false;
    } else {
      els.rNote.hidden = true;
    }

    els.rExtras.textContent = "";
    if (d.extras && d.extras.length) {
      for (var i = 0; i < d.extras.length; i++) {
        els.rExtras.appendChild(buildExtra(d.extras[i], i));
      }
    }

    // CON-03：结果元信息使用发送时的快照，而不是完成时的输入框内容。
    var snap = state.current || {
      expr: els.expr.value,
      precision: parseInt(els.precision.value, 10) || 60
    };
    var mode = d.exact ? "精确" : "近似";
    var meta = mode + " · 表达式 " + truncateText(snap.expr, MAX_EXPR_DISPLAY) +
      " · 有效数字 " + snap.precision +
      " · 内核 " + d.ms + " ms · 全程 " + d.wall_ms + " ms · " + nowStamp();

    showView("result");
    // 放在 showView 之后写入，保证结果区（aria-live=polite）可见时确有内容更新可播报。
    els.rMeta.textContent = meta;
    if (!reduceMotion) {
      els.result.classList.remove("is-in");
      void els.result.offsetWidth;
      els.result.classList.add("is-in");
    }

    addHistory(d);
    if (smokeMode) {
      setTimeout(function () {
        send({ cmd: "copy", field: "main" });
      }, 700);
    }
  }

  function renderError(message, wallMs) {
    setBusy(false);
    stopRun();
    els.status.textContent = "中止 · 无法计算";
    // 先让 role="alert" 的错误区可见，再写入原因，确保屏幕阅读器能播报本次错误。
    showView("error");
    els.errorMsg.textContent = message + (typeof wallMs === "number" ? "（耗时 " + wallMs + " ms）" : "");
    if (!reduceMotion) {
      els.error.classList.remove("is-in");
      void els.error.offsetWidth;
      els.error.classList.add("is-in");
    }
  }

  function truncateText(text, limit) {
    var value = typeof text === "string" ? text : String(text == null ? "" : text);
    return value.length > limit ? value.slice(0, limit) + "…" : value;
  }

  // 旧版本会把截断标记 "…" 写进 localStorage/历史数据；读取时剥离（引擎本身不接受该字符）。
  function stripTruncationMark(text) {
    return typeof text === "string" ? text.replace(/…+$/, "") : "";
  }

  function addHistory(d) {
    // UX-01：数据层保存完整表达式（长度已在 compute/restore 约束到 MAX_EXPR_CHARS），
    // 只在展示层（renderHistory）截断，避免把 "…" 当表达式持久化/回填。
    var snap = state.current || {
      expr: els.expr.value,
      precision: parseInt(els.precision.value, 10) || 60
    };
    var entry = {
      expr: snap.expr,
      precision: snap.precision,
      digits: d.digits,
      kind: d.kind,
      exact: d.exact,
      ms: d.wall_ms
    };
    state.history.unshift(entry);
    if (state.history.length > MAX_HISTORY) {
      state.history.length = MAX_HISTORY;
    }
    renderHistory();
    persist();
  }

  function renderHistory() {
    els.loglist.textContent = "";
    els.logEmpty.hidden = state.history.length > 0;
    for (var i = 0; i < state.history.length; i++) {
      var h = state.history[i];
      var li = document.createElement("li");
      var btn = document.createElement("button");
      btn.type = "button";
      btn.className = "logitem";
      btn.style.setProperty("--i", String(i));
      btn.dataset.index = String(i);

      var rawExpr = typeof h.expr === "string" ? h.expr : "";
      var expr = document.createElement("span");
      expr.className = "logitem__op";
      // 展示层截断：行内 28 字符，悬停 title 最多 300 字符；数据层始终是完整表达式。
      expr.textContent = truncateText(rawExpr, MAX_EXPR_DISPLAY);
      btn.title = truncateText(rawExpr, MAX_EXPR_TITLE);

      var meta = document.createElement("span");
      meta.className = "logitem__meta";
      meta.textContent = fmtInt(h.digits) + (h.kind === "int" ? " 位" : " 位精度") + " · " + (h.exact ? "精确" : "近似") + " · " + h.ms + " ms";

      var meta2 = document.createElement("span");
      meta2.className = "logitem__meta";
      meta2.textContent = "有效数字 " + h.precision;

      btn.appendChild(expr);
      btn.appendChild(meta);
      btn.appendChild(meta2);
      li.appendChild(btn);
      els.loglist.appendChild(li);
    }
  }

  function restoreHistory(index) {
    var h = state.history[index];
    if (!h || typeof h.expr !== "string") { return; }
    // SEC-02：历史条目同样受长度上限约束；回填前剥离旧数据里的截断标记（UX-01）。
    var expr = stripTruncationMark(h.expr);
    els.expr.value = expr.length > MAX_EXPR_CHARS ? expr.slice(0, MAX_EXPR_CHARS) : expr;
    var precision = parseInt(h.precision, 10);
    els.precision.value = precision >= 1 && precision <= 5000 ? precision : 60;
    updateLen();
    els.expr.focus();
  }

  function persist() {
    try {
      localStorage.setItem(
        "bignum-state",
        JSON.stringify({
          version: 1,
          // UX-01：持久化完整表达式，不再写入 "…" 截断标记。
          expr: stripTruncationMark(els.expr.value).slice(0, MAX_EXPR_CHARS),
          precision: parseInt(els.precision.value, 10) || 60,
          history: state.history
        })
      );
    } catch (e) { /* storage unavailable */ }
  }

  // SEC-04：localStorage 属于不可信本地数据，逐条校验并丢弃非法历史条目。
  function sanitizeHistory(list) {
    if (!Array.isArray(list)) { return []; }
    var out = [];
    for (var i = 0; i < list.length && out.length < MAX_HISTORY; i++) {
      var item = list[i];
      if (!item || typeof item !== "object") { continue; }
      var expr = typeof item.expr === "string" ? stripTruncationMark(item.expr) : "";
      if (!expr) { continue; }
      if (expr.length > MAX_EXPR_CHARS) { expr = expr.slice(0, MAX_EXPR_CHARS); }
      var precision = parseInt(item.precision, 10);
      if (!isFinite(precision) || precision < 1 || precision > 5000) { precision = 60; }
      var digits = Number(item.digits);
      if (!isFinite(digits) || digits < 0) { digits = 0; }
      var ms = Number(item.ms);
      if (!isFinite(ms) || ms < 0) { ms = 0; }
      out.push({
        expr: expr,
        precision: precision,
        digits: digits,
        kind: item.kind === "int" ? "int" : "approx",
        exact: item.exact === true,
        ms: ms
      });
    }
    return out;
  }

  function restore() {
    var raw = null;
    try { raw = localStorage.getItem("bignum-state"); } catch (e) { raw = null; }
    if (!raw) { return; }
    try {
      var s = JSON.parse(raw);
      if (!s || typeof s !== "object") { return; }
      if (typeof s.expr === "string") {
        // SEC-02：恢复的表达式截断到上限；UX-01：剥离旧数据里的 "…" 截断标记。
        var expr = stripTruncationMark(s.expr);
        els.expr.value = expr.length > MAX_EXPR_CHARS ? expr.slice(0, MAX_EXPR_CHARS) : expr;
      }
      var pv = parseInt(s.precision, 10);
      if (pv >= 1 && pv <= 5000) { els.precision.value = pv; }
      state.history = sanitizeHistory(s.history);
    } catch (e) { /* ignore */ }
  }

  function lockWidth(btn) {
    if (!btn.style.minWidth) {
      btn.style.minWidth = btn.offsetWidth + "px";
    }
  }

  function markCopied(scope, index, chars) {
    if (scope === "extra") {
      var btn = els.rExtras.querySelector('.extra__btn[data-role="copy-extra"][data-index="' + index + '"]');
      if (btn) {
        lockWidth(btn);
        btn.textContent = "已复制";
        btn.dataset.done = "true";
      }
    } else {
      lockWidth(els.copyMain);
      els.copyMain.dataset.done = "true";
      els.copyMain.querySelector(".btn__label").textContent = "已复制";
    }
    els.receipt.hidden = false;
    els.receipt.textContent = "已复制 " + fmtInt(chars) + " 个字符到剪贴板 · " + nowStamp();
  }

  function markSaved(scope, index, path, chars) {
    if (scope === "extra") {
      var btn = els.rExtras.querySelector('.extra__btn[data-role="save-extra"][data-index="' + index + '"]');
      if (btn) {
        lockWidth(btn);
        btn.textContent = "已导出";
        btn.dataset.done = "true";
      }
    }
    els.receipt.hidden = false;
    els.receipt.textContent = "已导出 " + fmtInt(chars) + " 个字符 → " + path + " · " + nowStamp();
  }

  window.__bignum_recv = function (msg) {
    if (!msg || typeof msg !== "object") { return; }
    if (msg.type === "stage") {
      noteProgress();
      els.status.textContent = "计算中 · " + msg.label;
      setStage(msg.label, msg.done, msg.total);
    } else if (msg.type === "done") {
      renderDone(msg);
    } else if (msg.type === "error") {
      renderError(msg.message, msg.wall_ms);
    } else if (msg.type === "copied") {
      // CON-02：旧回执不落到新一轮计算上；busy 期间到达的复制回执直接丢弃。
      if (state.busy) { return; }
      markCopied(msg.field, msg.index, msg.chars);
    } else if (msg.type === "saved") {
      if (state.busy) { return; }
      markSaved(msg.field, msg.index, msg.path, msg.chars);
    } else if (msg.type === "save_cancelled") {
      els.status.textContent = "已取消导出";
    }
  };

  els.keypad.addEventListener("click", function (e) {
    var key = e.target.closest(".key");
    if (!key) { return; }
    if (key.dataset.act === "clear") {
      clearExpr();
    } else if (key.dataset.act === "back") {
      deleteBack();
    } else if (key.dataset.act === "equals") {
      compute();
    } else if (key.dataset.ins) {
      insertAtCaret(key.dataset.ins);
    }
  });

  els.examples.addEventListener("click", function (e) {
    var chip = e.target.closest(".chip");
    if (!chip) { return; }
    els.expr.value = chip.dataset.expr;
    updateLen();
    els.expr.focus();
    persist();
  });

  els.copyMain.addEventListener("click", function () {
    if (state.busy) { return; }
    send({ cmd: "copy", field: "main" });
  });

  els.saveMain.addEventListener("click", function () {
    if (state.busy) { return; }
    send({ cmd: "save", field: "main" });
  });

  els.rExtras.addEventListener("click", function (e) {
    var btn = e.target.closest(".extra__btn");
    if (!btn) { return; }
    if (state.busy) { return; }
    var index = parseInt(btn.dataset.index, 10) || 0;
    if (btn.dataset.role === "copy-extra") {
      send({ cmd: "copy", field: "extra", index: index });
    } else {
      send({ cmd: "save", field: "extra", index: index });
    }
  });

  els.loglist.addEventListener("click", function (e) {
    var item = e.target.closest(".logitem");
    if (!item) { return; }
    restoreHistory(parseInt(item.dataset.index, 10) || 0);
  });

  els.expr.addEventListener("input", function () {
    updateLen();
    persist();
  });

  els.expr.addEventListener("keydown", function (e) {
    if (e.key === "Enter") {
      e.preventDefault();
      compute();
    }
  });

  els.precision.addEventListener("change", persist);

  window.addEventListener("resize", function () {});

  // SEC-01 双保险：页面侧拒绝文件拖放（Rust 侧已通过 drop handler 关闭 external drop）。
  window.addEventListener("dragover", function (e) { e.preventDefault(); });
  window.addEventListener("drop", function (e) { e.preventDefault(); });

  document.addEventListener("keydown", function (e) {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      compute();
    }
  });

  tickClock();
  setInterval(tickClock, 1000);
  restore();
  updateLen();
  showView("idle");
  renderHistory();

  if (smokeMode) {
    var presets = {
      "1": "10^3000000",
      div: "1e3000000/7",
      log: "log(2)",
      fact: "10000!",
      huge: "6.02e23!",
      hugediv: "6.02e23! / 6.02e22!",
      sum: "sum(k^2, k, 1, 10000)",
      diff: "diff(x^3, x, 2)",
      integ: "integ(e^(-x^2), x, -inf, inf)",
      ln: "ln(2)"
    };
    els.expr.value = presets[smokeKind] || presets["1"];
    if (smokeKind === "div" || smokeKind === "log" || smokeKind === "ln") {
      els.precision.value = "80";
    }
    updateLen();
    setTimeout(compute, 400);
  }
})();
