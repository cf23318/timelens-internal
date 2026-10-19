use image::DynamicImage;

/// Stable display label plus native-resolution image. Never fall back to an unrelated display.
pub(super) fn grab(
    plan: &CapturePlan,
    current: &impl Fn() -> bool,
) -> Result<Vec<(String, DynamicImage)>, String> {
    #[cfg(target_os = "macos")]
    {
        if core_graphics::display::CGDisplay::active_displays().map_err(|_| "显示器列表不可用")?
            != plan.native_ids
        {
            return Err("显示器拓扑变化".into());
        }
        let indices = plan.indices.clone();
        let mut images = Vec::new();
        for index in indices {
            if !current() {
                return Err("采集已暂停".into());
            }
            let scratch = tempfile::Builder::new()
                .prefix("timelens-capture-")
                .tempdir()
                .map_err(|_| "无法创建截图临时目录")?;
            let tmp = scratch.path().join("capture.png");
            let mut child = std::process::Command::new("/usr/sbin/screencapture")
                .args(["-x", "-t", "png", "-D", &index.to_string()])
                .arg(&tmp)
                .spawn()
                .map_err(|_| "无法执行截图命令")?;
            let began = std::time::Instant::now();
            let status = loop {
                if !current() || began.elapsed() > std::time::Duration::from_secs(5) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("截图已取消或超时".into());
                }
                match child.try_wait() {
                    Ok(Some(status)) => break status,
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err("系统截图失败".into());
                    }
                }
            };
            if !status.success() {
                return Err("系统截图失败".into());
            }
            let img = image::open(&tmp).map_err(|_| "无法读取系统截图")?;
            images.push((format!("display-{index}"), img));
        }
        Ok(images)
    }
    #[cfg(target_os = "windows")]
    {
        let screens = screenshots::Screen::all().map_err(|_| "无法枚举显示器")?;
        let mut images = Vec::new();
        for screen in screens {
            if !current() {
                return Err("采集已暂停".into());
            }
            let info = screen.display_info;
            let actual = Region::new(
                &info.id.to_string(),
                info.x as f64,
                info.y as f64,
                info.width as f64,
                info.height as f64,
            )?;
            if !plan.regions.iter().any(|r| r == &actual) {
                continue;
            }
            let captured = screen.capture().map_err(|_| "系统截图失败")?;
            let (w, h) = (captured.width(), captured.height());
            let rgba =
                image::RgbaImage::from_raw(w, h, captured.into_raw()).ok_or("截图数据无效")?;
            images.push((
                format!("display-{}", info.id),
                DynamicImage::ImageRgba8(rgba),
            ));
        }
        if images.is_empty() {
            return Err("前台显示器不可用".into());
        }
        Ok(images)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (plan, current);
        Err("此平台不支持屏幕采集".into())
    }
}

/// Desktop point coordinates, never screenshot pixels (important with mixed DPI).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Region {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
impl Region {
    pub fn new(id: &str, x: f64, y: f64, width: f64, height: f64) -> Result<Self, String> {
        if ![x, y, width, height].iter().all(|v| v.is_finite()) || width <= 0. || height <= 0. {
            return Err("屏幕或窗口坐标无效".into());
        }
        Ok(Self {
            id: id.into(),
            x,
            y,
            width,
            height,
        })
    }
    pub fn intersects(&self, other: &Self) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct CapturePlan {
    pub regions: Vec<Region>,
    #[cfg(target_os = "macos")]
    native_ids: Vec<u32>,
    #[cfg(target_os = "macos")]
    indices: Vec<u32>,
}
pub(super) fn plan(all: bool) -> Result<CapturePlan, String> {
    #[cfg(target_os = "macos")]
    {
        use core_graphics::display::CGDisplay;
        let ids = CGDisplay::active_displays().map_err(|_| "无法枚举显示器")?;
        let indices = crate::core::acquisition::capture_display_indices(all)?;
        if CGDisplay::active_displays().map_err(|_| "无法枚举显示器")? != ids {
            return Err("显示器拓扑变化".into());
        }
        let regions = indices
            .iter()
            .map(|index| {
                let id = *ids.get(*index as usize - 1).ok_or("显示器列表变化")?;
                let b = CGDisplay::new(id).bounds();
                Region::new(
                    &format!("{index}:{id}"),
                    b.origin.x,
                    b.origin.y,
                    b.size.width,
                    b.size.height,
                )
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(CapturePlan {
            regions,
            native_ids: ids,
            indices,
        })
    }
    #[cfg(target_os = "windows")]
    {
        let point = if all {
            None
        } else {
            Some(crate::core::acquisition::foreground_window_center().ok_or("前台屏幕不可用")?)
        };
        let mut regions = Vec::new();
        for s in screenshots::Screen::all().map_err(|_| "无法枚举显示器")? {
            let d = s.display_info;
            if let Some((x, y)) = point {
                if x < d.x || y < d.y || x >= d.x + d.width as i32 || y >= d.y + d.height as i32 {
                    continue;
                }
            }
            regions.push(Region::new(
                &d.id.to_string(),
                d.x as f64,
                d.y as f64,
                d.width as f64,
                d.height as f64,
            )?);
        }
        if regions.is_empty() {
            return Err("可采集屏幕为空".into());
        }
        Ok(CapturePlan { regions })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = all;
        Err("此平台不支持窗口检查".into())
    }
}
