use image::DynamicImage;

/// Stable display label plus native-resolution image. Never fall back to an unrelated display.
pub fn grab(all: bool, current: &impl Fn() -> bool) -> Result<Vec<(String, DynamicImage)>, String> {
    #[cfg(target_os = "macos")]
    {
        let indices = crate::core::acquisition::capture_display_indices(all)?;
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
        let point = if all {
            None
        } else {
            Some(
                crate::core::acquisition::foreground_window_center()
                    .ok_or("无法确定前台窗口屏幕")?,
            )
        };
        let mut images = Vec::new();
        for screen in screens {
            if !current() {
                return Err("采集已暂停".into());
            }
            let info = screen.display_info;
            if let Some((x, y)) = point {
                if x < info.x
                    || y < info.y
                    || x >= info.x + info.width as i32
                    || y >= info.y + info.height as i32
                {
                    continue;
                }
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
        let _ = (all, current);
        Err("此平台不支持屏幕采集".into())
    }
}
