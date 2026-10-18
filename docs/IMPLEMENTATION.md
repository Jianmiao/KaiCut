# 第一阶段交付记录

日期：2026-10-06 起持续更新。范围：桌面空壳、基础项目/素材管理和运行时媒体能力探针。没有实现真实剪辑、AI、云服务或完整导出。

## 后续许可与字体修订（2026-10-06）

- 按用户最新要求改为 Apache-2.0 自有代码 + 独立 LGPL FFmpeg；根 LICENSE 使用 Apache 官方全文，新增 NOTICE，同步 package.json/Cargo.toml、README、贡献指南、第三方来源、架构与里程碑。参考仓库及字体仓库的原许可文件未修改。
- Windows FFmpeg 7.1 sidecar 已按 LGPL-2.1-or-later 配置构建，禁止 GPL/nonfree/version3，排除 libx264/libx265；启用 `h264_mf` 和内置 AAC。仓库仅提交构建配方、许可文本、验证记录和候选哈希；预编译 `ffmpeg`/`ffprobe` x64 文件由 Git 忽略，H.264 能力仍按 Windows MFT 运行时探测，不能假定所有机器可用。
- 字体按最新设计约定使用系统字体栈：macOS 由 `-apple-system` 自动选择系统中文字体，Windows 使用 `Segoe UI` / `Microsoft YaHei UI`；不导入或打包用户字体文件。时间码保持等宽字体。
- 已通过 lint、前端测试、生产构建、Rust 测试和本机包含 FFmpeg/ffprobe 的 Windows Tauri sidecar 打包；`h264_mf + aac` 代表性编码探针和 PNG/H.264/AAC ffprobe 读取已在开发机验证，完整导出流程仍未接入。GitHub 源码快照不含 sidecar 可执行文件；须依照 `src-tauri/binaries/README.md` 准备精确版本与本地二进制后，才可启用 Tauri sidecar 打包配置。

## 文件清单

- 根工程：`package.json`、`pnpm-lock.yaml`、`pnpm-workspace.yaml`、`index.html`、`tsconfig.json`、`tsconfig.app.json`、`tsconfig.node.json`、`vite.config.ts`、`vitest.config.ts`、`eslint.config.js`、`components.json`、`.gitignore`。
- 说明：`README.md`、Apache-2.0 全文 `LICENSE`、版权/第三方提示 `NOTICE`、`CONTRIBUTING.md`、`AGENTS.md`、`THIRD_PARTY_NOTICES.md`。许可证按用户最新要求更新；FFmpeg 保持独立 LGPL，二进制来源和哈希记录见 `src-tauri/binaries/`。
- 入口：`src/App.tsx`、`src/main.tsx`、`src/vite-env.d.ts`。
- 工作台：`src/components/editor/toolbar.tsx`、`media-bin.tsx`、`preview-area.tsx`、`inspector.tsx`、`timeline.tsx`。
- 上游生成与适配组件：`src/components/ui/` 的 button、sidebar、tabs、input、select、dialog、sheet、dropdown-menu、context-menu、slider、progress 共 11 个。上游没有 `menu`，使用 dropdown-menu / context-menu 对应。
- 状态与模型：`src/stores/project-store.ts`、`preferences-store.ts`、`src/types/project.ts`、`src/lib/kaicut-format.ts`、`utils.ts`。
- 主题与可访问性：`src/styles/tokens.css`、`macos.css`、`globals.css`、`src/hooks/use-preferences.ts`、`use-dialog-focus.ts`。
- 前端测试：`src/lib/kaicut-format.test.ts`、`src/stores/project-store.test.ts`、`src/components/ui/accessibility.test.tsx`、`src/test/setup.ts`。
- 原生工程：`src-tauri/Cargo.toml`、`Cargo.lock`、`build.rs`、`tauri.conf.json`、`tauri.sidecar.conf.json`、`capabilities/default.json`、`.gitignore`、`CONTRACT.md`、`binaries/README.md`。
- 原生实现：`src-tauri/src/main.rs`、`lib.rs`、`project.rs`、`commands.rs`、`media_engine.rs`。项目模块内包含 Rust 测试；`media_engine` 提供 sidecar 状态和运行时 `media_probe`。
- 图标与截图：`public/kaicut.svg`、`src-tauri/icons/` CLI 生成的图标、`docs/screenshots/scaffold-light.jpg`、`scaffold-dark.jpg`。
- 文档同步：`docs/ARCHITECTURE.md`、`docs/TASKS.md`、`docs/DESIGN-TOKENS.md`、本记录。`DESIGN.md` 与 `PRD.md` 未改写。

## 验证结果

| 命令 / 检查 | 结果 |
| --- | --- |
| `pnpm install --frozen-lockfile` | 通过；精确锁定依赖，peer 检查无冲突 |
| `npx --yes shadcn-macos@latest init` | 成功；本次解析 1.0.5 |
| `npx --yes shadcn-macos@latest add button sidebar tabs input select dialog sheet dropdown-menu context-menu slider progress` | 成功；安装 11 组件 |
| `pnpm lint` | 通过，包含生成并适配的组件 |
| `pnpm test` | 61 项通过（媒体状态、缓存、懒加载、首帧组件和布局宽度约束在内） |
| `pnpm build` | TypeScript + Vite 生产构建通过 |
| `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check` | 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked -- --include-ignored` | 设置 KAICUT_THUMBNAIL_TEST_VIDEO 后 22 项通过，含真实 sidecar 旋转/透明度/非方形像素/缓存用例 |
| `pnpm tauri build --config src-tauri/tauri.sidecar.conf.json` | 上一轮 release 包通过；本轮因旧版 kaicut.exe 正在运行、文件被锁定而失败（os error 5），未替换旧 release 程序 |
| `pnpm tauri build --debug --config src-tauri/tauri.sidecar.conf.json` | 本轮 Windows debug 程序、MSI 和 NSIS 包通过，输出在 src-tauri/target/debug/；未关闭或替换正在运行的旧窗口 |
| Windows 程序启动 | 进程存活、响应正常，窗口标题 KaiCut / 开剪；检查后正常关闭 |
| 浏览器界面 | 重命名、导入合成 PNG、素材选中/检查器、标签页方向键、导出说明弹窗通过；控制台未见错误 |
| 主题与尺寸 | 固定深色、透明度独立关闭、减少动态效果；1280×820 / 1024×720 / 690×820；无水平溢出，4 条轨道可访问 |

修复过的环境/集成问题：typescript-eslint 尚不兼容 TS 7，固定为 TS 6.0.3；lint 与 Tailwind 扫描限定应用，避免参考仓库混入；修正上游暗色变量与浅色界面冲突；Vite 排除 Rust target 目录，避免生成产物引发热重载或文件占用警告。

Rust 链接器会将中文“正在创建库…”信息作为 warning 输出；构建退出码为 0，不影响程序产物。

## 实际边界

- Windows 原生对话框的完整人工流程、macOS 构建/签名、公证、VoiceOver/NVDA 实机仍待验证；Rust 文件逻辑已用临时目录测试。
- 浏览器保存只是请求下载，不能确认写盘，因此不伪报成功或清除未保存状态；桌面版成功写入才清除未保存状态。
- `.kaicut` 单文件 JSON 保存原始媒体引用，不包含视频字节；自动保存使用应用数据目录中的独立恢复快照，失联媒体通过运行时状态与重新链接恢复；跨版本迁移仍未进入本阶段。
- FFmpeg/ffprobe 使用固定打包路径；已接入真实静态首帧/图片预览、按需缩略图和媒体信息。连续播放、波形和完整导出尚未接入。探测、首帧、错误和缓存不会写入 `.kaicut`。当前包未做 Windows 代码签名。
- shadcn-macos 1.0.5 的许可缺口见第三方来源记录，发布前需要解决；FFmpeg 的专利义务也不因 LGPL 构建而自动消失。

真实 Windows 窗口的完整导入流程因用户要求暂停电脑控制，列为待验收；本轮不再操作窗口。测试范围、缓存预算和已知限制见 [MEDIA-VALIDATION.md](./MEDIA-VALIDATION.md)。下一步完成该验收，再接入波形与连续播放。macOS 实测、签名和公证仍待设备。

## 剪映结构与 Apple 风格

用户澄清方向为“剪映那种 UI 然后 Apple 式美化”，已停止二进制分析。参考用户提供的 `references/剪映专业版.png`，直接使用 KaiCut 自有组件调整：

- 素材/播放器/项目参数三分区，区域间距8px、容器圆角10px；默认素材320px、检查器300px，保留可拖拽与键盘调节，最小桌面窗口保留播放器300px。
- 素材导入面板与顶部导入入口；右侧无选择时展示当前项目参数，选择素材后显示真实媒体信息。
- 一条36px时间线工具行，项目/轨道/帧率移到底部；删除伪片段和假波形，保留4条真实空轨道。
- 中性深灰与单一蓝色导出，系统字体与原有动效/透明度降级；不复制剪映代码、图片资源或商标，不添加会员、AI和运营模块。

后台无界面 Edge 渲染检查通过：1440×900、1024×720 无页面水平溢出；分隔器位置与面板边界一致，键盘箭头每次移动8px；强制颜色下焦点可见、减少动态效果时无额外动画。检查曾发现旧CSS覆盖网格列定义，已修复并重新测量三列边界。

预览 `screenshots/jianying-apple-empty-render.png` 和 `screenshots/jianying-apple-compact-render.png` 来自真实前端的后台渲染，不是 Tauri 原生窗口截图，也不能验证原生毛玻璃。没有操作用户桌面窗口。

## 无网格时间线与基础编辑（2026-10-11）

移除轨道内容区纵横格线，仅保留顶部标尺、轨道头边界和白色播放头。接入真实片段模型和添加、分割、删除、副本、移动、裁切、吸附、缩放、锁轨、撤销/重做；检查器增加帧数编辑。`.kaicut` 升至v2，支持合法v1空轨项目及恢复点迁移。详情和未完成项见 [EDITING-OPERATIONS.md](./EDITING-OPERATIONS.md)。这不代表已完成剪映所有操作或时间线视频播放。

本轮最终验证：`pnpm lint`、`pnpm test`（106项，含片段播放时间映射、连续片段选择和音频时钟映射）、`pnpm build`、`cargo fmt --check`、`cargo test --locked`（32通过、1个真实sidecar用例按环境忽略）通过。Tauri asset协议、受控`<video>`/`<audio>`播放、相邻片段连续衔接、空隙播放头推进、音频轨跟随、漂移校准和运行时音频波形代码已接入；独立 target 目录的 debug MSI / NSIS 构建通过。当前未启动窗口。

Windows 窗口装饰已在 Tauri 配置和运行时同时关闭；顶部栏自绘控件是唯一窗口控制入口。浏览器预览和 macOS 后续适配不显示这组三个 Windows 控件。
