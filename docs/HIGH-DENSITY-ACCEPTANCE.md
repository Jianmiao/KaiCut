# 高密度视觉修订验收

日期：2026-10-06

本轮只调整深色界面的视觉密度、面板层次、预览控制条和时间标尺。新建、打开、保存、导入、项目格式、媒体状态、Rust 命令和导出占位逻辑没有改动。

## 实际截图

- [Windows Tauri 原生窗口](./screenshots/high-density/native-dark.jpg)：1283 × 850，包含 Windows 标题栏、窗口边框和底部状态栏；直接保存窗口捕获结果，没有裁剪或重绘。
- [与 Final Cut Pro 并排对比](./screenshots/high-density/comparison-final-cut-pro.png)：左侧为 KaiCut 原图，右侧为 Apple 官方用户指南中的 FCP 窗口截图。比较画布只添加了标签和说明，没有给 KaiCut 填入虚构媒体。
- [键盘焦点窗口](./screenshots/high-density/native-keyboard-focus.jpg)：Tab 导航后的菜单焦点，焦点轮廓可见。
- [关闭透明度回退窗口](./screenshots/high-density/native-solid.jpg)：应用内关闭透明效果后的实色回退。

FCP 参考来源：[Final Cut Pro 用户指南](https://support.apple.com/guide/final-cut-pro/welcome/mac)；本地原图为 `docs/references/final-cut-pro-mac-official.png`。两张图的窗口尺寸和内容不同，因此只用于比较层级、控件尺寸和留白，不能作为功能或实际剪辑密度相同的证明。

## 自检结果

| 项目 | 结果 | 证据 |
| --- | --- | --- |
| 侧边栏 / 检查器 `#2A2A2C`，查看器容器 `#1A1A1C` | 通过 | 原生窗口可见四区层次；CSS token 已同步 |
| 画面周围 `#0E0E10` | 通过 | 中央预览区域实测为近黑实色 |
| 时间线 `#202022`，分隔线白色10% | 通过 | 时间线与上方查看器边界可辨，轨道无亮色长条 |
| 工具栏44px、轨道34px、轨道头112px | 通过 | 最终窗口和 CSS token；粗指针模式才扩展触控行高 |
| 空态图标和文字缩小 | 通过 | 素材32px/13px、查看器18px/12px、检查器20px/12px |
| 每1秒刻度、5秒主刻度、刻度白色6% | 通过 | 最终截图中主刻度为00:00、00:05等，细刻度共享30秒标尺坐标 |
| 播放头三角缩小并保留白边 | 通过 | CSS SVG 4.5 × 6，白色1px描边；红色竖线共享起点 |
| 播放控制条浮在查看器底部 | 通过 | 控制条不再占用独立布局行，透明度关闭时回退查看器实色 |
| 原生毛玻璃 | 通过（Windows） | `KaiCut.log` 记录 `transparency=true, material=Native: Windows Acrylic 已由 DWM 接受` |
| 实色降级 | 通过（Windows） | 应用内关闭透明效果后日志记录 `material=Solid`，并保存回退截图 |
| 键盘焦点 | 通过（Windows） | 菜单按钮出现可见焦点轮廓，见焦点截图 |
| 业务逻辑未改动 | 通过 | 与本轮开始时的源码哈希对比，仅6个视觉文件变化：3个样式文件、预览、时间线、检查器 |

控制条当前保持常驻，保证键盘和触控可以直接访问；没有实现仅悬停时自动隐藏。macOS 字体、NSVisualEffectView 和实机窗口边界待拿到 macOS 设备后单独验收。

## 验证命令

- `pnpm lint`：通过
- `pnpm test`：26 项通过
- `pnpm build`：通过
- `pnpm tauri build --no-bundle --debug`：通过（关闭运行中的旧窗口后重建）
