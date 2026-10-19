# TimeLens Internal 逐张截图接收服务

从交付源码中适配 OSS 私有存储校验、设备 token 和幂等回执；接收原始 WebP，经 HTTPS 传输。客户端不再制作 age 加密包；OSS 仍启用服务端 AES256 存储加密。

此版本不依赖排班、服务器累计 48 小时或 30 天到期删除。采集目标、屏幕选择和手动开始仍由客户端现有规则控制。服务只接收图片及必要的调研/设备、时间、显示器和校验元数据，活动数据库保留在本地。上传成功后客户端也保留本地图片，不自动清理。

## 本地假数据测试

在独立虚拟环境安装 `.[test]` 后执行：

```sh
python -m pip install '.[test]'
python -m pytest -q
```

测试的 FakeBucket 仅存内存，不访问真实 OSS 或凭据。测试图片由 Pillow 或 Rust 编码器合成，不截图。

跨语言合同测试从项目根目录执行（将 Python 路径替换为该虚拟环境）：

```sh
TIMELENS_TEST_PYTHON=/absolute/path/to/venv/bin/python cargo test --manifest-path project/src-tauri/Cargo.toml --features frame-contract-tests --lib
```

其中 `rust_to_python_receiver_to_fake_oss_roundtrip` 自动启动仅监听 127.0.0.1 的假服务，测试完成停止。普通应用构建不启用该测试功能。

## 接口

`POST /v1/frames/{规范小写截图 UUID}`，请求体为单个原始 WebP，最大默认 20 MiB。必须携带：

- `Authorization: Bearer <设备 token>`
- `X-TimeLens-Survey`、`X-TimeLens-Device`
- `X-TimeLens-Captured-At`（含时区的 RFC3339）
- `X-TimeLens-Sha256`、`X-TimeLens-Size-Bytes`
- `X-TimeLens-Capture-Group-ID`、`X-TimeLens-Group-Elapsed-Ms`
- `X-TimeLens-Expected-Displays`、`X-TimeLens-Display-ID`

当前客户端将每张去重后保存的图片作为独立组，组时长为 0、组屏数为 1；这些字段只作元数据，不累计工作时长，也不要求每次采集全部显示器。

成功回执仅包含 `snapshot_id/object_key/sha256/size_bytes/verified`。客户端逐项匹配才持久标记 verified；不接受额外的 48 小时调查状态字段。同 UUID 的身份、内容和日期不可更改；同组同屏不可换 UUID。相同请求重试会重新核验 OSS，对象不覆盖。接收中或 PUT 后鉴权撤销不会返回成功回执。

客户端严格使用 HTTPS 根地址，禁止嵌入账号、路径、查询、重定向和系统代理。HTTP 仅用于内部测试函数的回环假服务，生产配置不接受 HTTP。408/429/5xx退避重试，身份、数据冲突及非法图片拒绝后需核对再重试。token 不出现在图片、日志、回执或界面。

## 部署配置（本次未部署）

仅适用于 Linux 单机受控部署、一个 Uvicorn worker。使用独立服务账户及可信 HTTPS 反向代理，Uvicorn只监听 127.0.0.1。`TIMELENS_SPOOL_DIR` 必须显式指定持久目录；接收清单与 SQLite 不能放临时盘。数据库、token 和状态须按需要备份，但本服务没有自动留存删除策略，负责人须另行定义数据保留和处置。

```sh
TIMELENS_SPOOL_DIR=/var/lib/timelens-frame
TIMELENS_DEVICE_TOKENS_FILE=/etc/timelens-frame/device-tokens.json
TIMELENS_OSS_BUCKET=umall-video-record
TIMELENS_OSS_ENDPOINT=https://oss-cn-beijing-internal.aliyuncs.com
```

上述桶名 `umall-video-record`、北京地域和内网端点沿用交付包的指定目标，保留在 [部署模板](deploy/server.env.example) 中；尚未核实真实资源、权限或连通性。北京地域阿里云主机可使用内网端点，其他网络位置应使用 `https://oss-cn-beijing.aliyuncs.com`。

[客户端配置示例](deploy/client-config.json.example) 保留交付包的 `https://upload.timelens.internal` 地址，作为管理员配置参考；该地址在原包中也位于 example 文件，未验证其 DNS/TLS 或实际服务。调研/设备编号仍留空，国内目的地确认仍为 false。示例不会自动载入，不能覆盖现有用户配置。有效上传 token 和 OSS AccessKey 不在交付包中，须由环境负责人另行配置。

本接收服务已去掉原版的 48 小时记账和 30 天自动删除，并使用精简回执。若该地址当前运行对方原版服务，需先按约定部署兼容版本；只复制 URL 不能保证兼容。此次没有对线上服务做任何修改。

设备 token 文件格式为 `{"survey-id":{"device-id":"<独立随机token，至少32字符>"}}`，0600且属于服务账户。每次接收和回执前重读，可轮换/撤销。文件错误时拒绝接收。不要将实际文件放进源码。OSS 凭据仅在服务器环境提供 `OSS_ACCESS_KEY_ID/OSS_ACCESS_KEY_SECRET`，客户端不保存 AccessKey。

仅支持北京地域指定公网/内网 OSS HTTPS endpoint。Bucket ACL需private、阻止公共访问开启、没有Bucket Policy、版本控制从未启用；每次验真检查对象 ACL、AES256、大小、SHA256元数据及CRC64，并在PUT后复核Bucket。专用RAM身份仅授所需前缀读写及安全属性查询权限。服务没有自动删除权限需求或到期清理守护进程。

启动：`uvicorn timelens_upload.app:create_app --factory --host 127.0.0.1 --port 8791 --workers 1`。反向代理配置可信TLS、21 MiB请求上限、上传时限，隐藏Authorization并不记录请求体。健康接口`/healthz`不读取图片。

并发默认4、单请求120秒/空闲5秒、待接收预算256MiB、磁盘最低剩余256MiB、每设备登记预算20GiB；修改Settings时须压测。请求进入前原子预留容量，finally释放；崩溃遗留暂存仍占预算，不会自动删除。可核对SQLite与OSS后由管理员离线处置。当前锁和回执数据库为单机文件，不支持直接多副本扩容。

真实OSS、RAM权限、TLS、生产负载与Windows/Mac长期运行仍需独立验收；本地假存储测试不能替代。
