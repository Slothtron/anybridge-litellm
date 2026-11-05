//! 构建期校验：`apps/ui/dist` 必须存在且不是占位页。
//!
//! 这里**只校验、不编排**——不在 `build.rs` 里跑 npm。真正的构建顺序由
//! `scripts/build.py` 保证：先 UI 再 native。

use std::path::{Path, PathBuf};

const PLACEHOLDER_MARKER: &str = "anybridge-console-placeholder";

fn dist_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/ui/dist")
}

fn main() {
    let dist = dist_dir();
    let index = dist.join("index.html");

    println!("cargo:rerun-if-changed={}", dist.display());

    let release = std::env::var("PROFILE").as_deref() == Ok("release");

    if !index.exists() {
        if release {
            panic!(
                "发布构建缺少控制台产物：{} 不存在。\n\
                 先跑 `mise run build`（它会先构建 apps/ui 再构建 native）。",
                index.display()
            );
        }
        // 开发态（cargo check/build/test）允许占位，避免必须先装 Node 才能跑 Rust 测试。
        std::fs::create_dir_all(&dist).expect("创建 apps/ui/dist 失败");
        std::fs::write(
            &index,
            format!(
                "<!doctype html><meta charset=\"utf-8\"><title>console placeholder</title>\n\
                 <!-- {PLACEHOLDER_MARKER}：cargo build 时为开发态生成的占位页；发布构建会拒绝它。 -->\n\
                 <p>控制台未构建。运行 <code>mise run build</code> 生成。</p>\n"
            ),
        )
        .expect("写入占位页失败");
        println!("cargo:warning=apps/ui/dist 不存在，已生成占位页（仅开发态）");
        return;
    }

    if release {
        let html = std::fs::read_to_string(&index).unwrap_or_default();
        if html.contains(PLACEHOLDER_MARKER) {
            panic!(
                "发布构建检测到占位页：{}。\n\
                 先跑 `mise run build` 构建真实控制台。",
                index.display()
            );
        }
    }
}
