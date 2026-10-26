#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// 说明：Tauri 命令注册通过 `tauri::generate_handler!` 在编译期展开，与 async / sync 无关。
// 这里使用同步 `fn main()` 是为了让 `Builder::run()` 以主线程阻塞的方式托管事件循环——
// 这是 Windows 端最稳的姿势（避免把窗口/事件循环错放到 tokio worker 线程上）。
fn main() {
    let args:Vec<_>=std::env::args_os().collect();
    if args.len()==2 && args[1]=="--timelens-build-info" {
        println!("{}",timelens_lib::internal_build_info());return;
    }
    if args.len()==3 && args[1]=="--timelens-build-info-file" {
        use std::io::Write;
        let mut options=std::fs::OpenOptions::new();options.write(true).create_new(true);
        #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
        let result=options.open(&args[2]).and_then(|mut f|f.write_all(timelens_lib::internal_build_info().to_string().as_bytes()));
        if result.is_err(){std::process::exit(2);}return;
    }
    timelens_lib::run();
}
