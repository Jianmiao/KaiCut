# 第三方来源与分发状态

KaiCut 自有源码为 Apache-2.0，根 LICENSE 与 NOTICE 适用于有权授权的项目代码；该声明不改变第三方组件原有许可。FFmpeg 独立 sidecar 只接受 LGPL 构建。以下是当前骨架的来源记录，不是完整发布包的最终许可清单。

| 来源 | 使用位置 | 记录 |
| --- | --- | --- |
| shadcn-macos 1.0.5 | `src/components/ui/`、`src/lib/utils.ts`、`src/styles/macos.css` | 通过官方 npm CLI init/add 生成；上游来源 https://registry.npmjs.org/shadcn-macos/1.0.5 。2026-10-06 查询元数据未包含 license、repository、homepage，不能猜测 MIT 许可。对外分发前须取得可核实许可来源。 |
| React、Zustand、Vite、Tailwind CSS、lucide-react 等 | UI / 工具链 | 具体版本在 package.json 与 pnpm-lock.yaml；原始 LICENSE 位于各依赖包。发布时按实际依赖生成 notices。 |
| Tauri 与 Rust crates | 原生壳与项目持久化 | Cargo.lock 固定解析结果；发布时按实际特性与依赖逐项保留许可和对应要求。 |
| FFmpeg | 可选 sidecar | 当前未选择/分发二进制；目标 LGPL-2.1-or-later，排除 GPL/nonfree 组件；交付前记录版本、来源、SHA-256、configure 选项、许可证、对应源码及补丁。若需要 LGPL v3 依赖，须重新记录实际许可版本；不得仍标为 LGPL 2.1。 |
| 用户提供的 PingFangSC 字体仓库 | 仅本机字体安装 | 原目录在 .gitignore 中，应用不导入其中字体字节。仓库的 MIT 文本不能单独证明对其中 Apple 字体的再分发权；不按 Apache-2.0 重新标注字体，不打包发布。 |

对 shadcn-macos 组件的本地修改：连接 KaiCut token、修正控件尺寸、标签、键盘导航、弹窗焦点/回焦、减少动态效果；侧栏、卡片和主要编辑区改为实色。没有使用本地 `macos-for-shadcn` 参考项目的组件或 CSS。

图标 `public/kaicut.svg` 为本项目创建的几何 K 标记，通过 Tauri CLI 生成应用图标；不包含 Apple 字体、商标或系统资源。
