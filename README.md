# KaiCut / 开剪

**让剪辑回到画面本身。** 本地优先的桌面视频剪辑工作台，当前先交付 Windows；macOS 等获得设备后再适配与验收，Linux 继续延后。

当前没有 Mac 设备，开发、截图、性能测试、安装包与发布门槛均以 Windows 为准；Apple 风格仅作为视觉参考，Windows 使用自身窗口、字体与系统交互。保留已有跨平台边界，不把未验证的 macOS 能力视为已支持。

当前是第一阶段骨架，已经建立 Tauri 2 + Rust 桌面宿主，以及 React + TypeScript + Vite + Tailwind CSS v4 + shadcn-macos + Zustand 界面。

## 当前可用

- 顶部工具栏、素材库、中央预览占位、检查器、底部 2V + 2A 空轨道。
- 原生文件对话框新建、打开、保存单文件 `.kaicut` JSON；后续保存覆盖当前项目文件。
- 导入 mp4 / mov / mp3 / wav / png / jpg / jpeg，记录文件名、类型、大小、修改时间和原始路径；显示缩略图占位。
- 素材搜索、列表/网格切换、选中素材查看元数据。
- 严格 JSON/schema 校验，拒绝未来版本和未支持的片段，取消和失败保留原项目；同目录临时写入后替换。
- 新建、打开、关闭时提醒未保存更改。
- 固定深色，应用内独立透明度与减少动态效果开关，偏好本地保存；旧浅色/系统主题设置会被忽略。
- FFmpeg sidecar 状态接口及可选打包配置，缺失时如实显示不可用。

预览播放、真实缩略图/波形、片段剪辑、撤销重做、自动保存、媒体重新链接、编码导出尚未实现。原始媒体默认只引用，不复制到项目；移动素材后目前需要重新导入。导出按钮显示说明，禁用控件不会执行假操作。

## 环境

- Node.js 24，pnpm 11.25.0（由 `packageManager` 固定）。依赖精确版本与 `pnpm-lock.yaml` 已锁定。
- Rust stable / MSVC 工具链，最低 Rust 1.90；开发使用 Rust 1.97.1。
- Windows：Microsoft C++ Build Tools 与 WebView2 Runtime。
- macOS（延期）：获得设备后再准备 Xcode 工具链、构建、签名和公证；不阻塞当前 Windows 交付。
- 平台前置条件见 [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)。

## 启动与验证

```sh
pnpm install --frozen-lockfile
pnpm tauri:dev
```

仅预览网页：`pnpm dev`，地址 `http://127.0.0.1:5173`。浏览器模式只能下载项目副本，不能确认磁盘写入成功，也不能取得素材持久路径。桌面版使用 Rust 项目状态和原生文件读写。

```sh
pnpm lint
pnpm test
pnpm build
cargo test --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
pnpm tauri build --no-bundle --debug
```

Windows 调试程序输出到 `src-tauri/target/debug/kaicut.exe`。这不是经过签名的发布安装包。macOS 构建和公证必须在对应平台另行验证。

## 本机字体

界面使用系统字体，时间码保持等宽。macOS 使用 `-apple-system`；Windows 使用 `"Segoe UI", "Microsoft YaHei UI", system-ui`。应用已移除用户字体别名和加载声明；`PingFangSC/` 不参与源码或构建，未卸载用户已有系统字体。

当前 Windows 桌面材质使用 Windows 11 Acrylic，只有工具栏与素材侧边栏透出背景，主要编辑区保持实色。macOS Sidebar vibrancy 分支保留但待设备后验证。开关复用现有透明度设置；系统减少透明度、高对比度和不支持设备采用实色。接口和平台边界见 [APPEARANCE.md](src-tauri/APPEARANCE.md)。

## 项目格式与状态

`.kaicut` 是 UTF-8 JSON，schemaVersion 为 1，包含项目 ID、名称、UTC 时间戳、有理数帧率、外部媒体引用和空轨道。项目最大 10 MiB。第一阶段拒绝包含真实片段的项目，避免误删未来版本的数据。该格式明确替代规划阶段的目录包建议，未来迁移必须显式设计。

桌面运行时 Rust 是项目的权威状态；Zustand 接收会话投影、控制操作繁忙状态与选择，另存 UI 偏好。浏览器模式提供独立演示适配，不参与桌面文件写入。接口见 [CONTRACT.md](src-tauri/CONTRACT.md)。

## FFmpeg sidecar

骨架**没有携带或下载 FFmpeg 二进制**。当前只实现固定打包路径发现、受限 `-version` 探测和缺失状态，不执行媒体解码或编码。后续仅交付经过验证的 **LGPL FFmpeg 独立 sidecar**，默认目标 LGPL-2.1-or-later；关闭 GPL/nonfree 组件，不包含 libx264/libx265。按 [sidecar 交付说明](src-tauri/binaries/README.md)保留实际构建、许可、对应源码和校验和证据后才打包。当前版本探测不能证明 LGPL 合规。

H.264 编码与软件解码是两种能力。没有可用的已审查 H.264 编码器时，不能靠启用 GPL 编码器实现“软件回退”；应提示该导出预设不可用，或由用户选择经过验证的替代格式。

## 目录

```text
src/
  components/editor/   工作台、素材库、预览与时间线占位
  components/ui/       shadcn-macos 生成并补齐可访问性的组件
  hooks/               主题同步、弹窗焦点
  stores/              项目会话投影与偏好
  lib/                 JSON 校验与浏览器适配工具
  types/               项目/媒体结构
  styles/              KaiCut tokens、主题、上游样式及适配
src-tauri/
  src/                 Rust 项目、文件命令、sidecar 状态
  capabilities/        桌面能力约束
  binaries/            sidecar 放置规范，不含二进制
public/kaicut-icon.png  KaiCut 用户提供的影片剪辑图标（透明角）
docs/                  PRD、架构、任务、UI 设计
```

`apple-web-design-skill/`、`macos-for-shadcn/` 是原有独立参考目录；不加入应用构建、包管理工作区或测试范围。

## 许可证

KaiCut 自有源码使用 **Apache-2.0**，全文见 [LICENSE](LICENSE)，版权与来源提示见 [NOTICE](NOTICE)。FFmpeg 单独使用 LGPL，二者不是对同一代码的双重授权。第三方组件保持各自许可，详见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。`shadcn-macos@1.0.5` 的 npm 元数据未声明许可证，当前不能声称已完成对外发布许可审查。

## 规划

[产品](docs/PRD.md) · [架构](docs/ARCHITECTURE.md) · [里程碑](docs/TASKS.md) · [设计](docs/DESIGN.md) · [Tokens](docs/DESIGN-TOKENS.md)
