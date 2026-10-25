#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tao::{
    dpi::{LogicalPosition, LogicalSize},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::http::{Response, StatusCode};
use wry::{WebViewBuilder, WebViewId};

#[derive(Debug)]
enum UserEvent {
    Msg(String),
}

#[derive(Default)]
struct Store {
    main: Option<String>,
    extras: Vec<String>,
}

/// 表达式长度硬上限（按 Unicode 字符计）。前端 `maxlength` 与 `compute()` 使用同一数值，
/// 这里再独立校验一次，确保不依赖前端也能挡住会压垮递归下降解析器的超长输入。
const MAX_EXPR_CHARS: usize = 4096;

/// 自定义协议响应统一附加的 CSP：页面只加载同源脚本/样式，禁止外联、表单与框架嵌套。
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// 可信页面来源白名单。`bignum://localhost/...` 在 Windows 上会被 wry 改写为
/// `http://bignum.localhost/...`，因此两种前缀都要放行。
fn is_trusted_url(url: &str) -> bool {
    url.starts_with("http://bignum.localhost/") || url.starts_with("bignum://")
}

/// IPC 边界上的表达式长度强校验（按字符数，而不是字节数）。
fn expr_within_limit(expr: &str) -> bool {
    expr.chars().count() <= MAX_EXPR_CHARS
}

const HTML: &str = include_str!("../ui/index.html");
const CSS: &str = include_str!("../ui/style.css");
const JS: &str = include_str!("../ui/app.js");

/// 自定义协议（bignum:// → Windows 上的 http://bignum.localhost/）的唯一响应入口。
/// 未知路径返回 404；所有响应统一附加 CSP、nosniff 与 charset，作为注入面的纵深防御。
fn asset_response(path: &str) -> Response<Cow<'static, [u8]>> {
    let (mime, status, body): (&str, StatusCode, &'static [u8]) = match path {
        "/" | "/index.html" => ("text/html; charset=utf-8", StatusCode::OK, HTML.as_bytes()),
        "/style.css" => ("text/css; charset=utf-8", StatusCode::OK, CSS.as_bytes()),
        "/app.js" => (
            "text/javascript; charset=utf-8",
            StatusCode::OK,
            JS.as_bytes(),
        ),
        _ => (
            "text/plain; charset=utf-8",
            StatusCode::NOT_FOUND,
            b"not found",
        ),
    };
    Response::builder()
        .status(status)
        .header("Content-Type", mime)
        .header("Cache-Control", "no-store")
        .header("Content-Security-Policy", CONTENT_SECURITY_POLICY)
        .header("X-Content-Type-Options", "nosniff")
        .body(Cow::Borrowed(body))
        .unwrap()
}

fn main() {
    if wry::webview_version().is_err() {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("缺少 WebView2 运行时")
            .set_description(
                "未检测到 Microsoft Edge WebView2 Runtime。\n\n\
                 请双击本文件夹中的 MicrosoftEdgeWebview2Setup.exe 完成安装后重新打开本程序。\n\n\
                 如文件缺失，也可从以下地址安装：\n\
                 https://go.microsoft.com/fwlink/p/?LinkId=2124703",
            )
            .show();
        return;
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title("BIGNUM · 任意精度大数计算器")
        .with_inner_size(LogicalSize::new(1280.0, 840.0))
        .with_min_inner_size(LogicalSize::new(900.0, 620.0))
        .build(&event_loop)
        .expect("failed to build window");

    if let Some(monitor) = window.primary_monitor() {
        let scale = monitor.scale_factor();
        let size = monitor.size();
        let w = (size.width as f64 / scale - 64.0).clamp(900.0, 1280.0);
        let h = (size.height as f64 / scale - 96.0).clamp(620.0, 840.0);
        window.set_inner_size(LogicalSize::new(w, h));
        window.set_outer_position(LogicalPosition::new(24.0, 24.0));
    }

    let proxy = event_loop.create_proxy();
    let store = Arc::new(Mutex::new(Store::default()));

    let ipc_store = store.clone();
    let ipc_proxy = proxy.clone();

    let smoke = std::env::var("BIGNUM_SMOKE").ok().filter(|v| !v.is_empty());

    let data_dir = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("BIGNUM")
        .join("WebView2-cache2");
    let _ = std::fs::create_dir_all(&data_dir);
    let mut web_context = wry::WebContext::new(Some(data_dir));

    let smoke_script = smoke
        .as_ref()
        .map(|v| {
            let q = serde_json::to_string(v).unwrap_or_else(|_| "\"1\"".to_string());
            format!("window.__bignum_smoke = {q};")
        })
        .unwrap_or_default();

    let webview = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_initialization_script(smoke_script)
        .with_background_color((11, 12, 14, 255))
        .with_custom_protocol("bignum".into(), |_id: WebViewId, req| {
            asset_response(req.uri().path())
        })
        // SEC-01：只允许同源（bignum）页面导航，外部 URL 一律取消。
        .with_navigation_handler(|url: String| is_trusted_url(&url))
        // SEC-01：配置（no-op）drop handler 会让 wry 调用 SetAllowExternalDrop(false)，
        // 关闭 WebView2 默认允许的外部文件拖放；返回值在 Windows 上被 wry 忽略。
        .with_drag_drop_handler(|_| true)
        .with_ipc_handler(move |req| {
            // SEC-01：只接受来自可信页面的 IPC；被拖入/外部加载的文档直接丢弃。
            if !is_trusted_url(&req.uri().to_string()) {
                return;
            }
            let body = req.body().clone();
            let Ok(v) = serde_json::from_str::<Value>(&body) else {
                return;
            };
            let cmd = v["cmd"].as_str().unwrap_or("").to_string();
            match cmd.as_str() {
                "eval" => {
                    let expr = v["expr"].as_str().unwrap_or("").to_string();
                    // SEC-02：后端强约束表达式长度，前端校验只是第一道防线。
                    if !expr_within_limit(&expr) {
                        let payload = json!({
                            "type": "error",
                            "message": format!(
                                "表达式过长：最多 {MAX_EXPR_CHARS} 个字符（当前 {} 个）。",
                                expr.chars().count()
                            ),
                        });
                        let _ = ipc_proxy.send_event(UserEvent::Msg(payload.to_string()));
                        return;
                    }
                    let precision = v["precision"].as_u64().unwrap_or(engine::DEFAULT_PRECISION as u64) as usize;
                    let st = ipc_store.clone();
                    let p = ipc_proxy.clone();
                    std::thread::spawn(move || {
                        let p_stage = p.clone();
                        let mut prog = move |stage: engine::Stage| {
                            let val = match stage {
                                engine::Stage::Label(l) => json!({"type": "stage", "label": l}),
                                engine::Stage::Ratio(l, done, total) => {
                                    json!({"type": "stage", "label": l, "done": done, "total": total})
                                }
                            };
                            let _ = p_stage.send_event(UserEvent::Msg(val.to_string()));
                        };
                        let started = std::time::Instant::now();
                        // CON-06：worker 内普通 panic 也必须回一条 error，否则前端会永久 busy。
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            engine::eval(&expr, precision, &mut prog)
                        }));
                        match result {
                            Ok(Ok(out)) => {
                                let extras_meta: Vec<Value> = out
                                    .extras
                                    .iter()
                                    .map(|e| {
                                        json!({
                                            "label": e.label,
                                            "head": e.head,
                                            "tail": e.tail,
                                            "omitted": e.omitted,
                                            "full_len": e.full.len(),
                                        })
                                    })
                                    .collect();
                                let payload = json!({
                                    "type": "done",
                                    "kind": out.kind,
                                    "exact": out.exact,
                                    "digits": out.digits,
                                    "omitted": out.omitted,
                                    "head": out.head,
                                    "tail": out.tail,
                                    "full_len": out.full.len(),
                                    "scientific": out.scientific,
                                    "note": out.note,
                                    "ms": out.ms,
                                    "wall_ms": started.elapsed().as_millis() as u64,
                                    "extras": extras_meta,
                                    "headline": out.headline,
                                    "unit": out.unit,
                                });
                                {
                                    // CON-06：Mutex 中毒时继续使用内部数据，避免 copy/save 永久失败。
                                    let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
                                    s.main = Some(out.full);
                                    s.extras = out.extras.into_iter().map(|e| e.full).collect();
                                }
                                let _ = p.send_event(UserEvent::Msg(payload.to_string()));
                            }
                            Ok(Err(message)) => {
                                let payload = json!({
                                    "type": "error",
                                    "message": message,
                                    "wall_ms": started.elapsed().as_millis() as u64,
                                });
                                let _ = p.send_event(UserEvent::Msg(payload.to_string()));
                            }
                            Err(_panic) => {
                                let payload = json!({
                                    "type": "error",
                                    "message": "内部错误：计算过程发生异常，请重试或反馈该表达式。",
                                    "wall_ms": started.elapsed().as_millis() as u64,
                                });
                                let _ = p.send_event(UserEvent::Msg(payload.to_string()));
                            }
                        }
                    });
                }
                "copy" => {
                    let field = v["field"].as_str().unwrap_or("main").to_string();
                    let index = v["index"].as_u64().unwrap_or(0) as usize;
                    let st = ipc_store.clone();
                    let p = ipc_proxy.clone();
                    std::thread::spawn(move || {
                        let text = {
                            let s = st.lock().unwrap_or_else(|e| e.into_inner());
                            if field == "extra" {
                                s.extras.get(index).cloned()
                            } else {
                                s.main.clone()
                            }
                        };
                        let payload = match text {
                            Some(t) => {
                                let chars = t.len();
                                match arboard::Clipboard::new()
                                    .and_then(|mut cb| cb.set_text(t))
                                {
                                    Ok(()) => json!({
                                        "type": "copied",
                                        "field": field,
                                        "index": index,
                                        "chars": chars,
                                    }),
                                    Err(e) => json!({
                                        "type": "error",
                                        "message": format!("写入剪贴板失败：{e}"),
                                    }),
                                }
                            }
                            None => json!({
                                "type": "error",
                                "message": "没有可复制的结果。",
                            }),
                        };
                        let _ = p.send_event(UserEvent::Msg(payload.to_string()));
                    });
                }
                "save" => {
                    let field = v["field"].as_str().unwrap_or("main").to_string();
                    let index = v["index"].as_u64().unwrap_or(0) as usize;
                    let st = ipc_store.clone();
                    let p = ipc_proxy.clone();
                    std::thread::spawn(move || {
                        let text = {
                            let s = st.lock().unwrap_or_else(|e| e.into_inner());
                            if field == "extra" {
                                s.extras.get(index).cloned()
                            } else {
                                s.main.clone()
                            }
                        };
                        let payload = match text {
                            Some(t) => {
                                let default_name = format!("bignum-{}-chars.txt", t.len());
                                match rfd::FileDialog::new()
                                    .set_title("导出计算结果")
                                    .set_file_name(&default_name)
                                    .add_filter("文本文件", &["txt"])
                                    .save_file()
                                {
                                    Some(path) => match std::fs::write(&path, t.as_bytes()) {
                                        Ok(()) => json!({
                                            "type": "saved",
                                            "field": field,
                                            "index": index,
                                            "path": path.display().to_string(),
                                            "chars": t.len(),
                                        }),
                                        Err(e) => json!({
                                            "type": "error",
                                            "message": format!("写入文件失败：{e}"),
                                        }),
                                    },
                                    None => json!({"type": "save_cancelled"}),
                                }
                            }
                            None => json!({
                                "type": "error",
                                "message": "没有可导出的结果。",
                            }),
                        };
                        let _ = p.send_event(UserEvent::Msg(payload.to_string()));
                    });
                }
                _ => {}
            }
        })
        .with_url("bignum://localhost/index.html")
        .build(&window)
        .expect("failed to build webview");

    let _window = window;
    let _web_context = web_context;

    event_loop.run(move |event, _elwt, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            Event::UserEvent(UserEvent::Msg(msg)) => {
                let script = format!("window.__bignum_recv && window.__bignum_recv({});", msg);
                let _ = webview.evaluate_script(&script);
            }
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        CONTENT_SECURITY_POLICY, MAX_EXPR_CHARS, StatusCode, asset_response, expr_within_limit,
        is_trusted_url,
    };

    #[test]
    fn expression_limit_accepts_at_limit_and_rejects_over() {
        assert!(expr_within_limit(""));
        assert!(expr_within_limit(&"1".repeat(MAX_EXPR_CHARS)));
        assert!(!expr_within_limit(&"1".repeat(MAX_EXPR_CHARS + 1)));
    }

    #[test]
    fn expression_limit_counts_chars_not_bytes() {
        // 'π' 是 2 字节 UTF-8 的单个字符；上限按字符数计，不能被字节数误伤。
        let at_limit = "π".repeat(MAX_EXPR_CHARS);
        assert!(at_limit.len() > MAX_EXPR_CHARS);
        assert!(expr_within_limit(&at_limit));

        // '𝄞' 是 4 字节 UTF-8 的单个字符（代理对），同样按 1 个字符计数。
        let over = "𝄞".repeat(MAX_EXPR_CHARS + 1);
        assert!(!expr_within_limit(&over));
    }

    #[test]
    fn trusted_url_whitelist_covers_bignum_origins() {
        assert!(is_trusted_url("http://bignum.localhost/index.html"));
        assert!(is_trusted_url("http://bignum.localhost/app.js"));
        assert!(is_trusted_url("bignum://localhost/index.html"));
    }

    #[test]
    fn trusted_url_rejects_external_documents() {
        assert!(!is_trusted_url(""));
        assert!(!is_trusted_url("file:///C:/tmp/invoice.html"));
        assert!(!is_trusted_url("https://example.com/"));
        assert!(!is_trusted_url(
            "http://bignum.localhost.evil.example/index.html"
        ));
        assert!(!is_trusted_url(
            "http://bignum.localhost@evil.example/index.html"
        ));
        assert!(!is_trusted_url("data:text/html,<h1>x</h1>"));
    }

    #[test]
    fn asset_response_sets_security_headers() {
        let response = asset_response("/index.html");
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(
            headers
                .get("Content-Security-Policy")
                .unwrap()
                .to_str()
                .unwrap(),
            CONTENT_SECURITY_POLICY
        );
        assert_eq!(
            headers
                .get("X-Content-Type-Options")
                .unwrap()
                .to_str()
                .unwrap(),
            "nosniff"
        );
        for path in ["/", "/index.html", "/style.css", "/app.js"] {
            let content_type = asset_response(path)
                .headers()
                .get("Content-Type")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            assert!(
                content_type.contains("charset=utf-8"),
                "{path} 的 Content-Type 缺少 charset：{content_type}"
            );
        }
    }

    #[test]
    fn unknown_asset_returns_404() {
        for path in ["/secret.txt", "/../Cargo.toml", "/app.js/extra"] {
            let response = asset_response(path);
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "path: {path}");
        }
    }
}
