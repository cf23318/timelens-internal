# TimeLens Internal 原图归档接收服务（v2）

逐张接收原始 WebP，通过 SHA256、大小、CRC64、私有 ACL 和 SSE 验真后返回精简回执。归档增加任务身份、云端清单、原图回读、数量对账和到期删除。客户端不保存 OSS AccessKey，不上传活动数据库；新任务默认本机累计48小时，保留手动开始和屏幕选择，不加入云端有效覆盖计时/排班/强制多屏。

完整流程、期限、旧数据边界和异常限制见 [归档与留存](../docs/archive-retention.md)。

## 假数据验证

    python -m pip install -e './upload-server[test]'
    python -m pytest -q upload-server/tests

FakeBucket 仅在内存保存合成图片，不访问 OSS 或读取真实凭据。跨语言合同：

    TIMELENS_TEST_PYTHON=/path/to/isolated/venv/bin/python cargo test --manifest-path project/src-tauri/Cargo.toml --features frame-contract-tests --lib

合同临时启动仅监听回环地址的假服务，测试结束停止；普通构建不启用该测试功能。

## 接口与兼容性

v2 客户端和接收器需配套升级，不能只改 URL 使用旧服务。POST /v1/frames/<snapshot UUID> 仍接收单张原始 WebP（默认20MiB上限），回执为 snapshot_id/object_key/sha256/size_bytes/verified。

请求必须有 Bearer 设备token、Survey/Device、Captured-At、Sha256、Size-Bytes、Capture-Group-ID、Group-Elapsed-Ms、Expected-Displays、Display-ID，以及新增的 X-TimeLens-Task（UUID）和 X-TimeLens-Archive-End（含时区）。不将旧v1对象或旧数据库行自动纳入归档删除。

客户端每张图为独立组、时长0、屏数1；这些字段不计工作时长，不强制采全部显示器。服务器对task/survey/device及批准结束上界进行持久绑定，身份不可换，结束只能提前，删除日不能因补传/重试延期。

- POST /v1/archives/<task>/register：登记，不发送图片。
- POST /v1/archives/<task>/finish：小型JSON ended_at；同步提前结束，允许其后补传截止前图片。结束同步不依赖图片队列全部成功。
- GET /v1/archives/<task>：状态和删除截止。
- GET /v1/archives/<task>/manifest?after=<UUID>：分页清单，最多500帧，每帧重新核验OSS HEAD。
- GET /v1/archives/<task>/reconcile：比对持久清单和OSS任务前缀，报告缺失/额外对象数量。
- GET /v1/archives/<task>/frames/<snapshot>：完整回读校验后返回WebP。

所有归档接口要求同设备有效token，逐请求重读，禁止缓存。撤销后不能读取/上传；清理按持久截止继续，不依赖token。错误请求不会返回验收回执。服务器在请求结束后补偿无确认孤儿写入；迟到请求与清理以同一主机OS锁串行。

## 配置（本次未部署）

Linux 单主机，一个Uvicorn worker。独立服务身份、可信HTTPS反向代理，Uvicorn只监听127.0.0.1。spool必须是持久、0700目录，不能使用临时盘。token和批准结束配置为0600、当前服务身份拥有。示例：

    TIMELENS_SPOOL_DIR=/var/lib/timelens-frame
    TIMELENS_DEVICE_TOKENS_FILE=/etc/timelens-frame/device-tokens.json
    TIMELENS_ARCHIVE_ENDS_FILE=/etc/timelens-frame/archive-ends.json
    TIMELENS_OSS_BUCKET=umall-video-record
    TIMELENS_OSS_ENDPOINT=https://oss-cn-beijing-internal.aliyuncs.com

token JSON形状为 survey -> device -> token（至少32字符）；archive-ends JSON为 survey -> device -> RFC3339批准结束。凭据只在服务器安全环境配置 OSS_ACCESS_KEY_ID/OSS_ACCESS_KEY_SECRET。不要放到源码、日志、客户端、安装包或工单。

桶名和北京端点是交付目标，没有在本次核实真实资源。仅北京阿里云同地域资源可用内网端点，其他位置用 https://oss-cn-beijing.aliyuncs.com。upload.timelens.internal 仍是客户端示例，未核实DNS/TLS/部署。

需要私有Bucket、阻止公共访问、无Bucket Policy、版本控制从未开启；每个对象显式private且AES256。身份需要限定任务前缀的写入/读取/列举/删除与安全属性查询权限。版本开启/曾开启时清理拒绝，不能声称删掉历史版本。

接收：

    uvicorn timelens_upload.app:create_app --factory --host 127.0.0.1 --port 8791 --workers 1

必须同时安装并启用 deploy/timelens-retention.service.example 的持续清理服务。两服务共用spool与进程间锁，禁止直接扩多机多副本。代理只开放必要/v1路径，限制21MiB请求体、连接/总时限，不记录Authorization或请求体。healthz只表示协议能力和需要worker，不证明清理任务在运行。

## 归档运维

    timelens-archive manifest --task <UUID> --survey <survey> --device <device>
    timelens-archive reconcile --task <UUID> --survey <survey> --device <device>
    timelens-archive verify --task <UUID> --survey <survey> --device <device>
    timelens-archive restore --task <UUID> --survey <survey> --device <device>
    timelens-archive retention
    timelens-archive retention --execute
    timelens-archive retention-daemon

manifest输出索引，reconcile核对对象数量，verify完整回读复算哈希，restore只写受管managed-restores/<task>。retention默认dry run；--execute才删除。daemon每30秒重试，失败写私有retention-alert.json并持续补偿；运维需接入现有告警和服务监控。状态文件需受控备份，恢复副本不可另做无期限备份。

固定删除日为服务端已知的实际/批准结束较早者+30天；结束元数据在本地持久保存并独立重试，不以图片上传全部成功为条件。若断网、凭证撤销或程序关闭导致服务端尚未收到提前结束，它暂时只能依据批准结束上界；负责人需同步结束记录并必要时提前处置，不能宣称离线仍已同步。到期删除开始时间与完成时间应分开记录，服务/OSS故障、慢请求、外部备份和人工副本须独立核验。

并发默认4、单请求120秒/空闲5秒、暂存预算256MiB、最低空闲256MiB、每设备预算20GiB。容量及负载、TLS/IAM、真实OSS、平台长期运行和实际删除均未在本次验收。

## OSS直传管理工具

不使用接收服务时，运行 `timelens-oss-grant` 签发并登记授权，`timelens-oss-admin` 同步清单、对账、完整回读、恢复和到期清理。步骤、私有台账要求、常驻/授权排空限制和launchd模板见 [离线授权操作](../docs/oss-offline-upload.md)。这套台账与旧接收器台账独立，不自动接管旧任务。

## Cloudflare签发（本地开发，未部署）

新增 `timelens-cloud-issuer` 管理员命令和云签发台账同步。客户端使用设备独立凭据自动续期；管理员须升级并在私有台账配置 `cloud-issuer.json`，云同步失败时禁止删除。完整部署、激活和留存说明见 [Cloudflare签发服务](../docs/cloudflare-issuer.md)。此改动尚未发布安装包或部署到真实Cloudflare，不代表真实OSS已验收。
