# Apple 风格修订记录 · 2026-10-06

本轮使用本地 `apple-web-design` 的 audit → implement 流程，并结合 `fu-liquiglass-skill/skill/fu-liquiglass/SKILL.md` 的材质与降级指导。HIG MCP 已实际查询 typography、layout 与 Liquid Glass 规则。

## 审查结论：Refine

| 严重性 | 类别 | 位置 | 原状 | 修订 | 原因 |
| --- | --- | --- | --- | --- | --- |
| HIGH | Content and hierarchy | 工作台各面板 | 双层标题、重复开发阶段说明 | 单行标题与紧凑辅助信息 | 让主要工作区获得空间 |
| HIGH | Navigation and controls | toolbar.tsx | 品牌、标题、多个带框按钮竞争 | 项目标题居中，左侧项目命令，右侧视图与导出 | 建立桌面文档工具的稳定顺序 |
| HIGH | Content and hierarchy | preview-area.tsx | 黑底拉伸铺满、宣传式空态 | 固定 16:9 画布、简短空态、单行播放控制 | 画布成为视觉中心 |
| MEDIUM | Typography and readability | media-bin.tsx / inspector.tsx | 搜索、切换、导入堆叠，属性松散 | 紧凑工具行、空态主操作、标签值对齐 | 改善扫描密度 |
| MEDIUM | Materials and visual style | globals.css / tokens.css | 块状灰表格、材质缺少层级 | 实色内容、细分隔线、定向边缘高光与有限阴影 | 用层级建立质感 |

## 已实施

- 顶部去除重复品牌块和重复项目按钮，保留原有命令与快捷键；新建、打开、保存统一在项目菜单，保存仍有独立快捷入口。
- 素材库和检查器可由顶部按钮显示/隐藏。该操作只影响视图，不影响项目与选中素材。
- 素材视图默认网格，复用 shadcn-macos Tabs、Sidebar、Input、Button；文件名与大小继续真实显示。
- 预览保持中性实色，显示独立 16:9 画布；占位播放按钮保持禁用。
- 时间线头部 40px、轨道头 128px；刻度和网格共享六等分坐标，细播放头从零点对齐。
- 玻璃 primitive 复用 `--kc-glass-*`，组合染色、14px blur、定向高光、内缘与外阴影；只用于工具栏与菜单/选择弹出层。表单、素材、预览、时间线实色。
- 降级保持应用独立透明度开关、减少动态效果、Safari 前缀、无 backdrop-filter 的实色基础与 forced-colors 规则。

文件：`src/App.tsx`、`src/components/editor/{toolbar,media-bin,preview-area,inspector,timeline}.tsx`、`src/styles/{globals,tokens}.css`，以及排除新参考目录的 Vite/ESLint 配置。项目 JSON、Rust IO、FFmpeg 接口未修改。

## 参考的使用边界

HIG MCP 是社区服务；macOS 排版与 iOS Dynamic Type 分开查询。其 blur 上限、layer 数量和间距网格标记为 community/convention，不作为 Apple 官方数值采用。保留项目原定的模糊预算与适合中文的字号。

FU—Liquiglass 示例以网页导航为主，本项目保留桌面命令菜单的点击/方向键打开方式；不因为鼠标经过而抢走编辑焦点。没有引入装饰壁纸、折射着色器、Apple 字体或假窗口按钮。

## HIG MCP 安装结果

- 安装：pipx 隔离环境，`hig-mcp 0.1.1`，Python 3.10.11。
- 兼容修复：上游仍导入 `mcp.server.fastmcp`，MCP 2.x 已移除该接口；执行 `python -m pipx inject hig-mcp "mcp<2" --force`，实际固定到 MCP 1.30.0。
- 注册：本机没有独立 `claude` 命令，使用官方 `npx --yes @anthropic-ai/claude-code mcp add hig -- <hig-mcp.exe绝对路径>`；写入用户 Claude local 配置中的当前项目项。健康检查显示 Connected。
- `python -m pipx ensurepath` 已加入用户 Scripts 与 pipx bin 路径，新的终端会读取更新；MCP 配置使用绝对路径避免旧 PATH 影响。
- 这是 Claude 的项目配置，不会自动更改其他 MCP 客户端的配置。本轮通过 stdio 实际调用服务取得设计数据。

## 验证

- lint、22 项既有测试、TypeScript/Vite 生产构建通过。
- Windows Tauri 调试构建通过，输出 `src-tauri/target/debug/kaicut.exe`。
- 实际浏览器检查浅/深色、项目菜单方向键与 Escape、面板折叠、关闭透明度、减少动态效果。
- 尺寸检查：1440×900、1280×820、1024×768、768×1024、390×844、320×844；画布维持 16:9，无页面水平溢出。窄屏减少时间标尺标签密度。
- FU 静态 strict 审计通过，7/8 项检测到；唯一 NOTE 是脚本只识别 `--lg-*` / `--glass-*`，项目实际使用既有 `--kc-glass-*`，未为通过启发式检查而建立第二套 token。
- macOS 原生窗口、Safari 实机、触控硬件和屏幕阅读器仍未实测；CSS 声明存在不等于这些平台已通过验收。

截图见 `screenshots/ui-refined-light.jpg`、`ui-refined-dark.jpg`；原始第一阶段截图保留用于同尺寸对照。
