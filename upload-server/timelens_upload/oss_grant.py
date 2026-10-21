"""Offline administrator tool. Signing never contacts OSS or exports the Secret."""
import argparse
import base64
import getpass
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import stat
from datetime import datetime, timedelta, timezone
from uuid import UUID

FORMAT = "timelens-oss-post-v1"
SHARED_FORMAT = "timelens-oss-shared-post-v1"
MAX_BYTES = 20 * 1024 * 1024
DYNAMIC = ("sha256", "task", "captured-at", "display")


def utc(value):
    result = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if result.tzinfo is None:
        raise ValueError("时间必须包含时区")
    return result.astimezone(timezone.utc)


def iso(value):
    return value.isoformat(timespec="milliseconds").replace("+00:00", "Z")


def make_grant(device, user, bucket, access_id, secret, *, now=None, days=7, study_end=None, ttl_seconds=None, shared=False):
    now = (now or datetime.now(timezone.utc))
    if now.tzinfo is None:
        raise ValueError("签发时间必须包含时区")
    now = now.astimezone(timezone.utc).replace(microsecond=0)
    if now.tzinfo is None or not 1 <= days <= 7:
        raise ValueError("授权有效期必须为1至7天")
    device = "*" if shared else str(UUID(device))
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}", user):
        raise ValueError("用户编号须为1至64位字母、数字或 ._-，不要使用姓名或手机号")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{1,61}[a-z0-9]", bucket):
        raise ValueError("Bucket名称无效")
    if not re.fullmatch(r"[A-Za-z0-9]{8,128}", access_id) or not secret:
        raise ValueError("管理员凭据无效")
    if ttl_seconds is not None and not 1 <= ttl_seconds <= 7*86400:
        raise ValueError("授权有效期须在7天内")
    end = now + timedelta(seconds=ttl_seconds) if ttl_seconds is not None else now + timedelta(days=days)
    planned = utc(study_end) if study_end else end
    if not 0 < planned.timestamp() <= 4102444800:
        raise ValueError("批准调研结束时间无效")
    end = min(end, planned + timedelta(days=30))
    if end <= now:
        raise ValueError("调研已到保留截止，不能签发上传授权")
    prefix = f"timelens/direct/v1/users/{user}/devices/" + ("" if shared else device + "/")
    date = now.strftime("%Y%m%d")
    fields = {
        "x-oss-signature-version": "OSS4-HMAC-SHA256",
        "x-oss-credential": f"{access_id}/{date}/cn-beijing/oss/aliyun_v4_request",
        "x-oss-date": now.strftime("%Y%m%dT%H%M%SZ"),
        "x-oss-object-acl": "private",
        "x-oss-server-side-encryption": "AES256",
        "x-oss-forbid-overwrite": "true",
        "success_action_status": "204",
        "Content-Type": "image/webp",
        "x-oss-meta-timelens-user": user,
        "x-oss-meta-timelens-device": "" if shared else device,
        "x-oss-meta-timelens-study-end": iso(planned),
    }
    key = ("aliyun_v4" + secret).encode()
    for part in (date, "cn-beijing", "oss", "aliyun_v4_request"):
        key = hmac.new(key, part.encode(), hashlib.sha256).digest()
    def signed_fields(content_type, allowed_prefix):
        form = {**fields, "Content-Type": content_type}
        conditions = [{"bucket": bucket}, *({k: v} for k, v in form.items() if not (shared and k == "x-oss-meta-timelens-device")),
                      ["starts-with", "$key", allowed_prefix], ["content-length-range", 1, MAX_BYTES],
                      *(["starts-with", f"$x-oss-meta-timelens-{k}", ""] for k in DYNAMIC)]
        if shared:
            conditions.append(["starts-with", "$x-oss-meta-timelens-device", ""])
        policy = base64.b64encode(json.dumps({"expiration": iso(end), "conditions": conditions},
                                           separators=(",", ":")).encode()).decode()
        return {**form, "policy": policy, "x-oss-signature": hmac.new(key, policy.encode(), hashlib.sha256).hexdigest()}
    return {"format": SHARED_FORMAT if shared else FORMAT, "deviceId": device, "userId": user,
            "bucket": bucket, "region": "cn-beijing",
            "endpoint": f"https://{bucket}.oss-cn-beijing.aliyuncs.com/", "prefix": prefix,
            "issuedAt": iso(now), "expiresAt": iso(end), "archiveEndAt": iso(planned),
            "fields": signed_fields("image/webp", prefix),
            "manifestFields": signed_fields("application/json", prefix if shared else prefix + "manifests/")}


def private_json(path):
    path = Path(path)
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > 65536:
        raise ValueError("凭据文件须为普通小文件，不允许符号链接")
    if os.name != "nt" and (info.st_uid != os.getuid() or info.st_mode & 0o077):
        raise ValueError("凭据文件须归当前用户所有且权限为0600")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    with os.fdopen(os.open(path, flags), encoding="utf-8") as source:
        value = json.load(source)
    if set(value) != {"accessKeyId", "accessKeySecret"}:
        raise ValueError("凭据文件只应包含accessKeyId和accessKeySecret")
    return value


def write_grant(path, grant):
    # Create-only: neither symlinks nor another employee's existing grant are overwritten.
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w", encoding="utf-8") as out:
        json.dump(grant, out, ensure_ascii=False, indent=2)
        out.write("\n")


def main():
    parser = argparse.ArgumentParser(description="本地签发TimeLens设备专用7天上传授权；不访问OSS")
    parser.add_argument("--shared", action="store_true", help="签发7天公用配置；按设备ID分目录，但不提供设备间云端写入隔离")
    parser.add_argument("--device-id")
    parser.add_argument("--user-id")
    parser.add_argument("--bucket")
    parser.add_argument("--days", type=int, default=7)
    parser.add_argument("--study-end", help="独立的批准调研结束时间，续签须保持原值；默认本次授权截止")
    parser.add_argument("--credentials-file", help="当前用户0600私有JSON；不要放在源码目录")
    parser.add_argument("--output")
    parser.add_argument("--registry", default=str(Path.home()/".timelens-admin"), help="管理员私有台账目录，续签和清理共用")
    args = parser.parse_args()
    try:
        device = "*" if args.shared else args.device_id or input("员工软件显示的固定设备ID：").strip()
        user = args.user_id or input("用户编号（如employee-001）：").strip()
        bucket = args.bucket or input("北京OSS Bucket名称：").strip()
        credentials = private_json(args.credentials_file) if args.credentials_file else {
            "accessKeyId": getpass.getpass("管理员AccessKey ID（隐藏输入）："),
            "accessKeySecret": getpass.getpass("管理员AccessKey Secret（隐藏输入）："),
        }
        grant = make_grant(device, user, bucket, credentials["accessKeyId"], credentials["accessKeySecret"],
                           days=args.days, study_end=args.study_end, shared=args.shared)
        from .direct_admin import Admin
        # Record expiry BEFORE exporting any usable grant; a failed export is conservative.
        Admin(args.registry).enroll(grant)
        output = args.output or f"TimeLens-{user}-{'shared' if args.shared else grant['deviceId'][:8]}.timelens-upload.json"
        write_grant(output, grant)
        print(f"已生成：{Path(output).resolve()}\n授权截止：{grant['expiresAt']}\n批准调研结束：{grant['archiveEndAt']}")
        print("将授权文件单独交给对应员工。不要上传到Git；本工具未访问OSS，未验证上传权限。")
    except (ValueError, OSError, KeyError, EOFError):
        parser.exit(1, "生成失败：请检查设备ID、用户编号、有效期、凭据文件权限和输出路径；未显示凭据内容。\n")


if __name__ == "__main__":
    main()
