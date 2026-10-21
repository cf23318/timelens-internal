"""Adapted /v1/frames receiver. Identity, private OSS verification and durable receipts only.
No capture scheduling, fixed duration, public object URLs, or automatic deletion jobs.
"""
import asyncio
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import tempfile
import time
import threading
from uuid import UUID
import warnings

import oss2
from oss2.utils import Crc64
from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import Response
from . import archive
from starlette.concurrency import run_in_threadpool
from PIL import Image
from .config import Settings, private_dir, valid_identifier
from .oss_backend import make_bucket, assert_private_bucket, verified_head

Image.MAX_IMAGE_PIXELS = 40_000_000

@contextmanager
def registry(settings):
    # A single host's interprocess lock serializes identity reservations and commits.
    import fcntl
    fd = os.open(settings.spool_dir / "ingest.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        path = settings.spool_dir / "receipts.sqlite3"
        if path.is_symlink():
            raise ValueError("Receipt database cannot be a symlink")
        conn = sqlite3.connect(path, timeout=10)
        os.chmod(path, 0o600)
        try:
            conn.execute("PRAGMA synchronous=FULL")
            conn.execute("""CREATE TABLE IF NOT EXISTS frames (
              snapshot_id TEXT PRIMARY KEY, survey TEXT NOT NULL, device TEXT NOT NULL,
              group_id TEXT NOT NULL, display_id INTEGER NOT NULL, identity TEXT NOT NULL,
              size INTEGER NOT NULL, object_key TEXT NOT NULL, confirmed INTEGER NOT NULL DEFAULT 0,
              UNIQUE(survey,device,group_id,display_id))""")
            archive.schema(conn)
            conn.commit()
            yield conn
            conn.commit()
        finally:
            conn.close()
    finally:
        os.close(fd)

def required(request, name):
    value = request.headers.get(name)
    if not value or len(value) > 4096:
        raise HTTPException(400, "Missing or invalid frame header")
    return value

def integer(request, name, lower, upper):
    text = required(request, name)
    if not text.isascii() or not text.isdecimal() or len(text) > 12 or not lower <= int(text) <= upper:
        raise HTTPException(400, "Invalid numeric frame header")
    return int(text)

def canonical_uuid(value):
    try:
        parsed = str(UUID(value))
        if value != parsed:
            raise ValueError()
        return parsed
    except ValueError:
        raise HTTPException(400, "Non-canonical frame UUID") from None

def metadata(request, settings, snapshot_id):
    canonical_uuid(snapshot_id)
    survey, device = required(request, "X-TimeLens-Survey"), required(request, "X-TimeLens-Device")
    if not valid_identifier(survey) or not valid_identifier(device):
        raise HTTPException(400, "Invalid device identity")
    token = required(request, "Authorization")
    if not token.startswith("Bearer ") or not settings.authorized(survey, device, token[7:]):
        raise HTTPException(401, "Invalid device credential")
    raw_time = required(request, "X-TimeLens-Captured-At")
    try:
        captured = datetime.fromisoformat(raw_time.replace("Z", "+00:00"))
        if captured.tzinfo is None:
            raise ValueError()
        captured = captured.astimezone(timezone.utc)
    except (ValueError, OverflowError):
        raise HTTPException(400, "Timestamp must include a timezone") from None
    sha = required(request, "X-TimeLens-Sha256")
    if len(sha) != 64 or any(c not in "0123456789abcdef" for c in sha):
        raise HTTPException(400, "Invalid SHA256")
    size = integer(request, "X-TimeLens-Size-Bytes", 12, settings.max_frame_bytes)
    length = request.headers.get("Content-Length")
    if length is not None and (not length.isdecimal() or int(length) != size):
        raise HTTPException(400, "Content length mismatch")
    if request.headers.get("Content-Type", "").split(";")[0].strip().lower() not in {"image/webp", "application/octet-stream"}:
        raise HTTPException(415, "Expected WebP")
    info = dict(snapshot_id=snapshot_id, survey=survey, device=device,
                captured_at=captured.isoformat(timespec="milliseconds").replace("+00:00", "Z"),
                group_id=canonical_uuid(required(request, "X-TimeLens-Capture-Group-ID")),
                elapsed_ms=integer(request, "X-TimeLens-Group-Elapsed-Ms", 0, 15000),
                expected_displays=integer(request, "X-TimeLens-Expected-Displays", 1, 32),
                display_id=integer(request, "X-TimeLens-Display-ID", 0, 4294967295), sha256=sha, size_bytes=size)
    info["task_id"] = canonical_uuid(required(request, "X-TimeLens-Task"))
    info["archive_end_at"] = datetime.fromtimestamp(archive.timestamp(required(request, "X-TimeLens-Archive-End"))/1000,timezone.utc).isoformat(timespec="milliseconds").replace("+00:00","Z")
    key = f"timelens/v2/surveys/{survey}/devices/{device}/tasks/{info['task_id']}/{captured:%Y/%m/%d}/{snapshot_id}.webp"
    return info, key, token[7:]

def verify_or_upload(bucket, key, path, info, crc):
    remote_meta = {k:v for k,v in info.items() if k not in {"sha256", "size_bytes"}}
    try:
        verified_head(bucket, key, info["sha256"], info["size_bytes"], crc, remote_meta)
        return
    except oss2.exceptions.NotFound:
        pass
    headers = {"Content-Type":"image/webp", "x-oss-object-acl":"private", "x-oss-server-side-encryption":"AES256", "x-oss-forbid-overwrite":"true", "x-oss-meta-sha256":info["sha256"]}
    headers.update({"x-oss-meta-"+k.replace("_","-"):str(v) for k,v in remote_meta.items()})
    try:
        result = bucket.put_object_from_file(key, str(path), headers=headers)
        if result.crc is None or int(result.crc) != crc:
            raise ValueError("Upload CRC64 mismatch")
    except oss2.exceptions.OssError as error:
        if error.code != "FileAlreadyExists":
            raise
    verified_head(bucket, key, info["sha256"], info["size_bytes"], crc, remote_meta)

def commit(settings, bucket, info, key, token, path, crc):
    identity = json.dumps(info, sort_keys=True, separators=(",", ":"))
    # Reserve immutable identity before any remote write. Commit survives failed ACK and restart.
    with registry(settings) as conn:
        found = conn.execute("SELECT identity FROM frames WHERE snapshot_id=?", (info["snapshot_id"],)).fetchone()
        if found and found[0] != identity:
            raise HTTPException(409, "Snapshot identity conflict")
        if not found:
            used = conn.execute("SELECT COALESCE(SUM(size),0) FROM frames WHERE survey=? AND device=?", (info["survey"],info["device"])).fetchone()[0]
            if used + info["size_bytes"] > settings.max_device_bytes:
                raise HTTPException(429, "Device storage budget reached")
            try:
                conn.execute("INSERT INTO frames(snapshot_id,survey,device,group_id,display_id,identity,size,object_key,task_id,crc64) VALUES(?,?,?,?,?,?,?,?,?,?)",
                             (info["snapshot_id"],info["survey"],info["device"],info["group_id"],info["display_id"],identity,info["size_bytes"],key,info["task_id"],str(crc)))
            except sqlite3.IntegrityError:
                raise HTTPException(409, "Capture group/display conflict") from None
    archive.authorized_archive(settings,info["task_id"],info["survey"],info["device"],token)
    assert_private_bucket(bucket)
    verify_or_upload(bucket, key, path, info, crc)
    assert_private_bucket(bucket)
    archive.authorized_archive(settings,info["task_id"],info["survey"],info["device"],token)
    with registry(settings) as conn:
        archive.require_live(settings,archive.get_archive(conn,info["task_id"]))
        conn.execute("UPDATE frames SET confirmed=1 WHERE snapshot_id=? AND identity=?", (info["snapshot_id"],identity))
    return dict(snapshot_id=info["snapshot_id"],object_key=key,sha256=info["sha256"],size_bytes=info["size_bytes"],verified=True)

def image_valid(path, expected_size):
    with open(path, "rb") as f:
        header = f.read(12)
    if len(header) != 12 or header[:4] != b"RIFF" or header[8:12] != b"WEBP" or int.from_bytes(header[4:8],"little") + 8 != expected_size:
        raise HTTPException(422, "Invalid WebP container")
    try:
        with warnings.catch_warnings():
            warnings.simplefilter("error", Image.DecompressionBombWarning)
            with Image.open(path) as image:
                if image.format != "WEBP":
                    raise ValueError()
                image.load()
    except Exception:
        raise HTTPException(422, "WebP cannot be decoded within image limits") from None

def create_app(settings=None, bucket=None):
    settings = settings or Settings.from_env()
    settings.validate()
    private_dir(settings.spool_dir)
    pending = private_dir(settings.spool_dir / "pending")
    bucket = bucket if bucket is not None else make_bucket(settings)
    app = FastAPI(docs_url=None, redoc_url=None, openapi_url=None)
    @app.middleware("http")
    async def no_cache(request, call_next):
        response=await call_next(request)
        response.headers["Cache-Control"]="no-store"
        return response
    slots = asyncio.Semaphore(settings.max_inflight)
    budget_lock = threading.Lock()
    reserved = 0

    @app.get("/healthz")
    async def health():
        return {"protocol":"raw-frames-v2", "retention_supported":True, "retention_worker_required":True}

    @app.post("/v1/frames/{snapshot_id}")
    async def frame(snapshot_id: str, request: Request):
        info,_,_ = metadata(request,settings,snapshot_id)
        with archive.lease(settings,info["task_id"]):
            archive.enroll(settings,info)
            archive.authorized_archive(settings,info["task_id"],info["survey"],info["device"],required(request,"Authorization")[7:])
            return await ingest(snapshot_id,request)

    async def ingest(snapshot_id: str, request: Request):
        nonlocal reserved
        info,key,token = metadata(request,settings,snapshot_id)
        try:
            await asyncio.wait_for(slots.acquire(), timeout=0.1)
        except asyncio.TimeoutError:
            raise HTTPException(429,"Ingestion concurrency limit") from None
        path = None
        claimed = False
        try:
            with budget_lock:
                occupied = sum(p.stat().st_size for p in pending.rglob("*.webp") if p.is_file())
                if shutil.disk_usage(pending).free - reserved - info["size_bytes"] < settings.min_free_bytes or occupied + reserved + info["size_bytes"] > settings.max_pending_bytes:
                    raise HTTPException(503,"Ingestion disk budget unavailable")
                reserved += info["size_bytes"]
                claimed = True
            task_pending = private_dir(pending / info["task_id"])
            fd,name = tempfile.mkstemp(prefix="frame-",suffix=".webp",dir=task_pending)
            path = Path(name)
            sha,crc,received = hashlib.sha256(),Crc64(),0
            started = time.monotonic()
            with os.fdopen(fd,"wb") as out:
                chunks = request.stream().__aiter__()
                while True:
                    remaining = 120 - (time.monotonic()-started)
                    if remaining <= 0:
                        raise HTTPException(408,"Frame upload timed out")
                    try:
                        chunk = await asyncio.wait_for(chunks.__anext__(),timeout=min(5,remaining))
                    except StopAsyncIteration:
                        break
                    except asyncio.TimeoutError:
                        raise HTTPException(408,"Frame upload timed out") from None
                    received += len(chunk)
                    if received > info["size_bytes"]:
                        raise HTTPException(413,"Frame exceeded declared size")
                    archive.authorized_archive(settings,info["task_id"],info["survey"],info["device"],token)
                    sha.update(chunk);crc.update(chunk);out.write(chunk)
                out.flush();os.fsync(out.fileno())
            if received != info["size_bytes"] or sha.hexdigest() != info["sha256"]:
                raise HTTPException(400,"Frame length or hash mismatch")
            await run_in_threadpool(image_valid,path,received)
            return await run_in_threadpool(commit,settings,bucket,info,key,token,path,crc.crc)
        except HTTPException:
            raise
        except ValueError:
            raise HTTPException(409,"Stored object verification conflict") from None
        except Exception:
            raise HTTPException(503,"Private storage or receipt persistence unavailable") from None
        finally:
            try:
                if path is not None:
                    path.unlink(missing_ok=True)
                    if not any(path.parent.iterdir()): path.parent.rmdir()
            except OSError:
                raise HTTPException(503,"Temporary frame cleanup unavailable") from None
            finally:
                if claimed:
                    with budget_lock:
                        reserved -= info["size_bytes"]
                slots.release()
    def identity(request):
        survey,device=required(request,"X-TimeLens-Survey"),required(request,"X-TimeLens-Device")
        token=required(request,"Authorization")
        if not token.startswith("Bearer "): raise HTTPException(401,"Invalid credential")
        return survey,device,token[7:]

    @app.post("/v1/archives/{task_id}/register")
    def archive_register(task_id: str,request: Request):
        canonical_uuid(task_id)
        who=identity(request)
        if not settings.authorized(*who): raise HTTPException(401,"Invalid credential")
        archive.enroll(settings,dict(task_id=task_id,survey=who[0],device=who[1],archive_end_at=required(request,"X-TimeLens-Archive-End"),captured_at="1970-01-01T00:00:00.001Z"))
        return archive.status(settings,task_id,*who)

    @app.get("/v1/archives/{task_id}")
    def archive_status(task_id: str,request: Request):
        canonical_uuid(task_id)
        return archive.status(settings,task_id,*identity(request))

    @app.get("/v1/archives/{task_id}/reconcile")
    def archive_reconcile(task_id: str,request: Request):
        canonical_uuid(task_id)
        try:return archive.reconcile(settings,bucket,task_id,*identity(request))
        except HTTPException:raise
        except Exception:raise HTTPException(503,"OSS listing reconciliation failed") from None

    @app.get("/v1/archives/{task_id}/manifest")
    def archive_manifest(task_id: str,request: Request,after: str=""):
        canonical_uuid(task_id)
        if after: canonical_uuid(after)
        try: return archive.manifest(settings,bucket,task_id,*identity(request),after)
        except HTTPException: raise
        except Exception: raise HTTPException(503,"Remote manifest verification failed") from None

    @app.post("/v1/archives/{task_id}/finish")
    async def archive_finish(task_id: str,request: Request):
        canonical_uuid(task_id)
        who=identity(request)
        archive.authorized_archive(settings,task_id,*who)
        data=bytearray();chunks=request.stream().__aiter__();started=time.monotonic()
        while True:
            remaining=10-(time.monotonic()-started)
            if remaining<=0: raise HTTPException(408,"Finish request timed out")
            try: chunk=await asyncio.wait_for(chunks.__anext__(),timeout=min(5,remaining))
            except StopAsyncIteration:break
            except asyncio.TimeoutError:raise HTTPException(408,"Finish request timed out") from None
            data.extend(chunk)
            if len(data)>256: raise HTTPException(413,"Finish metadata too large")
        try:
            body=json.loads(data)
            if set(body)!={"ended_at"}: raise ValueError()
        except (ValueError,TypeError): raise HTTPException(400,"Invalid finish metadata") from None
        try: return archive.finish(settings,task_id,*identity(request),body["ended_at"])
        except HTTPException: raise
        except Exception: raise HTTPException(503,"Archive busy or unavailable") from None

    @app.get("/v1/archives/{task_id}/frames/{snapshot_id}")
    def archive_read(task_id: str,snapshot_id: str,request: Request):
        canonical_uuid(task_id);canonical_uuid(snapshot_id)
        try:
            data,info=archive.read_frame(settings,bucket,task_id,*identity(request),snapshot_id)
            return Response(data,media_type="image/webp",headers={"Cache-Control":"no-store","X-TimeLens-Sha256":info["sha256"]})
        except HTTPException: raise
        except Exception: raise HTTPException(503,"Remote readback verification failed") from None
    return app
