//! 控制台产物的**唯一**嵌入口。
//!
//! `apps/ui/dist` 由 `scripts/build.py` 先于 native 构建；这里只做编译期嵌入，
//! 路径写死一处，别处不要再出现 `include_bytes!` / `#[folder]`。

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../apps/ui/dist/"]
pub struct Assets;

impl Assets {
    /// 取一个资源；`path` 是 `/console/` 之后的相对路径（空串等价于 `index.html`）。
    pub fn get_asset(path: &str) -> Option<rust_embed::EmbeddedFile> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            Self::get("index.html")
        } else {
            Self::get(path)
        }
    }
}
