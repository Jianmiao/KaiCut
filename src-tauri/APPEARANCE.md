# Native appearance contract

Call `invoke('apply_window_appearance', { theme: 'light' | 'dark', transparency: boolean })`.

The command returns `{ material: 'native' | 'solid', message: string }`. It updates the main window's native titlebar theme and the actual native backdrop. The frontend must keep the workspace root opaque until a successful result reports `material: 'native'`; a solid result requires opaque application surfaces. Preview, timeline, and inspection content stay opaque even when native material is available.

Requests are applied serially on the native main thread. The frontend should ignore stale responses or serialize its own preference calls. `transparency: false` ensures native effects are cleared and a solid theme-colored window/WebView background is active. Missing windows or main-thread dispatch failures reject the command; native material errors return an explicit solid result.

The last successfully applied theme and effective system-approved transparency state are cached. A focus refresh still checks system preferences, but an unchanged configuration returns its current result without reassigning the background, titlebar theme, or native effect. Failed native attempts are not cached as successful native appearance and can retry on the next request. Fallback reasons are recomputed, so the returned message stays current even when the solid surface does not need repainting.

Windows uses Acrylic rather than Mica, because the requested material samples content behind the window. macOS uses a native NSVisualEffectView material that follows active/inactive window state. Linux and other unsupported targets use a solid surface. System reduced-transparency settings override the application preference where the platform provides a readable setting.

The window remains natively decorated. Native transparency requires a transparent window and a transparent WebView background; CSS alone cannot reveal the desktop behind the window.

## Source diagnosis and implementation choices

- The original Tauri window was opaque (`transparent` absent, default false). A CSS `backdrop-filter` cannot sample desktop pixels through an opaque native window/WebView.
- Installed Tauri 2.12.1 `Window::set_effects` dispatches to the main thread but discards the underlying effect result; its platform adapter also discards `window-vibrancy` errors. KaiCut instead calls the already locked `window-vibrancy` 0.8.1 directly on the native main thread and handles its result.
- KaiCut's native Acrylic path requires Windows 11 build 22621 or newer, matching Microsoft's documented support for `DWM_SYSTEMBACKDROP_TYPE`. The library uses `DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TRANSIENTWINDOW)` for Acrylic, and KaiCut reads the DWM attribute back before returning `native`. This branch ignores custom tint, so no misleading tint argument is supplied; the system material and CSS tokens provide the color. The library's older Windows SWCA fallback ignores the native return value, so KaiCut deliberately returns `solid` on older systems instead of reporting unverified material support.
- Windows `UISettings.AdvancedEffectsEnabled` and `AccessibilitySettings.HighContrast` are read on every transparency-enabled request on supported systems. Disabled effects, high contrast, or an unreadable preference cause a solid fallback. No registry or system settings are modified.
- macOS uses `NSVisualEffectMaterial::Sidebar`, with `BehindWindow` blending supplied by the library and `FollowsWindowActiveState`. It reads the public `NSWorkspace.accessibilityDisplayShouldReduceTransparency` preference on the native main thread. Existing effect views are cleared before a replacement is added.
- Linux uses the shared solid theme surface.
- The window is created transparent with background alpha 0, but startup applies a solid background until the frontend requests material. Each change synchronizes the native titlebar theme and updates window/WebView background alpha. Preview, timeline, inspector, and other content surfaces must remain opaque in CSS.

## macOS tradeoff and validation boundary

The installed `tauri-utils` 2.10.1 explicitly documents `macOSPrivateApi` as a no-op in Tauri 2.12.1 and later: the relevant APIs are now always enabled. No obsolete feature or configuration switch is added.

This does **not** mean the transparency path is free of private APIs. The installed Wry 0.57.0 uses the private WKWebView `drawsBackground` KVC key for transparent background handling. This must be reviewed for Mac App Store distribution; do not claim App Store compatibility from a successful local build. Directly distributed, signed/notarized macOS packaging still requires its own platform validation.

The command re-reads OS preferences when invoked. The frontend should invoke it after app appearance changes, OS preference events available in the WebView, and window focus regain so changes made while KaiCut is inactive are applied. No continuous polling or system-wide settings writes are introduced.

Runtime appearance must be visually checked per platform. `native` means the platform API accepted the material (and the DWM type was read back on Windows), not that translucency was visually verified or guaranteed under every compositor/battery/remote-session policy. macOS cannot be runtime-tested on the current Windows host.

Source references: installed `tauri-2.12.1/src/window/mod.rs`, `tauri-2.12.1/src/vibrancy/{windows,macos}.rs`, `window-vibrancy-0.8.1/src/{windows.rs,macos/vibrancy.rs}`, `tauri-utils-2.10.1/src/config.rs`, `wry-0.57.0/src/wkwebview/mod.rs`, and [Microsoft's DWM backdrop type documentation](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwm_systembackdrop_type).
