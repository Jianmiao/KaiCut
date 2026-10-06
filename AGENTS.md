## Skill 使用规则
- UI 设计相关任务，必须显式调用 $apple-web-design skill
- 审查用 audit，规划用 plan，实现用 implement
- 组件优先从 shadcn-macos 获取，不要自行造 Apple 风格组件
- shadcn-macos 的安装命令：npx shadcn-macos@latest add <component>
- 如果 shadcn-macos 没有所需组件，再用 $apple-web-design 指导实现

## KaiCut 工程约定
- 项目使用 pnpm；产品显示名为 KaiCut / 开剪。
- 当前阶段使用 Tauri 2 + Rust + React + TypeScript + Vite + Tailwind CSS v4 + shadcn-macos + Zustand。
- `.kaicut` 在第一阶段是单文件 JSON；不要把媒体二进制写入项目文件。
- UI 需遵循 docs/DESIGN.md 与 docs/DESIGN-TOKENS.md；毛玻璃只用于工具栏、菜单和短小弹出层。
- 不在空壳阶段实现复杂剪辑、AI、云服务或真实 FFmpeg 编码流程。
- 改项目持久化时读取 src-tauri/CONTRACT.md，同步 Rust 与 TypeScript schema；Rust 管理桌面权威状态，Zustand 保存 UI 投影。
- 接入 sidecar 时读取 src-tauri/binaries/README.md 和 THIRD_PARTY_NOTICES.md，记录二进制来源与许可。
- 修改完成后运行 package.json 的 lint/test/build；改 Rust 时另跑 cargo test 和 cargo fmt --check。实际验证后才报告成功。
- apple-web-design-skill/ 与 macos-for-shadcn/ 是参考仓库，保持独立，不纳入应用测试或构建。
