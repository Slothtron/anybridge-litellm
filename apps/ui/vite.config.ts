import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

// 控制台固定挂在 /console/ 下，由 Rust 侧内嵌 dist/ 提供。
export default defineConfig({
  base: "/console/",
  // Tailwind v4 以 Vite 插件形式参与构建，产物仍是单个 CSS 文件。
  plugins: [tailwindcss()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // 资源文件名带 hash，避免 Rust 内嵌后浏览器缓存旧版本。
    assetsDir: "assets",
  },
  server: {
    port: 5199,
  },
  preview: {
    port: 5199,
  },
});
