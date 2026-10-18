# 第三方来源与分发状态

KaiCut 自有源码为 Apache-2.0，根 LICENSE 与 NOTICE 适用于有权授权的项目代码；该声明不改变第三方组件原有许可。FFmpeg 独立 sidecar 只接受 LGPL 构建。以下是当前骨架的来源记录，不是完整发布包的最终许可清单。

| 来源 | 使用位置 | 记录 |
| --- | --- | --- |
| shadcn-macos 1.0.5 | `src/components/ui/`、`src/lib/utils.ts`、`src/styles/macos.css` | 通过官方 npm CLI init/add 生成；上游来源 https://registry.npmjs.org/shadcn-macos/1.0.5 。2026-10-06 查询元数据未包含 license、repository、homepage，不能猜测 MIT 许可。对外分发前须取得可核实许可来源。 |
| React、Zustand、Vite、Tailwind CSS、lucide-react 等 | UI / 工具链 | 具体版本在 package.json 与 pnpm-lock.yaml；原始 LICENSE 位于各依赖包。发布时按实际依赖生成 notices。 |
| @modelcontextprotocol/sdk 1.32.1 | `mcp/` 开发工具 | 官方 TypeScript MCP SDK，包元数据与随包 LICENSE 为 MIT；来源 https://github.com/modelcontextprotocol/typescript-sdk 。固定版本写入 pnpm-lock.yaml，仅 MCP stdio 工具使用，不引入前端浏览器包或 FFmpeg。 |
| Tauri 与 Rust crates | 原生壳与项目持久化 | Cargo.lock 固定解析结果；发布时按实际特性与依赖逐项保留许可和对应要求。 |
| FFmpeg 7.1 | Build metadata under `src-tauri/binaries/`; prebuilt sidecars are intentionally not committed | Source `https://git.ffmpeg.org/ffmpeg.git`, tag `n7.1`, commit `b08d7969c550a804a59511c7b83f2dd8cc0499b8`; LGPL-2.1-or-later configuration with `--disable-gpl --disable-nonfree --disable-version3 --disable-everything`, `--enable-mediafoundation`, `h264_mf`, built-in AAC, `file` + `pipe` protocols with networking disabled, and the documented MP4/MOV/MP3/WAV/image components. The 2026-10-11 build adds LGPL timeline composition/mixing filters plus PCM/rawvideo test output. Candidate ffmpeg SHA-256 `9752FD44C9E1867C8D26702EF81DF1AAD1442C1099888D02FED7C29DE63AFF39`; candidate ffprobe SHA-256 `3E3DFE5C77E7F3B8F9DE1C8EC2CCFEA4EC1E9F34E930B6F918894C9D2838219A`. Configure/build/probe scripts and exact-binary records are under `src-tauri/binaries/`; the matching FFmpeg source checkout is required to rebuild the candidate. No local patches. |
| zlib / GCC runtime | Statically linked into the locally built Windows FFmpeg candidate (prebuilt executables are not committed) | License texts are included under `src-tauri/binaries/licenses/`: zlib, GCC runtime exception/GPL text, and libatomic runtime text. The candidate binaries import only Windows system/UCRT APIs. |
| 用户提供的 PingFangSC 字体仓库 | 仅本机字体安装 | 原目录在 .gitignore 中，应用不导入其中字体字节。仓库的 MIT 文本不能单独证明对其中 Apple 字体的再分发权；不按 Apache-2.0 重新标注字体，不打包发布。 |

对 shadcn-macos 组件的本地修改：连接 KaiCut token、修正控件尺寸、标签、键盘导航、弹窗焦点/回焦、减少动态效果；侧栏、卡片和主要编辑区改为实色。没有使用本地 `macos-for-shadcn` 参考项目的组件或 CSS。

图标 `public/kaicut.svg` 为本项目创建的几何 K 标记，通过 Tauri CLI 生成应用图标；不包含 Apple 字体、商标或系统资源。
