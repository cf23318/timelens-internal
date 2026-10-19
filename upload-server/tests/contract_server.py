"""Loopback-only synthetic fixture for Rust -> FastAPI -> memory OSS tests."""
from pathlib import Path
import json,socket,tempfile
import uvicorn
from timelens_upload.app import create_app
from timelens_upload.config import Settings
from fakes import FakeBucket
with tempfile.TemporaryDirectory(prefix="timelens-contract-fixture-") as directory:
    root=Path(directory);token=root/"tokens.json";token.write_text(json.dumps({"fixture-survey":{"fixture-device":"fixture-only-token-not-real-credentials-12345"}}));token.chmod(0o600)
    bucket=FakeBucket();app=create_app(Settings(root/"state",token,"fixture-private-bucket",min_free_bytes=0),bucket)
    @app.get("/fixture-state")
    async def fixture_state():return {"put_count":bucket.put_count}
    sock=socket.socket();sock.bind(("127.0.0.1",0));sock.listen(128)
    print(f"http://127.0.0.1:{sock.getsockname()[1]}/",flush=True)
    uvicorn.Server(uvicorn.Config(app,log_level="error",access_log=False)).run(sockets=[sock])
