use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, WebviewWindow, async_runtime, window::Color};

#[derive(Default)]
pub struct AppearanceState {
    update_lock: async_runtime::Mutex<()>,
    last_applied: Mutex<Option<AppliedAppearance>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppearanceTheme {
    Light,
    Dark,
}

impl AppearanceTheme {
    fn native(self) -> tauri::Theme {
        match self {
            Self::Light => tauri::Theme::Light,
            Self::Dark => tauri::Theme::Dark,
        }
    }

    fn solid_color(self) -> Color {
        match self {
            Self::Light => Color(245, 245, 247, 255),
            Self::Dark => Color(28, 28, 30, 255),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowMaterial {
    Native,
    Solid,
}

#[derive(Debug, Serialize)]
pub struct WindowAppearance {
    material: WindowMaterial,
    message: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct AppliedAppearance {
    theme: AppearanceTheme,
    native: bool,
}

/// The page begins opaque, so no uninitialized desktop pixels flash through.
/// Subsequent appearance requests explicitly opt in to native transparency.
pub fn initialize(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("main") {
        #[cfg(target_os = "windows")]
        window.set_decorations(false)?;
        let theme = match window.theme()? {
            tauri::Theme::Dark => AppearanceTheme::Dark,
            _ => AppearanceTheme::Light,
        };
        window.set_background_color(Some(theme.solid_color()))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn apply_window_appearance(
    app: AppHandle,
    state: tauri::State<'_, AppearanceState>,
    theme: AppearanceTheme,
    transparency: bool,
) -> Result<WindowAppearance, String> {
    // Serializing across the main-thread acknowledgement also prevents two
    // preference updates from interleaving clear/apply/background operations.
    let _guard = state.update_lock.lock().await;
    let window = app.get_webview_window("main").ok_or("主窗口尚未准备好")?;
    let target = window.clone();
    let (sender, mut receiver) = async_runtime::channel(1);
    window
        .run_on_main_thread(move || {
            let result = apply_on_main_thread(&target, theme, transparency);
            if let Err(error) = &result {
                log::warn!(target: "kaicut::appearance", "{error}");
            }
            let _ = sender.try_send(result);
        })
        .map_err(|error| format!("无法调度原生外观更新：{error}"))?;
    receiver
        .recv()
        .await
        .ok_or_else(|| "原生外观更新没有完成".to_string())?
}

fn apply_on_main_thread(
    window: &WebviewWindow,
    theme: AppearanceTheme,
    transparency: bool,
) -> Result<WindowAppearance, String> {
    // Re-read the OS preference before looking in the cache, so focus regain
    // can detect changes made while KaiCut was inactive without repainting an
    // unchanged window or recreating its macOS NSVisualEffectView.
    let native_permission = if transparency {
        native_effects_allowed()
    } else {
        Err("已按应用偏好关闭透明材质".into())
    };
    let requested = AppliedAppearance {
        theme,
        native: native_permission.is_ok(),
    };
    let state = window.state::<AppearanceState>();
    let mut last_applied = state
        .last_applied
        .lock()
        .map_err(|_| "原生外观状态不可用，请重新启动应用")?;
    if last_applied.as_ref() == Some(&requested) {
        return Ok(match native_permission {
            Ok(()) => WindowAppearance {
                material: WindowMaterial::Native,
                message: native_material_name().into(),
            },
            Err(message) => WindowAppearance {
                material: WindowMaterial::Solid,
                message,
            },
        });
    }

    // Clear the previous key before any native mutation. A failed attempt
    // must not masquerade as success or suppress retry on the next focus.
    *last_applied = None;
    let appearance = update_window(window, theme, native_permission)?;
    if (appearance.material == WindowMaterial::Native) == requested.native {
        *last_applied = Some(requested);
    }
    log::info!(
        target: "kaicut::appearance",
        "theme={theme:?}, transparency={transparency}, material={:?}: {}",
        appearance.material, appearance.message
    );
    Ok(appearance)
}

fn update_window(
    window: &WebviewWindow,
    theme: AppearanceTheme,
    native_permission: Result<(), String>,
) -> Result<WindowAppearance, String> {
    window
        .set_theme(Some(theme.native()))
        .map_err(|error| format!("无法同步原生窗口主题：{error}"))?;

    if let Err(reason) = native_permission {
        return use_solid(window, theme, reason);
    }

    // WebView2 supports alpha 0 or 255, not a partially transparent default
    // background. CSS supplies the readable tint over this native material.
    if let Err(error) = window.set_background_color(Some(Color(0, 0, 0, 0))) {
        return use_solid(
            window,
            theme,
            format!("无法启用透明 WebView，已使用实色：{error}"),
        );
    }
    match apply_native(window, theme) {
        Ok(()) => Ok(WindowAppearance {
            material: WindowMaterial::Native,
            message: native_material_name().into(),
        }),
        Err(error) => use_solid(
            window,
            theme,
            format!("原生材质不可用，已使用实色：{error}"),
        ),
    }
}

fn use_solid(
    window: &WebviewWindow,
    theme: AppearanceTheme,
    message: String,
) -> Result<WindowAppearance, String> {
    // Restore opacity first: even a failure to clear an OS backdrop cannot
    // leak through a solid WebView and the frontend's solid surfaces.
    window
        .set_background_color(Some(theme.solid_color()))
        .map_err(|error| format!("无法恢复实色窗口背景：{error}"))?;
    if let Err(error) = clear_native(window) {
        log::warn!(
            target: "kaicut::appearance",
            "Native backdrop clear returned an error; the solid background is active: {error}"
        );
    }
    Ok(WindowAppearance {
        material: WindowMaterial::Solid,
        message,
    })
}

#[cfg(target_os = "windows")]
fn native_effects_allowed() -> Result<(), String> {
    use windows::UI::ViewManagement::{AccessibilitySettings, UISettings};
    // window-vibrancy's Windows 10 SWCA branch ignores the native return value.
    // Use only the DWM branch whose HRESULT can be checked and read back.
    if windows_version::OsVersion::current().build < 22621 {
        return Err("当前 Windows 版本没有可验证的 Acrylic 接口，已使用实色".into());
    }
    let high_contrast = AccessibilitySettings::new()
        .and_then(|settings| settings.HighContrast())
        .map_err(|error| format!("无法读取系统高对比度设置，已使用实色：{error}"))?;
    if high_contrast {
        return Err("系统高对比度模式已启用，使用实色背景".into());
    }
    let effects_enabled = UISettings::new()
        .and_then(|settings| settings.AdvancedEffectsEnabled())
        .map_err(|error| format!("无法读取系统透明效果设置，已使用实色：{error}"))?;
    if !effects_enabled {
        return Err("系统透明效果已关闭，使用实色背景".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn native_effects_allowed() -> Result<(), String> {
    use objc2::{
        class, msg_send,
        runtime::{AnyObject, Bool},
    };
    // These NSWorkspace selectors are public AppKit APIs. This function only
    // runs inside run_on_main_thread; it reads the OS preference without
    // changing it or touching application/user defaults.
    let reduce_transparency = unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return Err("无法读取 macOS 辅助功能设置，已使用实色".into());
        }
        let reduced: Bool = msg_send![workspace, accessibilityDisplayShouldReduceTransparency];
        reduced.as_bool()
    };
    if reduce_transparency {
        Err("macOS 已启用减少透明度，使用实色背景".into())
    } else {
        Ok(())
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn native_effects_allowed() -> Result<(), String> {
    Err("当前平台暂不支持原生透明材质，使用实色背景".into())
}

#[cfg(target_os = "windows")]
fn apply_native(window: &WebviewWindow, _: AppearanceTheme) -> Result<(), String> {
    use windows::Win32::Graphics::Dwm::{
        DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DwmGetWindowAttribute,
    };
    // DWM's system-backdrop branch ignores custom tint; the system and CSS
    // surface tokens provide the material color instead.
    window_vibrancy::apply_acrylic(window, None).map_err(|error| error.to_string())?;
    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    let mut applied: i32 = 0;
    // The HWND is borrowed from the live Tauri window on the main thread;
    // DWM writes exactly one i32 into the supplied output buffer.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&mut applied as *mut i32).cast(),
            std::mem::size_of::<i32>() as u32,
        )
    }
    .map_err(|error| format!("无法确认 Acrylic 状态：{error}"))?;
    if applied != DWMSBT_TRANSIENTWINDOW.0 {
        return Err("系统没有接受 Acrylic 背景类型".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn apply_native(window: &WebviewWindow, _: AppearanceTheme) -> Result<(), String> {
    use window_vibrancy::{NSVisualEffectMaterial, NSVisualEffectState};
    window_vibrancy::clear_vibrancy(window).map_err(|error| error.to_string())?;
    window_vibrancy::apply_vibrancy(
        window,
        NSVisualEffectMaterial::Sidebar,
        Some(NSVisualEffectState::FollowsWindowActiveState),
        None,
    )
    .map_err(|error| error.to_string())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn apply_native(_: &WebviewWindow, _: AppearanceTheme) -> Result<(), String> {
    Err("当前平台不支持原生透明材质".into())
}

#[cfg(target_os = "windows")]
fn clear_native(window: &WebviewWindow) -> Result<(), String> {
    window_vibrancy::clear_acrylic(window).map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
fn clear_native(window: &WebviewWindow) -> Result<(), String> {
    window_vibrancy::clear_vibrancy(window)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn clear_native(_: &WebviewWindow) -> Result<(), String> {
    Ok(())
}

fn native_material_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "Windows Acrylic 已由 DWM 接受"
    } else if cfg!(target_os = "macos") {
        "macOS Sidebar 原生材质已应用，随窗口激活状态变化"
    } else {
        "原生材质已应用"
    }
}
