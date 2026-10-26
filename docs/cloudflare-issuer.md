# Cloudflare 自动签发与客户端续期（本地开发，未部署）

## 员工专用包：授权、姓名、开始

员工专用包在构建时预置批次激活配置。员工首次打开只需完成系统权限、在首页填写姓名、点击“开始采集”。姓名在开始时自动保存，后台自动注册设备、续期和同步，不再导入JSON或进入上传设置。首页和顶栏使用同一开始逻辑；托盘等原生入口也强制检查权限、姓名和预置配置。开始与暂停仍由员工控制，不因填姓名或重启就偷偷恢复采集。

首次离线也可开始。软件先保存固定设备的私有随机凭据、预置目的地和批准结束，再在后台联网激活。即使离线直至任务结束，联网后仍可在核对owner、任务、目的地和期限完全相同后安装首份授权并补传；无需员工重新暂停、导入或配置。更新安装包保留原姓名、设备凭据、任务归属及“停止同步”选择，不用新包里的另一批次悄悄换绑旧任务。

**管理员只在打包前操作一次：** 先生成该批次激活JSON，保存在源码外的0600私有文件，设置 `TIMELENS_ACTIVATION_CONFIG` 为它的路径，再运行现有Mac或Windows构建脚本。程序直接携带预置内容，员工电脑不需要这份源文件。仅允许激活格式的7个字段，拒绝重复字段、长期OSS Secret、无效地址和已结束批次；不将预置内容传给前端、日志或构建清单。

```sh
# 仅管理员本机执行；文件路径可见，密钥内容不放命令行。
export TIMELENS_ACTIVATION_CONFIG=/private/path/pilot.activation.json
./project/scripts/build-internal-macos.sh
```

Windows同样设置环境变量后执行 `project/scripts/build-internal-windows.ps1`。release构建缺少预置时直接失败；公开CI显式使用 `TIMELENS_ALLOW_UNCONFIGURED_BUILD=1`，生成不含批次激活凭据的通用诊断包。它不是员工开箱即用包。调试开发可不提供预置，以保留管理员诊断入口。

**员工专用包包含可提取的批次激活凭据，应内部发放。** 放进程序不能使凭据不可提取；不应把含真实激活凭据的包当成无凭据通用包发布到公开Release。公开源代码与通用CI包不含真实预置。批次名额和激活期限仍由云端限制，OSS长期Secret始终仅留签发服务。

构建记录读取实际编译程序的安全 `--timelens-build-info-file` 输出，对比输入文件SHA256，并核对Mac `.app`/DMG或Windows NSIS内的主程序。版本相同但预置不同的旧安装器会被拒绝。Windows验包要求7-Zip；本机没有相应工具就失败，不跳过。检查进程使用临时隔离profile，不启动采集。公开记录只含模式及指纹，不含激活码。

打包工具默认检查release产物；本地合成验收可使用 `TIMELENS_BUILD_PROFILE=debug`，以及指向独立假数据目录的 `TIMELENS_BUNDLE_DIR`、`TIMELENS_DISTRIBUTION_DIR`，避免覆盖已有Release下载产物。取消预置会显式清空Cargo输出，更新同路径配置也会重新编译。

数据流：TimeLens 向 Cloudflare 请求上传授权；截图和 JSON 清单由 TimeLens 直接上传北京 OSS。Cloudflare 不接收截图、活动数据库或姓名，只处理随机设备 ID、批次、设备认证和授权期限。姓名仍随截图保存在 OSS 对象路径中。

## 客户端行为

- 姓名必填。采集仍由用户手动开始，保留暂停和隐私黑名单。
- 上传默认开启，无需确认对话框；配置就绪且任务归属、落盘、完整性和留存校验通过后，启动后台上传。停止上传会持久保存关闭状态，重启不覆盖用户选择。到期、完整性异常、旧混合数据、旧包和旧回执均不被自动接管。
- 员工专用包自动读取预置配置，为本机随机固定ID绑定独立凭据；通用诊断包仍支持手动导入。设备凭据先写入0600文件，再请求激活。成功响应丢失可用同一凭据重试，不重复占用名额。手动导入前需暂停采集并停止上传；自动激活不要求员工做这两步。
- 启动后和运行期间检查授权，到期前 24 小时续签，最长 7 天。每 5 秒协调后台状态，每分钟检查是否需要续期；休眠唤醒后按实际时间重新判断。网络失败按退避重试，最长 15 分钟。旧授权仍有效时继续使用，过期后保留队列等待续期。
- 正在上传时可原子更新同范围授权，无需暂停。用户、格式、设备、Bucket、前缀和批准结束保持不变；图片策略和清单策略一起更新。服务器拒绝（403）最多尝试一次强制续期，只有授权内容确实更新才重发，仍失败则保留错误。409、CRC 或回执异常不会被自动重置为成功或无限重试。
- 结束采集不依赖网络和授权是否有效。结束后仍按自动上传开关补传。上传成功不立即删除原图；到期规则保持原实际/批准结束较早者加 30 天。

已配置目的地和批准结束的任务，即使授权过期或截图为零，结束时也封存本地留存期限。未完成目的地/批准结束配置的离线试录尚未绑定归档，不能视为已进入云端留存闭环；正式使用前应完成首次激活。

## Cloudflare 服务

源码在 `cloudflare-issuer/`，使用 Worker + SQLite-backed Durable Object。所有请求路由至同一持久对象，状态临界区显式串行；请求体在锁外限长、限时读取。先持久记录签发到期时间，再返回可用签名。响应 `Cache-Control: no-store`，默认关闭观测日志/遥测，不记录请求正文、认证头或签名。

部署需要设置：

| 名称 | 存放位置 | 用途 |
|---|---|---|
| `OSS_BUCKET` | Worker 变量 | 固定北京私有 Bucket |
| `PUBLIC_ISSUER_URL` | Worker 变量 | 员工访问的 HTTPS 根地址，含末尾 `/` |
| `OSS_ACCESS_KEY_ID` / `OSS_ACCESS_KEY_SECRET` | Worker Secrets | 建议专用 RAM 身份，仅允许既定上传根目录 `oss:PutObject`；不授予客户端读、删、列举权限 |
| `ISSUER_ADMIN_TOKEN` | Worker Secret，64 位小写随机十六进制 | 创建批次、停用设备；仅管理员持有 |
| `ISSUER_SYNC_TOKEN` | 另一份 Worker Secret，同样格式 | 管理员同步台账和缩短截止，不能创建批次或激活设备 |

域名、账号 ID、真实密钥均不写进源码。`wrangler.jsonc` 尚未绑定任何真实账号。部署命令须单独在获授权后执行：`npm ci`、设置 Variables/Secrets、`wrangler deploy`。当前没有发布到 Cloudflare 或 GitHub。国内员工网络与域名可达性尚需真实验收。

接口：

| POST 路径 | 认证 | 作用 |
|---|---|---|
| `/admin/batches` | 管理 token | 建立不可变批次、激活截止和设备名额，返回可多人首次导入的激活配置 |
| `/v1/activate` | 激活码 + 本机随机凭据 | 首次注册或幂等重试；设备 ID 本身不是密码 |
| `/v1/renew` | 设备独立 Bearer 凭据 | 为已绑定设备续签，服务端决定 Bucket/范围，不接受客户端指定任意前缀 |
| `/admin/device` | 管理 token | 设置设备停用状态；停用阻止领取新授权，已经签发的授权仍可用到其到期 |
| `/admin/sync` | 管理或同步 token | 原子缩短删除上限并读取已签发最大到期时间 |

默认建议每设备专属授权；仍支持旧公用格式以便同范围迁移。公用格式仍是批次写权限，不隔离不同设备的写入，也不认证员工真实身份。首次激活码可设独立有效期与人数上限；客户端保存设备独立凭据后无需再分发 7 天 JSON。全服务上限为 200 个批次、2000 台设备，单批次最多 1000 台，当前实现适合内部调研，扩容前需专项调整。

## 管理员首次建立批次

安装新版 `upload-server` 管理工具；在源码外准备 0600 文件，仅含 `issuerUrl` 和 `adminToken`，不在命令行或聊天输入 token。执行：

```sh
timelens-cloud-issuer --credentials-file /private/path/issuer-admin.private.json \
  --batch-id pilot-001 --study-end '<带时区的批准结束时间>' \
  --activation-end '<首次激活截止，不晚于批准结束>' --max-devices 100 \
  --output /private/path/pilot.activation.json
```

工具先保存0600的待完成请求，创建成功响应丢失时，用完全相同参数和输出路径重试即可恢复，不另建批次或延长截止。员工专用包由管理员将该文件预置进程序，员工无需拿到或导入文件；通用诊断包仍可单独导入。激活文件不能公开放进GitHub/Release。旧任务绑定了公用授权时须保持同批次、公用范围、原Bucket和原批准结束，不能直接换设备专属格式。

## 与到期清理配套上线

管理员台账目录需新增 0600 的 `cloud-issuer.json`，内容仅为 `issuerUrl` 和 `syncToken`。现有 `timelens-oss-admin watch --execute ...` 启动前必须升级管理工具并准备此配置。

同步流程：导入云端设备与最大签发到期 → 发现 OSS 任务与清单 → 再向同一个 Durable Object 原子提交最早任务删除截止并读取最大已签发到期 → 等待该到期及 120 秒在途排空后删除。后续签发上限为截止减 120 秒，不能延长截止。公用授权按整个批次的最早任务约束；共享目录内未激活/旧设备的任务也参与约束和清理。

云台账无法同步、身份冲突、台账缺失或管理配置被移除时，云管理范围禁止删除并报警重试。管理机仍须在线；把签发迁到 Cloudflare 不会自动把原图回读、对账或删除执行迁到 Cloudflare。签发台账应保留，不得清空 Durable Object 或另起空台账绕过截止。恢复备份与迁移签发服务需另行审计。

## 本地验证与发布边界

Worker 单元测试比较 Python 签名器的结果，覆盖重复激活、批次创建响应丢失、权限、设备停用、最早截止、并发约束和规模边界。`npm run check` 仅本地打包，不部署。`test/local-runtime.mjs` 显式测试本机 `wrangler dev --local` 的 SQLite 台账与重启，使用固定假密钥，不访问 OSS。Rust 使用回环 TLS 假服务验证激活失败/恢复、过期续期、录制中替换、拒绝范围变化与隐私文件权限；Python 使用假 Bucket 验证同步、回读和到期删除。

发布时客户端、管理员工具、Cloudflare 服务与私有同步配置应配套启用。现有安装包/Release 尚未更新。本地测试不代表 Cloudflare 生产部署、Windows 原生运行或真实 OSS 已验收。

## GitHub 员工包构建与私有发放

普通PR、push和workflow_call仍只构建无激活凭据的通用包。管理员可在受信任提交上手动触发现有Build工作流，勾选employee_build；独立员工job使用受限Environment `timelens-employee-build`，只允许管理员核准的分支。Secrets为 `TIMELENS_ACTIVATION_CONFIG_JSON` 和独立随机32字节十六进制 `TIMELENS_BUILD_ENCRYPTION_KEY`。OSS长期Secret不进入GitHub。

员工job不恢复或保存依赖/Rust缓存；先跑合成测试，再把预置写入RUNNER_TEMP私有文件。激活码单独遮罩，只给构建传文件路径。实际app/安装器核对通过后，分发目录打ZIP并用随机nonce的AES-256-GCM保护；公开Artifacts仅上传 `.tle` 密文及其SHA256，保留7天。失败不回退上传明文，结束清理私有文件、明文ZIP和编译输出。

管理员本机通过私有环境注入同一包密钥，再执行 `node project/scripts/protect-employee-build.mjs open package.tle package.zip`，认证成功才写出ZIP；核对BUILD-INFO和SHA256SUMS后安装或内部发放。密钥不通过命令行参数传递。员工拿到普通安装器，不需要解密、配置JSON或处理上传。持有仓库工作流写权限的人属于可信构建边界；运行前必须审阅固定提交，不自动合并或发布含真实激活凭据的公开Release。
