"""Server credentials remain on the server. No default temporary state directory."""
import hmac
import json
import os
import re
from dataclasses import dataclass
from pathlib import Path

REGION = "cn-beijing"
IDENTIFIER = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}\Z")
ENDPOINTS = {"https://oss-cn-beijing.aliyuncs.com", "https://oss-cn-beijing-internal.aliyuncs.com"}

def valid_identifier(value):
    return isinstance(value, str) and bool(IDENTIFIER.fullmatch(value))

def private_dir(path):
    path = Path(path)
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0))
    try:
        if os.name == "posix":
            if os.fstat(fd).st_uid != os.geteuid():
                raise ValueError("State directory has a different owner")
            os.fchmod(fd, 0o700)
    finally:
        os.close(fd)
    return path

@dataclass(frozen=True)
class Settings:
    spool_dir: Path
    tokens_file: Path
    bucket: str
    endpoint: str = "https://oss-cn-beijing.aliyuncs.com"
    max_frame_bytes: int = 20 * 1024 * 1024
    max_inflight: int = 4
    min_free_bytes: int = 256 * 1024 * 1024
    max_pending_bytes: int = 256 * 1024 * 1024
    max_device_bytes: int = 20 * 1024**3

    @classmethod
    def from_env(cls):
        # Explicit persistent storage and bucket are required; never inherit a real bucket.
        settings = cls(Path(os.environ["TIMELENS_SPOOL_DIR"]), Path(os.environ["TIMELENS_DEVICE_TOKENS_FILE"]),
                       os.environ["TIMELENS_OSS_BUCKET"], os.environ.get("TIMELENS_OSS_ENDPOINT", "https://oss-cn-beijing.aliyuncs.com"))
        settings.validate()
        return settings

    def validate(self):
        if not self.spool_dir.is_absolute() or self.endpoint not in ENDPOINTS or not re.fullmatch(r"[a-z0-9][a-z0-9-]{1,61}[a-z0-9]", self.bucket):
            raise ValueError("Explicit absolute state directory, private bucket and approved domestic endpoint required")
        if not 12 <= self.max_frame_bytes <= 100*1024*1024 or not 1 <= self.max_inflight <= 32:
            raise ValueError("Invalid capacity limits")
        if min(self.min_free_bytes, self.max_pending_bytes, self.max_device_bytes) < 0:
            raise ValueError("Invalid capacity budget")

    def authorized(self, survey, device, token):
        try:
            fd = os.open(self.tokens_file, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
            with os.fdopen(fd, "r", encoding="utf-8") as source:
                meta = os.fstat(source.fileno())
                if meta.st_size > 1024*1024 or (os.name == "posix" and (meta.st_mode & 0o077 or meta.st_uid != os.geteuid())):
                    raise ValueError("Token file must be owner-only")
                values = json.load(source)
            expected = values[survey][device]
            if not isinstance(expected, str) or len(expected) < 32 or len(expected) > 4096 or any(c.isspace() for c in expected):
                return False
            return isinstance(token, str) and hmac.compare_digest(expected.encode(), token.encode())
        except (OSError, ValueError, TypeError, KeyError):
            return False
