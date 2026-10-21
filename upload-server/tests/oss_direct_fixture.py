"""TLS loopback OSS emulator with real POST V4 HMAC/policy checks; synthetic credentials only."""
import base64
from datetime import datetime, timezone
from email.parser import BytesParser
from email.policy import default
import hashlib
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import ssl
import sys
from timelens_upload.oss_grant import make_grant
from oss2.utils import Crc64

DEVICE = "11111111-1111-4111-8111-111111111111"
ACCESS_ID = "FIXTUREONLYACCESSID"
SECRET = "fixture-secret-used-only-in-loopback-tests"
BUCKET = "fixture-private-bucket"


def grant(device=DEVICE, user="employee-001", **options):
    return make_grant(device, user, BUCKET, ACCESS_ID, SECRET, **options)


def verify(fields, data, now=None):
    policy = json.loads(base64.b64decode(fields["policy"], validate=True))
    if datetime.fromisoformat(policy["expiration"].replace("Z", "+00:00")) <= (now or datetime.now(timezone.utc)):
        raise ValueError("expired")
    identifier, date, region, service, version = fields["x-oss-credential"].split("/")
    if (identifier, region, service, version) != (ACCESS_ID, "cn-beijing", "oss", "aliyun_v4_request"):
        raise ValueError("scope")
    key = ("aliyun_v4" + SECRET).encode()
    for part in (date, region, service, version):
        key = hmac.new(key, part.encode(), hashlib.sha256).digest()
    expected = hmac.new(key, fields["policy"].encode(), hashlib.sha256).hexdigest()
    if not hmac.compare_digest(expected, fields["x-oss-signature"]):
        raise ValueError("signature")
    timestamp = datetime.strptime(fields["x-oss-date"], "%Y%m%dT%H%M%SZ").replace(tzinfo=timezone.utc)
    age = ((now or datetime.now(timezone.utc)) - timestamp).total_seconds()
    if not -900 <= age <= 604800 or date != timestamp.strftime("%Y%m%d"):
        raise ValueError("time")
    values = {**fields, "bucket": BUCKET}
    for condition in policy["conditions"]:
        if isinstance(condition, dict):
            if any(values.get(k) != v for k, v in condition.items()):
                raise ValueError("exact condition")
        elif condition[0] == "starts-with":
            if not values.get(condition[1].removeprefix("$"), "").startswith(condition[2]):
                raise ValueError("prefix condition")
        elif condition[0] == "content-length-range":
            if not condition[1] <= len(data) <= condition[2]:
                raise ValueError("size condition")
        else:
            raise ValueError("unknown condition")
    return fields["key"]


class Handler(BaseHTTPRequestHandler):
    objects = {}
    requests = 0
    disconnected = set()

    def log_message(self, *_args):
        pass

    def do_GET(self):
        payload = json.dumps({"requests": self.requests, "objects": {
            k: {"sha256": hashlib.sha256(v).hexdigest(), "bytes": len(v)} for k, v in self.objects.items()
        }}).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_POST(self):
        type(self).requests += 1
        try:
            size = int(self.headers["Content-Length"])
            if size > 21 * 1024 * 1024:
                raise ValueError("body too large")
            raw = self.rfile.read(size)
            message = BytesParser(policy=default).parsebytes(
                ("Content-Type: " + self.headers["Content-Type"] + "\r\nMIME-Version: 1.0\r\n\r\n").encode() + raw)
            parts = list(message.iter_parts())
            fields = {}
            data = None
            for i, part in enumerate(parts):
                name = part.get_param("name", header="content-disposition")
                if name == "file":
                    if i != len(parts)-1 or data is not None:
                        raise ValueError("file must be last")
                    data = part.get_payload(decode=True)
                else:
                    if name in fields:
                        raise ValueError("duplicate field")
                    fields[name] = part.get_payload(decode=True).decode()
            key = verify(fields, data)
            if self.headers.get("X-Fixture-Fault") == "disconnect-before-store-once" and key not in self.disconnected:
                self.disconnected.add(key)
                self.close_connection = True
                return
            if key in self.objects:
                self.send_response(409); self.end_headers(); return
            self.objects[key] = data
            fault = self.headers.get("X-Fixture-Fault")
            if fault == "disconnect":
                self.close_connection = True
                return
            if fault == "redirect":
                self.send_response(302); self.send_header("Location", "https://must-not-follow.invalid/"); self.end_headers(); return
            crc = Crc64(); crc(data)
            self.send_response(204)
            self.send_header("x-oss-request-id", "synthetic-request-id")
            self.send_header("x-oss-hash-crc64ecma", str(crc.crc ^ (1 if fault == "crc" else 0)))
            self.end_headers()
        except (ValueError, KeyError, TypeError):
            self.send_response(403); self.end_headers()


if __name__ == "__main__":
    if sys.argv[1] in ("--grant", "--shared-grant"):
        print(json.dumps(grant(sys.argv[2] if len(sys.argv)>2 else DEVICE, shared=sys.argv[1] == "--shared-grant")))
    else:
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(sys.argv[1], sys.argv[2])
        server.socket = context.wrap_socket(server.socket, server_side=True)
        print(f"https://127.0.0.1:{server.server_port}/", flush=True)
        server.serve_forever()
