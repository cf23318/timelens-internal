"""Loopback-only offline signer. No OSS requests and no administrator key in the browser."""
import argparse
from datetime import datetime, timedelta, timezone
import hmac
import json
from pathlib import Path
import secrets
from uuid import UUID, uuid4

from fastapi import FastAPI, Request, HTTPException
from fastapi.responses import HTMLResponse, FileResponse

from .direct_admin import Admin, private_dir, read_private
from .oss_grant import SHARED_FORMAT, make_grant, private_json, write_grant


def create_app(registry, credentials_file, bucket, *, port=8877):
    registry = Path(registry)
    admin = Admin(registry)
    output = private_dir(registry / "grants")
    origin = f"http://127.0.0.1:{port}"
    csrf = secrets.token_urlsafe(32)
    app = FastAPI(docs_url=None, redoc_url=None, openapi_url=None)

    @app.middleware("http")
    async def local_only(request: Request, call_next):
        if request.headers.get("host") != f"127.0.0.1:{port}":
            return HTMLResponse("仅限本机访问", status_code=403)
        if request.method == "POST":
            if request.headers.get("origin") != origin or not hmac.compare_digest(request.headers.get("x-timelens-csrf", ""), csrf):
                return HTMLResponse("请求来源验证失败", status_code=403)
            try:
                if not 0 < int(request.headers.get("content-length", "0")) <= 4096:
                    return HTMLResponse("请求大小无效", status_code=413)
            except ValueError:
                return HTMLResponse("请求大小无效", status_code=413)
        response = await call_next(request)
        response.headers.update({"Cache-Control": "no-store", "X-Content-Type-Options": "nosniff", "Referrer-Policy": "no-referrer",
                                 "Content-Security-Policy": "default-src 'none'; script-src 'nonce-" + csrf + "'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"})
        return response

    def grant_path(identifier):
        try:
            if str(UUID(identifier)) != identifier:
                raise ValueError()
        except ValueError:
            raise HTTPException(404, "配置不存在")
        return output / f"shared-{identifier}.timelens-upload.json"

    def summary(path):
        value = read_private(path)
        if value is None or value.get("format") != SHARED_FORMAT:
            raise HTTPException(404, "配置不存在")
        return {k: value[k] for k in ("userId", "issuedAt", "expiresAt", "archiveEndAt", "endpoint", "prefix")}

    @app.get("/", response_class=HTMLResponse)
    def index():
        now = datetime.now(timezone.utc)
        initial = {"userId": f"shared-{now:%Y%m%d}-{uuid4().hex[:8]}",
                   "archiveEndAt": (now + timedelta(days=7)).isoformat(timespec="seconds"), "csrf": csrf, "bucket": bucket}
        return HTML.replace("__INITIAL__", json.dumps(initial).replace("<", "\\u003c" )).replace("__NONCE__", csrf)

    @app.get("/api/grants")
    def list_grants():
        result = []
        for path in sorted(output.glob("shared-*.timelens-upload.json"), key=lambda p: p.stat().st_mtime, reverse=True)[:100]:
            identifier = path.name.removeprefix("shared-").removesuffix(".timelens-upload.json")
            try:
                grant_path(identifier)
                result.append({**summary(path), "id": identifier})
            except (HTTPException, ValueError, OSError, KeyError):
                continue
        return result

    @app.post("/api/grants")
    async def generate(request: Request):
        try:
            body = await request.json()
            if not isinstance(body, dict) or set(body) != {"userId", "archiveEndAt"}:
                raise ValueError()
            if not isinstance(body["userId"], str) or not isinstance(body["archiveEndAt"], str):
                raise ValueError()
            credentials = private_json(credentials_file)
            grant = make_grant("*", body["userId"], bucket, credentials["accessKeyId"], credentials["accessKeySecret"],
                               days=7, study_end=body["archiveEndAt"], shared=True)
            identifier = str(uuid4())
            # Same private registry as the cleanup daemon; record before exporting usable credentials.
            admin.enroll(grant)
            path = grant_path(identifier)
            write_grant(path, grant)
            return {**summary(path), "id": identifier, "filename": path.name}
        except (ValueError, OSError, KeyError, TypeError):
            raise HTTPException(400, "生成失败：检查批次编号、结束时间和私有凭据；续签需保持原批次与批准结束时间。")

    @app.get("/api/grants/{identifier}/download")
    def download(identifier: str):
        path = grant_path(identifier)
        summary(path)
        return FileResponse(path, media_type="application/json", filename=path.name)

    return app


HTML = r'''<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>TimeLens 上传配置</title><style>
*{box-sizing:border-box}body{margin:0;background:#f4f6fa;color:#1e293b;font:15px/1.65 -apple-system,BlinkMacSystemFont,"PingFang SC",sans-serif}main{max-width:940px;margin:42px auto;padding:0 24px}h1{font-size:30px;letter-spacing:-.7px;margin:12px 0}h2{font-size:18px;margin:0 0 16px}.tag{color:#2563eb;font-weight:600;font-size:13px}.sub{color:#64748b}.card{background:white;border:1px solid #e2e8f0;border-radius:14px;padding:26px;margin:22px 0;box-shadow:0 3px 12px #18284905}.grid{display:grid;grid-template-columns:1fr 1fr;gap:18px}label{display:block;font-weight:600}input{display:block;width:100%;border:1px solid #cbd5e1;border-radius:8px;padding:11px;margin-top:8px;font:inherit;color:inherit;background:#fff}button,.download{display:inline-block;border:0;border-radius:8px;padding:11px 20px;background:#2563eb;color:#fff;text-decoration:none;cursor:pointer;font:inherit;font-weight:600}button:disabled{opacity:.5;cursor:wait}.note{padding:14px 18px;border-radius:9px;background:#fff7e6;color:#805400}.ok{background:#eff8f1;color:#247246;padding:14px;border-radius:9px}small{font-weight:400;color:#64748b}code{font-size:12px;overflow-wrap:anywhere}.row{padding:18px 0;border-bottom:1px solid #e2e8f0}.row:last-child{border:0}.row p{margin:5px 0}.error{color:#b91c1c}#message{margin-top:18px}details{margin-top:12px}@media(max-width:600px){.grid{grid-template-columns:1fr}main{margin:22px auto;padding:0 16px}.card{padding:20px}}
</style><main><span class="tag">TIMELENS · 本机管理工具</span><h1>上传配置</h1><p class="sub">生成一份公用配置，分发给同一批次的使用者。每台设备保存到各自固定 ID 的目录。</p>
<div class="note">公用授权只按目录区分设备，不能阻止持有者向同批次其他设备目录写入。配置不含长期 Secret，仍须妥善保管；仅可新增上传，不能读取、删除或覆盖。</div>
<section class="card"><h2>生成 7 天公用配置</h2><form id="form"><div class="grid"><label>批次编号<input id="user" required pattern="[A-Za-z0-9][A-Za-z0-9._-]{0,63}" maxlength="64"><small>同一批员工用同一编号，续签保持不变。</small></label><label>批准调研结束时间<input id="end" type="datetime-local" step="0.001" required><small>北京时间。数据到期规则与上传授权有效期独立。</small></label></div><p class="sub" id="destination"></p><button id="generate" type="submit">生成公用配置</button><div id="message" role="status"></div></form></section>
<section class="card"><h2>已生成的公用配置</h2><div id="list" class="sub">正在读取…</div></section>
<p class="sub">仅在本机 127.0.0.1 运行。签发过程不连接 OSS、不上传截图、不自动导入或开启上传。公用配置需要支持此格式的新版客户端；旧版会拒绝导入。</p></main>
<script nonce="__NONCE__">const initial=__INITIAL__;const $=id=>document.getElementById(id);$('user').value=initial.userId;const end=new Date(initial.archiveEndAt);$('end').value=new Date(end.getTime()+8*3600000).toISOString().slice(0,23);$('destination').textContent='OSS：'+initial.bucket+' · 北京区域 · 授权从生成时起有效 7 天';
function row(g){const d=document.createElement('div');d.className='row';const title=document.createElement('strong');title.textContent=g.userId;d.append(title);const p=document.createElement('p');p.textContent='有效至 '+new Date(g.expiresAt).toLocaleString('zh-CN',{timeZone:'Asia/Shanghai'})+'（北京时间）';d.append(p);const code=document.createElement('code');code.textContent=g.prefix+'<各设备固定ID>/';d.append(code,document.createElement('br'));const a=document.createElement('a');a.className='download';a.href='/api/grants/'+g.id+'/download';a.textContent='下载配置 JSON';a.style.marginTop='12px';d.append(a);const renew=document.createElement('button');renew.type='button';renew.textContent='续签此批次';renew.style.marginLeft='12px';renew.addEventListener('click',()=>{$('user').value=g.userId;$('end').value=new Date(new Date(g.archiveEndAt).getTime()+8*3600000).toISOString().slice(0,23);$('message').className='';$('message').textContent='已填入原批次和批准结束时间。点击生成，得到新的7天配置。';$('form').scrollIntoView({behavior:'smooth'});});d.append(renew);return d;}
async function refresh(){const response=await fetch('/api/grants');if(!response.ok)throw Error('读取失败');const list=await response.json();$('list').replaceChildren();if(!list.length)$('list').textContent='还没有生成配置。';for(const g of list)$('list').append(row(g));}
$('form').addEventListener('submit',async e=>{e.preventDefault();$('generate').disabled=true;$('message').textContent='正在本机签发…';try{const response=await fetch('/api/grants',{method:'POST',headers:{'Content-Type':'application/json','X-TimeLens-CSRF':initial.csrf},body:JSON.stringify({userId:$('user').value,archiveEndAt:new Date($('end').value+'+08:00').toISOString()})});const result=await response.json();if(!response.ok)throw Error(result.detail||'生成失败');$('message').className='ok';$('message').textContent='已生成。将同一份 JSON 发给这一批使用者；生成过程没有上传数据。';await refresh();}catch(err){$('message').className='error';$('message').textContent=err.message;}finally{$('generate').disabled=false;}});refresh().catch(()=>{$('list').textContent='读取配置列表失败';});</script></html>'''


def main():
    parser = argparse.ArgumentParser(description="本机网页签发7天公用上传配置，不访问OSS")
    parser.add_argument("--registry", default=str(Path.home()/".timelens-admin"))
    parser.add_argument("--credentials-file", required=True)
    parser.add_argument("--bucket", required=True)
    parser.add_argument("--port", type=int, default=8877)
    args = parser.parse_args()
    if not 1024 <= args.port <= 65535:
        parser.error("端口须在1024至65535之间")
    import uvicorn
    uvicorn.run(create_app(args.registry, args.credentials_file, args.bucket, port=args.port), host="127.0.0.1", port=args.port, access_log=False, log_level="warning")


if __name__ == "__main__":
    main()
