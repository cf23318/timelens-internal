# Mac离线签发与7天OSS直传

这是一条新增的上传路径：管理员Mac离线签发 → 员工导入 → 员工软件直接POST到北京OSS。签发不访问OSS，无需创建STS/RAM角色或云函数。使用的管理员密钥必须已有目标目录的`oss:PutObject`权限；本地假OSS测试不能证明真实密钥权限。

## 7天公用配置与本地网页

本地新增格式 `timelens-oss-shared-post-v1`。一份公用文件可由同批次所有设备导入；设备自动使用自己的固定 ID，写入 `timelens/direct/v1/users/<batchId>/devices/<fixedId>/`。原设备专属格式继续支持。公用文件需要本次新版客户端，0.17.7及更早版本会拒绝导入；本地改造不代表 Windows 新包已构建或发布。

公用配置**仅做目录分组，不提供设备间的云端写入隔离或员工身份认证**。持有者能冒用其他设备 ID 写新对象；私有 ACL、禁止覆盖、无 GET/LIST/DELETE、WebP/清单 MIME、20MiB 和7天限制保持。批准结束仍是独立参数；续签同一批次必须保持其原值。已绑定任务不能在专属与公用授权之间切换，旧混合数据不会自动纳入上传。

管理员可用 `timelens-oss-grant --shared --user-id '<唯一批次编号>' ...` 签发。也可在已安装管理工具的私有环境运行：

```sh
python -m timelens_upload.grant_web --bucket your-private-bucket --credentials-file /private/path/credentials.json --registry /private/admin-registry
```

打开 `http://127.0.0.1:8877/`，生成后下载同一个 JSON 分发。网页仅监听本机，检查 Host、Origin 和 CSRF；不接收浏览器提供的密钥或任意文件路径，不访问 OSS、不自动导入或开启上传。授权 JSON 是可用凭证，不能公开发布。

公用授权先登记到同一私有台账的 `shared-groups.json`。管理端发现批次下的设备及无清单任务，以“批次+设备”登记，兼容原 UUID 台账。续签限制整个批次最早已知的删除期限；删除时读取批次最晚授权到期并等待120秒排空。批准结束加30天且授权排空后，重复扫除批次内的未知设备及畸形孤立对象。管理端必须同时更新并保持常驻，不能只升级客户端。

## 员工取得固定ID

1. 打开“设置 → 管理员配置”，上传方式选“OSS直传 · 7天上传授权”，保存修改。
2. 复制“本机固定设备ID”交给管理员。管理员另行维护用户编号与员工的对应关系。
3. 固定ID保存在当前用户profile的`device-identity.json`，重启、更新授权不改变它。更换、删除或克隆profile会改变或复制身份；它不是硬件认证或真实员工身份认证。
4. 已绑定原接收服务的任务不能切换模式；旧的混合数据目录不会自动纳入上传。应为新调研使用独立profile，不能删除旧记录来绕过归属检查。

## 管理员在Mac生成授权

需要Python 3.10或以上。先创建虚拟环境并安装管理工具：`python3 -m venv .venv`，然后 `.venv/bin/python -m pip install ./upload-server`。激活虚拟环境后，双击或在终端运行仓库中的`tools/create-oss-upload-grant.command`，按提示填写固定ID、用户编号、北京Bucket和管理员凭据。凭据隐藏输入，不作为命令行参数传入。

也可以在终端运行下例，省略凭据文件参数时仍会隐藏输入凭据：

```sh
PYTHONPATH=upload-server .venv/bin/python -m timelens_upload.oss_grant \
  --device-id '11111111-1111-4111-8111-111111111111' \
  --user-id 'employee-001' \
  --bucket 'your-private-bucket' \
  --study-end '2026-10-27T17:00:00+08:00' \
  --registry '/Users/your-user/.timelens-admin' \
  --output '/private/tmp/employee-001.timelens-upload.json'
```

固定ID示例必须替换为员工提供的值。批准调研结束是独立的留存参数，不能和授权有效期混为一谈。未传`--study-end`时默认为首次签发后7天；签发后终端会显示两个截止时间。

批量使用可通过`--credentials-file`指定源码目录外的私有JSON，键仅为`accessKeyId`和`accessKeySecret`。文件须归当前用户、0600权限且不是符号链接。不要将管理员凭据、授权文件、真实截图或运行profile提交到Git。输出授权文件采用0600、禁止覆盖既有文件；授权文件不含长期Secret，但仍是可被持有者使用的上传凭证。

## 员工导入和启用

在管理员配置中选择授权JSON，或粘贴完整内容后导入。软件校验本机ID、用户目录、北京OSS域名、签发/截止、最大7天、私有ACL、AES256、禁止覆盖、WebP类型及20MiB文件上限。导入保存配置，不自动启动采集或上传；目的地及授权摘要不代表OSS已在线验证签名。

确认用户、设备、目录及留存安排后，另点“确认并启用本次后台上传”，或“结束并上传”。重启后上传默认关闭。授权和签名不进入前端overview、日志、截图元数据或回执；完整授权仅保存在当前profile的0600文件`oss-upload-grant.json`中（Windows遵循当前用户profile访问权限）。

对象目录为`timelens/direct/v1/users/<userId>/devices/<fixedId>/tasks/<taskId>/<UTC年月日>/<snapshotId>.webp`。每份签名限制到该用户和设备前缀；仅分发的POST授权不能用于GET/LIST/DELETE。复制授权和profile仍可冒用相同身份，应按凭证保管。

## 过期、续签和失败

- 有效期从签发时间计算，最多7个日历日。到期停止上传，待传原图按原任务留存规则保存，不把授权到期当作数据删除或任务结束。
- 续签须使用原设备ID、用户编号、Bucket及**原批准调研结束时间**，用`--study-end`保留旧值，换一个输出文件名。可以重新签7天，但不能延后已经绑定任务的留存截止。
- 暂停采集、停止后台上传并等待在途请求结束后，再导入新授权并重新确认上传。已收到有效回执的图片不重复发送。
- 网络/5xx等临时错误退避重试；拒绝、CRC不符、重定向或409需人工核验。上传已落盘但回执丢失时，重试可能409；只有上传权限无法确认既有对象内容，因此**409不当成功**，本地保留，管理员另行核验。
- 这不是可逐用户实时吊销的在线登录系统。泄露授权的提前撤销需要管理员处理底层密钥或云端策略，可能影响同密钥签发的其他授权。

## 回执与留存边界

直传单独显示“OSS已接收”：TLS POST返回204、OSS请求编号存在且CRC64与本地文件相符。接收记录独立保存，不把它写成原接收服务的`verified`，不把任务标为“云端清单已对账”。原图不会因上传成功立即删除。目的地/模式在首次确认后绑定，防止把任务改发到其他目录。

本机已绑定任务仍按实际结束与批准结束的较早者加30天到期清理，授权续签不延期。新版授权增加独立的 JSON 清单 POST 策略；截图策略仍只接收 WebP。清单包含整个本机上传队列的预期截图（包括未传、被阻断、回执丢失），分页不可覆盖，所有页回执确认后最后提交索引。每60秒、任务结束时同步；静态页复用，不重复发送全部历史清单。重启后不会自动发送原图，已绑定任务的结束清单会单独重试。

旧授权没有清单策略时仍可传图片，但管理员不能将其认定为完成对账；需在暂停、停止上传后重新导入新授权。员工只有 POST 权限，管理员工具持有 GET/LIST/DELETE 权限。目录权限按用户和设备隔离，**同一设备的不同任务不是独立的云端权限边界**。

## 管理员闭环

签发工具在导出授权前，将用户、设备、Bucket、批准结束、已签发授权最晚到期写入 `--registry` 私有台账；不保存 Secret、policy 或签名。续签必须使用同一台账，禁止将授权续签到已知任务的删除截止之后。台账需备份，不得另起一个空台账绕过已签授权记录。不要用其他签发脚本签发未登记的生产授权。

```sh
# 私有凭据 JSON 仅含 accessKeyId/accessKeySecret，0600，放源码外；不要发到聊天或提交 Git。
.venv/bin/timelens-oss-admin sync --bucket your-private-bucket --credentials-file /private/path/credentials.json
.venv/bin/timelens-oss-admin list
.venv/bin/timelens-oss-admin reconcile --bucket your-private-bucket --credentials-file /private/path/credentials.json --task TASK_UUID
.venv/bin/timelens-oss-admin verify --bucket your-private-bucket --credentials-file /private/path/credentials.json --task TASK_UUID
.venv/bin/timelens-oss-admin restore --bucket your-private-bucket --credentials-file /private/path/credentials.json --task TASK_UUID
# 预览到期任务，执行时显式加 --execute
.venv/bin/timelens-oss-admin retention --bucket your-private-bucket --credentials-file /private/path/credentials.json
.venv/bin/timelens-oss-admin watch --execute --bucket your-private-bucket --credentials-file /private/path/credentials.json
```

所有命令支持 `--registry`，须与签发保持一致。对账比较预期与实际对象；`verify` 完整下载到内存核对 SHA256、CRC64、大小、用户/设备/任务/采集时间、私有ACL和AES256。`restore` 仅写入台账下受管的0700目录、0600原图；到期一并删除。中途清单返回 `finalInventory=false`，只能证明当前批次；只有最终清单且缺失/多余为0、完整回读通过才能作最终数据验收。空任务也必须有合法索引，不能将缺失索引误认成0张成功。

清理按任务分别进行，错误不会阻断其他任务。未提交索引的部分图片仍纳入批准结束加30天的兜底清理；设备整个批准期限结束后清理该设备精确前缀下的异常孤立对象。删除前验证 Bucket 私有、未启用过版本控制；逐对象删除并 HEAD/列举确认，保留最小任务墓碑，后续轮次继续扫描迟到对象。台账和恢复目录不允许符号链接。

**常驻要求和期限边界：** 管理工具必须在保持联网、运行的 Mac/Linux 管理机上持续执行 `watch --execute`（每60秒）；机器关机/睡眠不能准时清理，需报警补扫或使用常开服务器。旧接收服务不负责这些直传任务。离线签名不能被本机锁立即吊销；若实际提前结束让删除截止早于已签授权到期，清理会报警等待授权失效及120秒在途排空，而不会假报删除成功。所有签发必须进同一台账，时钟需同步。即使客户端始终离线未上报真实结束，管理端也只能按已批准结束兜底，不能声称云端已获知更早的实际结束。严格实时30天删除需在线授权/服务端策略另行约束。

## 本地假数据验证

Python测试验证V4 HMAC、目录/设备/用户条件、文件上限、7天边界、续签及凭据文件权限。Rust合同测试通过回环TLS假OSS实际发送合成WebP，校验POST表单、CRC、错误签名、重复409、响应丢失、错误CRC、不跟随重定向及本地队列计数；不接触真实OSS或用户profile。

```sh
python -m pytest -q upload-server/tests
TIMELENS_TEST_PYTHON=/path/to/test-venv/bin/python \
  cargo test --locked --manifest-path project/src-tauri/Cargo.toml --features frame-contract-tests --lib
```

真实环境合成图验收与安装包结果以本次独立验收报告为准。未配置 Apple Developer ID／公证及 Windows 代码签名时，只生成内部测试包；不能将未签名内部包描述为正式签名发布。员工端网络和48小时连续运行需另行实机验收。
