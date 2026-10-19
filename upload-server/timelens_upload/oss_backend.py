"""Small OSS adapter with fail-closed private bucket checks."""

import oss2
from oss2.credentials import EnvironmentVariableCredentialsProvider

from .config import REGION, Settings


def make_bucket(settings: Settings):
    # The SDK reads OSS_ACCESS_KEY_ID and OSS_ACCESS_KEY_SECRET only on this server.
    auth = oss2.ProviderAuthV4(EnvironmentVariableCredentialsProvider())
    # This is a connect/read inactivity timeout, not a hard total deadline.
    # Network PUT/HEAD/GET are kept outside the ingestion lock.
    return oss2.Bucket(auth, settings.endpoint, settings.bucket, region=REGION,
                       enable_crc=True, connect_timeout=10)


def assert_private_bucket(bucket) -> None:
    if bucket.get_bucket_acl().acl != oss2.BUCKET_ACL_PRIVATE:
        raise RuntimeError("OSS bucket ACL is not private")
    block = bucket.get_bucket_public_access_block()
    if block.block_public_access is not True:
        raise RuntimeError("OSS bucket Block Public Access is not enabled")
    # Any policy is rejected: oss2 V1 cannot classify all policy conditions
    # as safely non-public. A public policy must never be accepted by guesswork.
    try:
        bucket.get_bucket_policy()
    except oss2.exceptions.NoSuchBucketPolicy:
        pass
    else:
        raise RuntimeError("OSS bucket policy exists; private access cannot be proven")
    assert_unversioned_bucket(bucket)


def assert_unversioned_bucket(bucket) -> None:
    # Forbid-overwrite and per-participant deletion cannot be guaranteed if
    # historical versions may survive. A missing permission also fails closed.
    if bucket.get_bucket_versioning().status is not None:
        raise RuntimeError("OSS bucket versioning must never have been enabled")


def assert_private_object(bucket, key: str) -> None:
    if bucket.get_object_acl(key).acl != oss2.OBJECT_ACL_PRIVATE:
        raise RuntimeError("OSS object ACL is not explicitly private")


def delete_and_verify_absent(bucket, key: str) -> None:
    """Prove one unaccepted or expired object is absent after deletion."""
    try:
        bucket.head_object(key)
    except oss2.exceptions.NotFound:
        return
    try:
        bucket.delete_object(key)
    except oss2.exceptions.NotFound:
        pass
    try:
        bucket.head_object(key)
    except oss2.exceptions.NotFound:
        return
    raise RuntimeError("OSS object survived delete")


def header_value(result, name: str):
    """OSS SDK headers may be regular or case-insensitive mappings."""
    return next((v for k, v in result.headers.items() if k.lower() == name.lower()), None)


def verified_head(bucket, key: str, sha256: str, size: int, crc64: int,
                  metadata: dict = None):
    """Return a verified HEAD or raise ValueError for conflicting/corrupt data."""
    head = bucket.head_object(key)
    remote_crc = header_value(head, "x-oss-hash-crc64ecma")
    remote_sha = header_value(head, "x-oss-meta-sha256")
    if (
        head.content_length != size
        or remote_sha != sha256
        or remote_crc is None
        or int(remote_crc) != crc64
    ):
        raise ValueError("Existing OSS object differs from submitted frame")
    if header_value(head, "x-oss-server-side-encryption") != "AES256":
        raise ValueError("Existing OSS object lacks AES256 server-side encryption")
    assert_private_object(bucket, key)
    if metadata is not None:
        for field, value in metadata.items():
            remote = header_value(head, "x-oss-meta-" + field.replace("_", "-"))
            if remote != (str(value) if value is not None else None):
                raise ValueError("Existing OSS object metadata differs from submitted frame")
    return head
