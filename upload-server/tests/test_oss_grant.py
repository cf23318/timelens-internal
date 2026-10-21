from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import pytest
from timelens_upload.oss_grant import make_grant, private_json, write_grant, MAX_BYTES
from oss_direct_fixture import grant, verify, DEVICE


def form(g, key=None):
    return {**g["fields"], "key": key or g["prefix"]+"test.webp",
            **{f"x-oss-meta-timelens-{k}": "fixture" for k in ("sha256", "task", "captured-at", "display")}}


def test_signed_grant_supports_multiple_new_objects_and_isolates_users():
    one = grant()
    two = grant(user="employee-002")
    for name in ("first", "second"):
        assert verify(form(one, one["prefix"]+name+".webp"), b"synthetic").startswith(one["prefix"])
    with pytest.raises(ValueError): verify(form(one, two["prefix"]+"evil.webp"), b"synthetic")
    assert "accessKeySecret" not in json.dumps(one)
    assert one["userId"] != two["userId"]


@pytest.mark.parametrize("field,value", [("Content-Type","application/json"), ("x-oss-object-acl","public-read"),
    ("x-oss-forbid-overwrite","false"), ("x-oss-meta-timelens-device","other"),
    ("x-oss-meta-timelens-user","other"), ("x-oss-signature","0"*64)])
def test_tampering_is_denied(field, value):
    f = form(grant()); f[field] = value
    with pytest.raises(ValueError): verify(f,b"synthetic")


def test_expiry_exact_boundary_and_file_limit():
    now = datetime.now(timezone.utc).replace(microsecond=0)
    g = grant(now=now)
    assert verify(form(g),b"synthetic",now+timedelta(days=7,microseconds=-1))
    with pytest.raises(ValueError): verify(form(g),b"synthetic",now+timedelta(days=7))
    with pytest.raises(ValueError): verify(form(g),b"x"*(MAX_BYTES+1))
    with pytest.raises(ValueError): grant(days=8)


def test_credentials_and_output_protection(tmp_path):
    secret = tmp_path/"private.json"
    secret.write_text(json.dumps({"accessKeyId":"FIXTUREONLYACCESSID","accessKeySecret":"not-real"}))
    secret.chmod(0o644)
    with pytest.raises(ValueError): private_json(secret)
    secret.chmod(0o600)
    assert private_json(secret)["accessKeySecret"] == "not-real"
    link=tmp_path/"link.json"; link.symlink_to(secret)
    with pytest.raises(ValueError): private_json(link)
    out=tmp_path/"grant.json"; write_grant(out,grant())
    assert out.stat().st_mode & 0o077 == 0
    with pytest.raises(FileExistsError): write_grant(out,grant())


def test_renewal_preserves_independent_study_end():
    first=grant()
    renewed=grant(now=datetime.now(timezone.utc)+timedelta(days=6),study_end=first["archiveEndAt"])
    assert first["prefix"]==renewed["prefix"]
    assert first["archiveEndAt"]==renewed["archiveEndAt"]
    assert first["expiresAt"]!=renewed["expiresAt"]


def test_signature_matches_official_oss_sdk():
    from types import SimpleNamespace
    from oss2.auth import ProviderAuthV4
    from oss2.credentials import Credentials, StaticCredentialsProvider
    from oss_direct_fixture import ACCESS_ID, SECRET
    import hashlib
    import hmac
    g = grant()
    auth = ProviderAuthV4(StaticCredentialsProvider(ACCESS_ID, SECRET))
    signing_key = auth._ProviderAuthV4__get_signing_key(
        SimpleNamespace(region="cn-beijing", cloudbox_id=None, product="oss"),
        Credentials(ACCESS_ID, SECRET), g["fields"]["x-oss-date"])
    expected = hmac.new(signing_key, g["fields"]["policy"].encode(), hashlib.sha256).hexdigest()
    assert hmac.compare_digest(expected, g["fields"]["x-oss-signature"])
