//! Windows：`Windows.Media.Ocr`（离线，依赖系统已安装 OCR 语言）。

#![cfg(target_os = "windows")]

use std::path::Path;
use std::time::{Duration, Instant};
use windows::Foundation::{AsyncStatus, IAsyncOperation};

use windows::core::HSTRING;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::FileAccessMode;
use windows::Storage::StorageFile;
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

/// WinRT 未提供词级置信度：每词给中性分，便于统一闸门（行级仍生效）。
const NEUTRAL_WORD_CONF: f32 = 82.0;

const OCR_TIMEOUT: Duration = Duration::from_secs(15);

fn wait_async<T: windows::core::RuntimeType + 'static>(
    operation: IAsyncOperation<T>,
    deadline: Instant,
) -> Result<T, String> {
    loop {
        let status = operation
            .Status()
            .map_err(|e| format!("OCR 状态读取失败: {e}"))?;
        if status != AsyncStatus::Started {
            return operation
                .GetResults()
                .map_err(|e| format!("OCR 系统调用失败: {e}"));
        }
        if Instant::now() >= deadline {
            let _ = operation.Cancel();
            let _ = operation.Close();
            return Err("OCR 超时（15s），已取消本次识别".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn recognize_png_path(path: &Path) -> Result<Vec<Vec<(String, f32)>>, String> {
    unsafe {
        RoInitialize(RO_INIT_MULTITHREADED).map_err(|e| format!("OCR 初始化失败: {e}"))?;
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { RoUninitialize() }
        }
    }
    let _apartment = Apartment;
    let deadline = Instant::now() + OCR_TIMEOUT;
    let path_s = path.to_str().ok_or("截图路径非 UTF-8")?;
    let file = wait_async(
        StorageFile::GetFileFromPathAsync(&HSTRING::from(path_s))
            .map_err(|e| format!("GetFileFromPathAsync: {e}"))?,
        deadline,
    )?;
    let stream = wait_async(
        file.OpenAsync(FileAccessMode::Read)
            .map_err(|e| format!("OpenAsync: {e}"))?,
        deadline,
    )?;
    let decoder = wait_async(
        BitmapDecoder::CreateAsync(&stream)
            .map_err(|e| format!("BitmapDecoder::CreateAsync: {e}"))?,
        deadline,
    )?;
    let bitmap = wait_async(
        decoder
            .GetSoftwareBitmapAsync()
            .map_err(|e| format!("GetSoftwareBitmapAsync: {e}"))?,
        deadline,
    )?;

    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|e| format!("OcrEngine::TryCreateFromUserProfileLanguages: {e}"))?;

    let result = wait_async(
        engine
            .RecognizeAsync(&bitmap)
            .map_err(|e| format!("RecognizeAsync: {e}"))?,
        deadline,
    )?;

    let lines = result
        .Lines()
        .map_err(|e| format!("OcrResult.Lines: {e}"))?;
    let n = lines.Size().map_err(|e| format!("OcrLines.Size: {e}"))? as usize;
    let mut out: Vec<Vec<(String, f32)>> = Vec::with_capacity(n);
    for i in 0..n {
        let line = lines
            .GetAt(i as u32)
            .map_err(|e| format!("OcrLines.GetAt: {e}"))?;
        let words = line.Words().map_err(|e| format!("OcrLine.Words: {e}"))?;
        let wn = words.Size().map_err(|e| format!("OcrWords.Size: {e}"))? as usize;
        let mut row: Vec<(String, f32)> = Vec::new();
        for j in 0..wn {
            let w = words
                .GetAt(j as u32)
                .map_err(|e| format!("OcrWords.GetAt: {e}"))?;
            let t = w
                .Text()
                .map_err(|e| format!("OcrWord.Text: {e}"))?
                .to_string();
            let t = t.trim().to_string();
            if !t.is_empty() {
                row.push((t, NEUTRAL_WORD_CONF));
            }
        }
        if row.is_empty() {
            let lt = line
                .Text()
                .map_err(|e| format!("OcrLine.Text: {e}"))?
                .to_string();
            let lt = lt.trim().to_string();
            if !lt.is_empty() {
                row.push((lt, NEUTRAL_WORD_CONF));
            }
        }
        if !row.is_empty() {
            out.push(row);
        }
    }
    Ok(out)
}
