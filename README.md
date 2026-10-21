# TimeLens Internal · 内部采集版

基于 [gitxuzhefeng/timelines](https://github.com/gitxuzhefeng/timelines) v0.17.5（f8e7107）改造，保留 [MIT 许可证](LICENSE)。当前内部测试版本为 **0.17.9**。这是用于本地屏幕采样与受控逐张上传的桌面客户端，支持 macOS 和 Windows；内部版的安装包与上游个人版不同。

[English](README.en.md) · [下载安装包](https://github.com/cf23318/timelens-internal/releases) · [构建与测试包](https://github.com/cf23318/timelens-internal/actions/workflows/build.yml) · [安装与配置](docs/internal-frame-install.md) · [接收服务](upload-server/README.md)

## OSS上传界面（0.17.9）

新数据目录默认使用OSS直传，管理员界面仅显示OSS上传。接收服务选项和配置界面已注释停用；旧任务的后端兼容和留存处理保留，已绑定旧目的地的任务不会自动切换。上传仍需单独确认。

新增本机签发网页（仅监听 `127.0.0.1:8877`）和7天公用上传配置。同批次共用一份 JSON，客户端按固定设备 ID 分目录；公用配置不提供设备之间的云端写入隔离。旧单设备授权继续支持。客户端和管理员工具均需更新；生成或导入配置不会启动上传。使用方式见 [离线上传授权说明](docs/oss-offline-upload.md)。

## 当前功能

- 默认中文、浅色和原生窗口边框。新装等待手动开始；可暂停、继续，正常退出和重启后保持暂停。
- 默认每 12 秒尝试截图，可配置 10–15 秒；窗口切换截图保留原分辨率，定时图默认宽 1600、质量 85，WebP 压缩和近似画面去重。它保存离散截图，不是连续录屏视频。
- 默认采集前台窗口所在显示器，可选择全部显示器。新任务默认累计采集 48 小时，可在未开始且未绑定归档前配置总目标，也可提前结束。暂停、退出及长时间采样中断不累计；电脑空闲会停止截图，采集任务计时仍继续。
- 本地保存应用、脱敏窗口标题、时长及可获取的网址域名/路径；记录复制流向估计，不保存按键内容或剪贴板正文。Windows 没有实现输入计数。浏览器地址栏隐藏协议时以 `//域名/路径` 表示协议未知。
- 时间线和时间回放直接读取本地截图，**不依赖 OCR**。OCR 默认关闭，开启时在本机识别文字。外部 AI、在线更新及旧个人版 AI/导出入口已禁用或移除。

## 本地签发7天OSS直传授权

支持软件固定设备ID、Mac离线签发设备专属授权、员工导入后直接上传北京OSS。长期Secret只留在管理员Mac，软件需单独确认才开始上传。详见 [离线授权与OSS直传](docs/oss-offline-upload.md)。

直传的“OSS已接收”只表示POST响应和CRC校验通过，不等同于下述接收服务模式的云端清单对账。新版直传同步分页任务清单，由专用管理员工具执行对账、原图回读、受管恢复和到期删除；管理员常驻进程必须保持在线，所有签发使用同一私有台账。旧接收服务不负责直传清理。本机任务留存规则继续生效。

## 隐私与数据流

截图黑名单按**当前前台应用、标识、标题和网址**判断，不依赖 OCR。默认包含微信、钉钉、常见密码管理器、敏感标题和列出的网银域名；浏览器网址读取失败时跳过截图。

这是基于前台规则的过滤，没有对原图做像素打码。背景中可见的聊天、通知或其他敏感内容仍可能入镜，处理敏感内容时应暂停采集。标题脱敏用于新产生的记录，不会自动重写旧记录。规则覆盖和授权仍需按实际组织环境核实，软件不能代替隐私或法律审查。

- 运行数据位于 `~/.timelens-internal/`，Windows 对应 `%USERPROFILE%\.timelens-internal\`；数据库为 `data/db.sqlite`，截图为 `data/shots/`。
- 默认不上传。接收服务模式需配置国内 HTTPS 接收服务根地址、调研编号、设备编号、批准结束时间、管理员 token，并在界面确认启用本次上传。重启后上传关闭，需重新确认。
- 上传内容为单张原始 WebP 及必要的调研/设备、时间、显示器、大小和哈希元数据；**不上传活动数据库，不再制作 age 加密压缩包**。HTTPS 传输与服务端存储保护仍保留。
- 接收服务模式的原图归档使用 v2 接口：逐图回执、云端清单及对象数量对账。新任务首次确认上传后，实际结束与管理员批准结束取较早者，最多保留 30 天；本机原始/派生记录、OSS 和受管恢复副本按同一截止清理，补传不延期。旧数据目录不会自动接管或删除；没有强制 48 小时、多屏完整组或排班计时。
- `https://upload.timelens.internal` 是配置示例，不能视为已上线的服务器。真实接收服务、凭据及部署由环境负责人提供；源码不含真实运行数据或密钥。

## 长期构建与提交流程

已发布的版本从 [GitHub Releases](https://github.com/cf23318/timelens-internal/releases) 下载，可直接选择 Mac DMG 或 Windows x64 安装器；发布页附源码提交、校验文件及签名状态。安装包不含上传授权，授权文件由管理员单独发放。

1. 在专用分支修改并创建 PR。**PR 自动运行**前端构建、外部调用检查、接收服务假数据测试，以及 Windows/macOS Rust 测试和打包。
2. 从该次成功运行的 **Artifacts** 下载 `TimeLens-windows` 或 `TimeLens-macos`。Windows 包含安装器和独立 exe，macOS 包含 DMG；包内附 `BUILD-INFO.json` 和 `SHA256SUMS`，用于核对版本、源码提交和文件校验值。产物保留 30 天，下载通常需要登录 GitHub。
3. 对涉及桌面行为的改动，用测试内容实机验证启动、窗口唤回、截图、暂停、隐私过滤、无 OCR 回放及重启状态；默认不启用上传。记录未测项。
4. 检查通过并取得合并授权后合并 PR、删除分支。main 更新会重新构建；Actions 长期保持启用，也可在页面手动运行 Build。
5. 普通构建不会发布 Release。明确授权发布后才打 `v*` 标签；Release 工作流复用同一构建流程并生成待审核的草稿。

测试包未使用组织代码签名/公证时，只适合受控测试。更新可能需要重新核对系统权限；按系统正常提示处理，不关闭系统安全保护。

## 本地开发

需要 Node.js 22 或更新版本、Rust stable，以及 [Tauri 平台依赖](https://v2.tauri.app/start/prerequisites/)；Windows 构建还需要 MSVC C++ 工具链和 WebView2。

```sh
git clone https://github.com/cf23318/timelens-internal.git
cd timelens-internal/project
npm ci
npm run tauri dev
```

```sh
npm run build
npm run verify:no-net
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib
npm run tauri -- build
```

已有依赖的离线构建可使用 `project/scripts/build-internal-macos.sh` 或 `build-internal-windows.ps1`。接收服务假数据测试与跨语言合同测试见 [接收服务说明](upload-server/README.md)。

## 验证边界

2026-10-20 已在 Windows 10 Education 22H2（远程桌面、单屏）完成 0.17.5 安装和约 14 分钟实测：31 张截图、暂停、无 OCR 回放及重启保持暂停通过，同时发现窗口唤回、标题脱敏和界面收尾问题。0.17.6 修复后的结果见 [当前验证记录](docs/validation.md)。

尚不能宣称完成 Windows 11、真实双屏/混合 DPI、长时间锁屏/睡眠、完整48小时、员工试点、真实性能指标或真实服务器上传验收。CI、短时试录、生产部署和合规结论分别验证。

本地归档专项改造及假数据验证见 [归档与留存](docs/archive-retention.md)。可用显式绝对路径环境变量 TIMELENS_PROFILE_DIR 创建独立调研 profile；Finder 启动不会继承终端临时变量。请管理员配置受控启动方式，不要搬移或清空旧数据来绕过归属检查。v2 需配套新版接收器与持续清理服务，不能只改 URL 接入旧版服务。
