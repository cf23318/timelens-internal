# 上传归档、回读、对账与到期删除（本地改造）

新任务默认累计48小时，保留手动开始/暂停、显示器选择、截图节奏/去重和隐私规则。没有移植服务端 48 小时覆盖计时、固定全屏、排班或 120 秒断网停采。

累计计时是本机运行中的任务时长，暂停、退出和睡眠不计；空闲时可能无截图，但计时仍继续。因此本机48小时不等于V4的“OSS已校验完整多屏采样组覆盖48小时”。

配置改用 targetHours（默认48，范围1–48）。旧 workdayHours 字段可读取但不再按“两工作日”计算；未开始且未归档任务采用新目标。已有累计、已结束、待落盘或已归档任务继续使用持久化的 targetMs，不因升级延长；配置显示同步原任务目标。批准结束和删除截止均不随目标变化自动延后。

## 配套协议

客户端和接收器必须一起升级到 raw-frames-v2。旧服务不能只改 URL 兼容。客户端仍只连接明确确认的 HTTPS 根地址；不接触 OSS AccessKey、不上传活动数据库，不制作加密压缩包。

管理员提供 survey/device、设备 token、archiveEndAt（RFC3339，含时区）。接收器 TIMELENS_ARCHIVE_ENDS_FILE 的 0600 JSON 必须登记相同的 UTC 时刻：

    {"study-example": {"device-example": "2026-11-10T17:00:00+11:00"}}

示例日期不是运行配置；管理员须使用批准的时间。它提供归档拍摄时间/留存上界，不改变客户端累计目标（新任务默认48小时）。超出批准结束的帧被服务拒收，不会悄悄延长时间；需负责人核对，不自动重设调研。

首次确认上传才绑定归档与本地删除策略，保存配置不会上传或自动删除。需全新、归属明确的 profile：程序在空数据目录、采集前写入任务归属证明。已有记录却没有证明时，拒绝绑定，不自动迁移或删除。旧截图/旧 age 回执和服务端 v1 对象须另行盘点。可使用 TIMELENS_PROFILE_DIR 指定独立绝对路径；测试仅使用临时目录，不更改现有 .timelens-internal。

## 流程

1. 上传前持久化本地任务/目的地/结束上界，服务端登记不可更换的同一身份和上界。
2. 图片写盘和 outbox 仍为持久事务/恢复日志。发送时带 X-TimeLens-Task 与 X-TimeLens-Archive-End；对象前缀为 timelens/v2/surveys/<survey>/devices/<device>/tasks/<task>/...。
3. 逐图验证 SHA256、大小、CRC64、私有 ACL 和 SSE 后返回原有五字段精简回执。UUID/内容/任务不可更改；暂存和未确认写入留在任务所有的目录/登记内。
4. 本地任务结束后独立同步结束元数据，不等待图片队列成功；队列全部确认后再用 OSS 对象列表和分页 manifest 对账。只有全部本地截图和云端索引一致才标记 uploaded。它表示清单已对账，不表示所有原图已经下载校验，也不表示本地已删除。
5. 管理员在接收服务上运行 verify 或 restore，完整回读并复算 SHA256/CRC64/大小。恢复只写固定受管目录，生成原图和清单；不接受任意输出路径。复制到分析机、备份或其他目录的副本须另外登记同一删除截止及责任人。

## 运行命令（本次未部署或运行真实 OSS 命令）

安装后读取同一份私有服务器环境。token 从受保护配置读取，不能放命令行：

    timelens-archive manifest --task <task-uuid> --survey <survey> --device <device>
    timelens-archive reconcile --task <task-uuid> --survey <survey> --device <device>
    timelens-archive verify --task <task-uuid> --survey <survey> --device <device>
    timelens-archive restore --task <task-uuid> --survey <survey> --device <device>
    timelens-archive retention
    timelens-archive retention --execute
    timelens-archive retention-daemon

retention 默认仅列出到期任务；--execute 才会删除。持续清理需部署 deploy/timelens-retention.service.example，与接收服务同机、同一持久 spool、同一身份，每 30 秒重试。健康接口只表示功能支持及需要独立 worker，不能证明 worker 已部署/运行。需要限定任务前缀的列举、读取、删除及安全属性查询权限。版本控制开启或曾开启时拒绝清理，防止历史版本遗留。

## 截止与异常

以实际任务结束和批准结束中较早者加 30 天作为最晚删除截止，持久化后只能缩短，补传、重试和重启均不能延后。结束元数据独立于图片队列持久重试；重启仍关闭原图上传，但已明确绑定的归档可继续同步最小结束元数据。断网、凭证撤销或程序关闭时，服务端在收到提前结束前只能按批准结束上界清理，负责人需同步记录或提前处置，不能宣称离线已同步。服务端即使 token 撤销仍按持久登记清理；不可将截止状态文件随凭证撤销一起删除。

到期后拒绝上传、清单和回读。本地启动及运行中定期检查，停止采集并等待截图、上传、OCR 生产者结束，删除受管图片/恢复日志/exports，清理数据库原始及派生行，并执行 secure_delete、VACUUM、WAL 截断；保留配置和最小任务截止审计。完整性错误本身不取消已批准的到期删除；停止生产者或取锁失败必须保留告警并重试，不虚报成功。

服务端删除 OSS 任务前缀、受管恢复目录、崩溃暂存和帧索引；未确认孤儿写入在无活跃请求时补偿。跨截止慢请求和中断可能延迟物理删除；worker 保留私有告警并持续重试。接收与清理服务必须同机共用锁和 spool，不能直接扩成多机多副本。

本机关闭、服务/OSS 不可用、系统时钟异常、系统快照/备份、人工复制副本及 SSD 残留不能靠应用保证瞬间物理删除。下次启动/服务恢复后补偿；时钟应由运维同步。备份和人工副本需另行核验，不能宣称应用清理覆盖所有副本或精确到截止秒。

## 验证范围

仅合成 WebP、内存 FakeBucket、回环服务和临时 profile。没有读取真实云密钥、上传真实截图、连接真实 OSS、部署或提交 GitHub。真实云资源、签名安装包、Windows 新版运行、多屏和长期清理仍需独立验收。
