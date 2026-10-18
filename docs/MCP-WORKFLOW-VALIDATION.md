# MCP 真实媒体工作流验收

日期：2026-10-16。平台：Windows x64，Tauri 调试构建与真实 WebView2。通过官方 MCP SDK 的 stdio 客户端连接应用，未使用 computer-use、鼠标或键盘自动化。以下是本次实际运行记录，接口用法见 [MCP.md](./MCP.md)。

## 结果

16 个 MCP 工具已发现；新增 `project_open_path`、`project_import_paths`、`project_save_as`。测试使用合成 MP4、WAV、PNG 的独立副本，验证导入、探测、添加片段、源裁切、分割、移动、图片片段、音轨音量及淡化，再通过真实页面定位、播放、暂停、另存、重开。

| 检查 | 实际结果 |
| --- | --- |
| 窗口隔离 | `--mcp --mcp-hidden --mcp-validation`；主窗口每次采样均 `visible=false / focused=false`。 |
| Rust / UI 一致性 | 每次修改后等到相同 projectId/revision；最终 revision 15，4 轨、4 片段、3 素材，dirty=false。 |
| 视频源裁切 | 时间线第 15 帧对应视频源 1.500000 秒；readyState=4，seeking=false，errorCode=null。 |
| 独立音频 | 第 15 帧源时间 0.500000 秒、增益 0.5；第 45 帧视频空隙仍解析到音频轨。 |
| 图片加载 | 第 105 帧显示图片片段；complete=true，naturalWidth=160，naturalHeight=90。 |
| 真实播放 | 播放头从 0 推进到第 10 帧；同一视频元素从 1.000000 到 1.344407 秒，音频从 0 到 0.302300 秒，均 paused=false / readyState=4。 |
| 暂停稳定性 | 停在第 11 帧。等待解码定位稳定后，再隔 300ms 采样，视频保持 1.366666 秒、音频保持 0.366666 秒，均 paused=true。 |
| 文件操作保护 | 过期 revision 导入被拒；另存到已有路径被拒且原文件不变；未保存修改时打开项目被拒。 |
| 同 ID 重开 | 保存后重开同一项目，轨道一致、撤销历史清空、播放头归零、选择清空，并能再次解码播放。 |
| 素材完整性 | 原始和复制素材的 SHA-256、大小、修改时间均保持不变。 |

DOM 的 `visibility=visible / hidden=false` 与上面的原生窗口隐藏并不矛盾。Tauri 主窗口与 WebView 文档可见性是不同状态；Windows 的 `MainWindowHandle` 还可能指向 Tao 的不可见辅助窗口。验收以 Rust 读取的 `main` 窗口状态为准。隐藏模式额外设置 `focus=false`，避免初始化时请求焦点。

本次修复了三个异步生命周期问题：新导入素材先检查链接再探测，避免请求代次互相失效；同 ID 重开使旧探测/缩略图/波形请求失效，并重置编辑视图；旧波形响应及其清理回调不能覆盖或删除新请求。图片加载失败后也不能直接重新启动空白预览，需先重试读取。

## 可复查产物

最终验收目录：

```text
src-tauri/target/mcp-workflow-validation/run-026d8a50-59ee-4912-b7e7-64a28341b170/
  validation.json
  MCP 初次保存.kaicut
  MCP 剪辑验收.kaicut
  原始 视频;[literal].mp4
  原始 音频.wav
  原始 图片%03d.png
```

该目录在构建产物范围，不进入 Git；报告没有连接令牌。测试实例已停止，所属连接描述文件已清理，产物保留用于复查。使用独立 app-data 配置，没有读取用户恢复点或使用用户媒体。

验收可执行文件：`src-tauri/target/debug/kaicut.exe`，SHA-256：

```text
E20E914FCB65A84B955CC28309A6A630573F7B0840CC798D1BF90C6929910DAA
```

## 命令与检查

| 命令 | 结果 |
| --- | --- |
| `pnpm lint` | 通过。 |
| `pnpm test` | 18 个测试文件、198 项通过。 |
| `pnpm test:mcp` | 15 项通过，包含 SDK 握手及新项目工具参数校验。 |
| `pnpm build` | 由最终 Tauri 打包的 beforeBuildCommand 执行，TypeScript 与 Vite 通过。 |
| `cargo test --locked`（src-tauri） | 94 项通过，6 项已有设备环境用例默认忽略。 |
| `cargo fmt --check`（src-tauri） | 通过。 |
| `pnpm tauri:build --debug --config src-tauri/tauri.sidecar.conf.json` | 通过，生成 MSI 与 NSIS 调试安装包。 |
| `pnpm mcp:workflow-check --connection-file <独立连接文件> --output-dir <target下独立目录>` | 真实工作流 11 组检查通过。 |
| `pnpm mcp:smoke --connection-file <同一连接文件>` | 官方 SDK 握手、16 工具发现、Rust/UI 查询通过。 |
| `git diff --check` | 通过。 |

Windows 链接器输出“正在创建库/对象”被 Rust 报为 linker_messages warning；构建与测试退出码均为 0，没有编译失败。安装包位于 `target/debug/bundle/msi/KaiCut_0.1.0_x64_en-US.msi` 和 `target/debug/bundle/nsis/KaiCut_0.1.0_x64-setup.exe`，本轮只打包，没有执行安装。

## 验收边界与下一步

这证明当前 Windows 设备上的真实应用、媒体解码及 MCP 工作流联通，不是截图验收，也不证明音质、长时间音画同步、复杂编码素材兼容性或逐帧画面一致性。本轮没有重新执行 6 项默认忽略的设备测试，也没有升级 FFmpeg 或 `.kaicut` schema。

下一步可把导出预检、任务启动/取消接入同一 MCP 受限命令层，让“导入→编辑→保存→导出文件验证”成为可重复回归；继续沿用明确目标路径、revision 检查和禁止覆盖策略。

## 本轮文件

- 原生桥接：`src-tauri/src/mcp_project.rs`、`src-tauri/src/mcp_bridge.rs`、`src-tauri/src/lib.rs`。
- MCP 适配与验收：`mcp/tools.mjs`、`mcp/bridge.mjs`、`mcp/server.test.mjs`、`mcp/workflow-check.mjs`、`package.json`。
- 页面状态与预览：`src/stores/project-store.ts` 及测试、`src/types/project.ts`、`src/hooks/use-mcp-bridge.ts`、`src/components/editor/preview-area.tsx` 及测试。
- 文档：`README.md`、`src-tauri/CONTRACT.md`、`docs/MCP.md`、本文。
