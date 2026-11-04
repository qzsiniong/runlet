//! 系统托盘:Tauri 2 TrayIconBuilder + Menu。
//! - 菜单项:状态(不可点) + 脚本编辑器 + 开机时启动(勾选) + 退出
//! - 忙时更新状态行;托盘图标动态绘制进度环(单色 template,自动适配明暗主题)

use crate::config::Config;
use image::{ImageBuffer, Rgba, RgbaImage};
use tauri::{
    image::Image,
    menu::{
        CheckMenuItem, CheckMenuItemBuilder, IconMenuItem, IconMenuItemBuilder, IsMenuItem, Menu,
        PredefinedMenuItem,
    },
    tray::TrayIconBuilder,
    App, AppHandle, Manager, Wry,
};

pub const M_STATUS: &str = "status";
pub const M_SCRIPTS: &str = "scripts";
pub const M_AUTOSTART: &str = "autostart";
pub const M_QUIT: &str = "quit";

/// 托盘图标的 id（全应用唯一）。
pub const TRAY_ID: &str = "main";

/// 构建托盘（首次）：建图标与菜单，并把菜单项句柄存进 State 供忙闲切换使用。
pub fn build(app: &App, _cfg: &Config) -> tauri::Result<()> {
    let items = TrayItems::build(app)?;

    // `Menu::with_items` 要 `&[&dyn IsMenuItem]`：数组里的元素类型必须一致，
    // 所以先建分隔符再与句柄一起装进 `Vec<&dyn IsMenuItem>`
    let separators = [
        PredefinedMenuItem::separator(app)?,
        PredefinedMenuItem::separator(app)?,
    ];
    let all: Vec<&dyn IsMenuItem<Wry>> = vec![
        &items.status,
        &separators[0],
        &items.scripts,
        &items.autostart,
        &separators[1],
        &items.quit,
    ];
    let menu = Menu::with_items(app, &all)?;

    let icon = app
        .default_window_icon()
        .cloned()
        .unwrap_or_else(|| Image::new(&[], 1, 1));

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("Runlet 脚本")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .build(app)?;

    // macOS 27+: 显式要求显示菜单项图像(系统默认全部隐藏)
    #[cfg(target_os = "macos")]
    force_menu_icons_visible_impl(&app.handle().clone());

    app.manage(items);

    Ok(())
}

/// 托盘菜单项句柄(存入 Tauri State 供 set_busy/切换勾选用)。
pub struct TrayItems {
    pub status: IconMenuItem<Wry>,
    pub scripts: IconMenuItem<Wry>,
    pub autostart: CheckMenuItem<Wry>,
    pub quit: IconMenuItem<Wry>,
}

impl TrayItems {
    /// 建出全部固定菜单项。
    fn build(app: &App) -> tauri::Result<Self> {
        use tauri_plugin_autostart::ManagerExt;

        let status = IconMenuItemBuilder::with_id(M_STATUS, "状态:空闲")
            .enabled(false)
            .build(app)?;
        let scripts = IconMenuItemBuilder::with_id(M_SCRIPTS, "脚本编辑器…")
            .icon(icon_scripts())
            .build(app)?;
        // 勾选状态以系统登录项为准：用户可能在系统设置里直接关掉它
        let autostart_enabled = app.autolaunch().is_enabled().unwrap_or(false);
        let autostart = CheckMenuItemBuilder::with_id(M_AUTOSTART, "开机时启动")
            .checked(autostart_enabled)
            .build(app)?;
        let quit = IconMenuItemBuilder::with_id(M_QUIT, "退出 Runlet")
            .icon(icon_quit())
            .build(app)?;

        Ok(Self {
            status,
            scripts,
            autostart,
            quit,
        })
    }
}

/// 切换「开机时启动」：以**系统登录项的实际状态**为准取反，再把菜单勾选同步过去。
///
/// 为什么不读菜单的 `is_checked()`：点击是否自动切换勾选在不同平台上不一致
/// （macOS 的 NSMenuItem 会自己 toggle，Windows/Linux 不会），依赖它就会有一半平台反着来。
/// 系统登录项是唯一真源 —— 用户可能在系统设置里直接改过，读它还能顺便纠正陈旧勾选。
pub fn toggle_autostart(app: &AppHandle) {
    use tauri_plugin_autostart::ManagerExt;

    let manager = app.autolaunch();
    let wanted = !manager.is_enabled().unwrap_or(false);

    let result = if wanted {
        manager.enable()
    } else {
        manager.disable()
    };

    let items = app.state::<TrayItems>();
    match result {
        Ok(()) => {
            let _ = items.autostart.set_checked(wanted);
            crate::notify::notify(
                "Runlet",
                if wanted {
                    "已设置为开机时启动"
                } else {
                    "已取消开机时启动"
                },
            );
        }
        Err(e) => {
            // 写登录项失败：界面必须回到真实状态，否则用户会以为已经设置成功
            let _ = items
                .autostart
                .set_checked(manager.is_enabled().unwrap_or(!wanted));
            log::warn!("设置开机启动失败(wanted={wanted}): {e}");
            crate::notify::notify("Runlet", &format!("设置开机启动失败：{e}"));
        }
    }
}

/// 忙闲状态切换:更新状态文本。
pub fn set_busy(app: &AppHandle, busy: bool, status: &str) -> tauri::Result<()> {
    let items = app.state::<TrayItems>();
    items.status.set_text(status)?;
    // 恢复默认图标(空闲)
    if !busy {
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            if let Some(icon) = app.default_window_icon() {
                let _ = tray.set_icon(Some(icon.clone()));
            }
        }
    }
    Ok(())
}

/// 更新状态行进度文本(供 worker send_progress 调用)。
pub fn set_status_text(app: &AppHandle, text: &str) -> tauri::Result<()> {
    let items = app.state::<TrayItems>();
    items.status.set_text(text)
}

/// 更新托盘图标为带进度环的单色图标(macOS template,自动适配明暗主题)。
pub fn set_tray_progress(app: &AppHandle, percent: u8) -> tauri::Result<()> {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Ok(());
    };
    let img = draw_progress_icon(percent);
    let w = img.width();
    let h = img.height();
    let rgba = img.into_raw();
    let tauri_img = Image::new_owned(rgba, w, h);
    tray.set_icon(Some(tauri_img))?;
    // 设为 template image,macOS 自动适配明暗主题
    tray.set_icon_as_template(true)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 菜单图标:32x32 RGBA,程序化绘制,彩色方案在 macOS 明暗菜单下均可见
// ---------------------------------------------------------------------------

const MENU_ICON: u32 = 32;

/// 在 32x32 透明画布上执行绘制并转成 Tauri Image。
fn menu_icon(draw: impl Fn(&mut RgbaImage)) -> Image<'static> {
    let mut img: RgbaImage = ImageBuffer::from_pixel(MENU_ICON, MENU_ICON, Rgba([0, 0, 0, 0]));
    draw(&mut img);
    Image::new_owned(img.into_raw(), MENU_ICON, MENU_ICON)
}

/// 越界安全的单像素写入。
fn ico_put(img: &mut RgbaImage, x: i32, y: i32, c: Rgba<u8>) {
    if (0..MENU_ICON as i32).contains(&x) && (0..MENU_ICON as i32).contains(&y) {
        img.put_pixel(x as u32, y as u32, c);
    }
}

/// 闭区间填充矩形。
fn ico_rect(img: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, c: Rgba<u8>) {
    for y in y0..=y1 {
        for x in x0..=x1 {
            ico_put(img, x, y, c);
        }
    }
}

/// 闭区间实心圆角矩形(r 为角半径)。
fn ico_round(img: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, r: i32, c: Rgba<u8>) {
    let r2 = r * r;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let cx = x.clamp(x0 + r, x1 - r);
            let cy = y.clamp(y0 + r, y1 - r);
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy <= r2 {
                ico_put(img, x, y, c);
            }
        }
    }
}

/// 粗细为 thickness 的线段(像素中心到线段距离 <= thickness/2)。
fn ico_line(img: &mut RgbaImage, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32, c: Rgba<u8>) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len2 = dx * dx + dy * dy;
    let half = thickness / 2.0;
    let pad = half.ceil() as i32 + 1;
    let minx = x0.min(x1) as i32 - pad;
    let maxx = x0.max(x1) as i32 + pad;
    let miny = y0.min(y1) as i32 - pad;
    let maxy = y0.max(y1) as i32 + pad;
    for y in miny..=maxy {
        for x in minx..=maxx {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let t = if len2 > 0.0 {
                (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let qx = x0 + t * dx;
            let qy = y0 + t * dy;
            let ddx = px - qx;
            let ddy = py - qy;
            if ddx * ddx + ddy * ddy <= half * half {
                ico_put(img, x, y, c);
            }
        }
    }
}

/// 运行脚本:琥珀色代码文档(外框 + 左侧竖线 + 两行代码条)。
fn icon_scripts() -> Image<'static> {
    menu_icon(|img| {
        let c = Rgba([217, 119, 6, 255]); // amber-600
        let clear = Rgba([0, 0, 0, 0]);
        ico_round(img, 5, 4, 27, 28, 3, c);
        ico_round(img, 7, 6, 25, 26, 2, clear);
        // 代码左侧的「行号」竖线
        ico_rect(img, 9, 9, 10, 23, c);
        // 两行代码条
        ico_rect(img, 13, 11, 23, 13, c);
        ico_rect(img, 13, 16, 21, 18, c);
        ico_rect(img, 13, 21, 23, 23, c);
    })
}

/// 退出:红色 X。
fn icon_quit() -> Image<'static> {
    menu_icon(|img| {
        let c = Rgba([220, 38, 38, 255]); // red-600
        ico_line(img, 8.0, 8.0, 24.0, 24.0, 4.0, c);
        ico_line(img, 24.0, 8.0, 8.0, 24.0, 4.0, c);
    })
}

/// 绘制 128x128 单色进度环 PNG(透明背景 + 环)。
/// 环内已完成部分 alpha=255,未完成部分 alpha=60(半透明背景环)。
fn draw_progress_icon(percent: u8) -> RgbaImage {
    let size = 128;
    let cx = 64.0;
    let cy = 64.0;
    let outer = 56.0;
    let inner = 44.0;
    let percent = percent.min(100) as f64 / 100.0;
    // 进度弧:从顶部(-90°)顺时针扫过
    let start_angle = -std::f64::consts::FRAC_PI_2;

    let mut img: RgbaImage = ImageBuffer::from_pixel(size, size, Rgba([0, 0, 0, 0]));

    for y in 0..size {
        for x in 0..size {
            let dx = x as f64 - cx;
            let dy = y as f64 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist < inner || dist > outer {
                continue;
            }
            // 计算像素角度(atan2,从顶部顺时针)
            let angle = dy.atan2(dx);
            // 归一化到 [-PI, PI],判断是否在进度弧内
            let in_progress = if percent >= 1.0 {
                true
            } else {
                // 把 angle 转换到 [start_angle, start_angle + TAU) 区间
                let mut a = angle - start_angle;
                while a < 0.0 {
                    a += std::f64::consts::TAU;
                }
                while a >= std::f64::consts::TAU {
                    a -= std::f64::consts::TAU;
                }
                a <= percent * std::f64::consts::TAU
            };
            let alpha = if in_progress { 255u8 } else { 60u8 };
            img.put_pixel(x, y, Rgba([0, 0, 0, alpha]));
        }
    }
    img
}

// ---------------------------------------------------------------------------
// macOS 27 兼容:NSMenu 默认隐藏菜单项图像,需逐项设置 preferredImageVisibility
// ---------------------------------------------------------------------------

/// 泛型实现：`App` 与 `AppHandle` 都实现了 `Manager`，因此只写一份。
#[cfg(target_os = "macos")]
fn force_menu_icons_visible_impl<R: tauri::Runtime>(manager: &AppHandle<R>) {
    use objc2::MainThreadMarker;

    /// NSMenuItemImageVisibilityVisible(macOS 27+,旧系统忽略该消息)
    const IMAGE_VISIBILITY_VISIBLE: isize = 1;

    let Some(tray) = manager.tray_by_id(TRAY_ID) else {
        return;
    };
    let _ = tray.with_inner_tray_icon(|inner| {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(status_item) = inner.ns_status_item() else {
            return;
        };
        let Some(menu) = status_item.menu(mtm) else {
            return;
        };
        unsafe {
            let n = menu.numberOfItems();
            for i in 0..n {
                if let Some(item) = menu.itemAtIndex(i) {
                    let _: () = objc2::msg_send![
                        &*item,
                        setPreferredImageVisibility: IMAGE_VISIBILITY_VISIBLE
                    ];
                }
            }
        }
    });
}
