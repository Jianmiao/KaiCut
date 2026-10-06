# 第一阶段交付记录

日期：2026-10-06。范围：桌面空壳与基础项目/素材管理。没有实现真实剪辑、AI、云服务或编码。

## 后续许可与字体修订（2026-10-06）

- 按用户最新要求改为 Apache-2.0 自有代码 + 独立 LGPL FFmpeg；根 LICENSE 使用 Apache 官方全文，新增 NOTICE，同步 package.json/Cargo.toml、README、贡献指南、第三方来源、架构与里程碑。参考仓库及字体仓库的原许可文件未修改。
- FFmpeg 尚未选择/打包二进制；默认构建基线 LGPL-2.1-or-later，禁止 GPL/nonfree，排除 libx264/libx265，H.264 编码按能力探测决定，不能假定软件回退。版本接口不等于许可审计。
- `PingFangSC/` 克隆已完整（ec0d240）；TTF 的内部 weight 全为 400 且 family 分散，选用用户提供的 Regular/Medium/Semibold，原样安装到当前 Windows 用户字体目录，用本地别名映射 400/500/600。没有更改字体数据、打包 WOFF2 或改字体许可证。
- 字体验证：Windows 已安装字体枚举确认三个 family；实际页面存在三个 local-only font-face，400/500/600 中文加载检查均通过；侧栏使用统一字体 token，时间码保持 monospace；1024px 页面无水平溢出。效果见 `screenshots/pingfang-local-light.jpg`。
- 此次 lint、22 项既有测试、生产前端构建和 Windows Tauri 调试构建通过。FFmpeg 编码没有测试，因为尚未交付媒体引擎二进制。

## 文件清单

- 根工程：`package.json`、`pnpm-lock.yaml`、`pnpm-workspace.yaml`、`index.html`、`tsconfig.json`、`tsconfig.app.json`、`tsconfig.node.json`、`vite.config.ts`、`vitest.config.ts`、`eslint.config.js`、`components.json`、`.gitignore`。
- 说明：`README.md`、Apache-2.0 全文 `LICENSE`、版权/第三方提示 `NOTICE`、`CONTRIBUTING.md`、`AGENTS.md`、`THIRD_PARTY_NOTICES.md`。许可证按用户最新要求更新；FFmpeg 保持独立 LGPL，仅接入约束、没有二进制交付。
- 入口：`src/App.tsx`、`src/main.tsx`、`src/vite-env.d.ts`。
- 工作台：`src/components/editor/toolbar.tsx`、`media-bin.tsx`、`preview-area.tsx`、`inspector.tsx`、`timeline.tsx`。
- 上游生成与适配组件：`src/components/ui/` 的 button、sidebar、tabs、input、select、dialog、sheet、dropdown-menu、context-menu、slider、progress 共 11 个。上游没有 `menu`，使用 dropdown-menu / context-menu 对应。
- 状态与模型：`src/stores/project-store.ts`、`preferences-store.ts`、`src/types/project.ts`、`src/lib/kaicut-format.ts`、`utils.ts`。
- 主题与可访问性：`src/styles/tokens.css`、`macos.css`、`globals.css`、`src/hooks/use-preferences.ts`、`use-dialog-focus.ts`。
- 前端测试：`src/lib/kaicut-format.test.ts`、`src/stores/project-store.test.ts`、`src/components/ui/accessibility.test.tsx`、`src/test/setup.ts`。
- 原生工程：`src-tauri/Cargo.toml`、`Cargo.lock`、`build.rs`、`tauri.conf.json`、`tauri.sidecar.conf.json`、`capabilities/default.json`、`.gitignore`、`CONTRACT.md`、`binaries/README.md`。
- 原生实现：`src-tauri/src/main.rs`、`lib.rs`、`project.rs`、`commands.rs`、`media_engine.rs`。项目模块内包含 Rust 测试。
- 图标与截图：`public/kaicut.svg`、`src-tauri/icons/` CLI 生成的图标、`docs/screenshots/scaffold-light.jpg`、`scaffold-dark.jpg`。
- 文档同步：`docs/ARCHITECTURE.md`、`docs/TASKS.md`、`docs/DESIGN-TOKENS.md`、本记录。`DESIGN.md` 与 `PRD.md` 未改写。

## 验证结果

| 命令 / 检查 | 结果 |
| --- | --- |
| `pnpm install --frozen-lockfile` | 通过；精确锁定依赖，peer 检查无冲突 |
| `npx --yes shadcn-macos@latest init` | 成功；本次解析 1.0.5 |
| `npx --yes shadcn-macos@latest add button sidebar tabs input select dialog sheet dropdown-menu context-menu slider progress` | 成功；安装 11 组件 |
| `pnpm lint` | 通过，包含生成并适配的组件 |
| `pnpm test` | 22 项通过 |
| `pnpm build` | TypeScript + Vite 生产构建通过 |
| `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check` | 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | 5 项通过 |
| `pnpm tauri build --no-bundle --debug` | Windows x64 调试程序构建通过 |
| Windows 程序启动 | 进程存活、响应正常，窗口标题 KaiCut / 开剪；检查后正常关闭 |
| 浏览器界面 | 重命名、导入合成 PNG、素材选中/检查器、标签页方向键、导出说明弹窗通过；控制台未见错误 |
| 主题与尺寸 | 深浅色、透明度独立关闭、减少动态效果；1280×820 / 1024×720 / 690×820；无水平溢出，4 条轨道可访问 |

修复过的环境/集成问题：typescript-eslint 尚不兼容 TS 7，固定为 TS 6.0.3；lint 与 Tailwind 扫描限定应用，避免参考仓库混入；修正上游暗色变量与浅色界面冲突；Vite 排除 Rust target 目录，避免生成产物引发热重载或文件占用警告。

Rust 链接器会将中文“正在创建库…”信息作为 warning 输出；构建退出码为 0，不影响程序产物。

## 实际边界

- Windows 原生对话框的完整人工流程、macOS 构建/签名、公证、VoiceOver/NVDA 实机仍待验证；Rust 文件逻辑已用临时目录测试。
- 浏览器保存只是请求下载，不能确认写盘，因此不伪报成功或清除未保存状态；桌面版成功写入才清除未保存状态。
- `.kaicut` 单文件 JSON 保存原始媒体引用，不包含视频字节；暂不提供自动保存、失联媒体重新链接或跨版本迁移。
- FFmpeg 仅有固定打包路径发现、版本探测与可选配置，没有捆绑二进制；不进行真实解码/编码。
- 当前为调试构建，没有制作签名安装包。shadcn-macos 1.0.5 的许可缺口见第三方来源记录，发布前需要解决。

下一步先在 macOS/Windows 实测原生文件流程，选定可再分发 FFmpeg 构建并完成媒体探测，再实现真正的缩略图、波形和首帧预览。
