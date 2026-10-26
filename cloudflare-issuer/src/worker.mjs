// The Worker only signs policies. It never receives screenshots or contacts OSS.
const DAY = 86400000, RETENTION = 30 * DAY, DRAIN = 120000;
const enc = new TextEncoder();
const id = v => typeof v === 'string' && /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(v);
const uuid = v => typeof v === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(v);
const secret = v => typeof v === 'string' && /^[0-9a-f]{64}$/.test(v);
function must(ok, status = 400) { if (!ok) throw Object.assign(new Error('Request rejected'), {status}); }
function date(v) { const n = Date.parse(v); must(typeof v === 'string' && Number.isFinite(n) && n > 0 && n <= 4102444800000); return n; }
function bounded(v, min, max) { must(Number.isSafeInteger(v) && v >= min && v <= max); return v; }
const iso = n => new Date(n).toISOString();
const random = () => Array.from(crypto.getRandomValues(new Uint8Array(32)), b => b.toString(16).padStart(2, '0')).join('');
const hex = a => Array.from(new Uint8Array(a), b => b.toString(16).padStart(2, '0')).join('');
async function hash(v) { return hex(await crypto.subtle.digest('SHA-256', enc.encode(v))); }
async function hmac(key, text) {
  const k = await crypto.subtle.importKey('raw', key, {name:'HMAC', hash:'SHA-256'}, false, ['sign']);
  return new Uint8Array(await crypto.subtle.sign('HMAC', k, enc.encode(text)));
}
function json(v, status = 200) { return Response.json(v, {status, headers:{'Cache-Control':'no-store', 'X-Content-Type-Options':'nosniff'}}); }
async function body(req) {
  must(req.headers.get('content-type')?.split(';')[0] === 'application/json', 415);
  const reader = req.body?.getReader(); must(reader);
  let size = 0, chunks = [];
  const timer=setTimeout(()=>reader.cancel(),5000);
  try { for (;;) { const {done, value} = await reader.read(); if (done) break; size += value.length; if (size > 262144) { await reader.cancel(); must(false, 413); } chunks.push(value); } }
  finally {clearTimeout(timer);}
  const bytes = new Uint8Array(size); let at = 0; for (const c of chunks) { bytes.set(c, at); at += c.length; }
  let v; try { v = JSON.parse(new TextDecoder('utf-8', {fatal:true}).decode(bytes)); } catch { must(false); }
  must(v && typeof v === 'object' && !Array.isArray(v)); return v;
}
function exact(v, keys) { must(Object.keys(v).every(k => keys.includes(k))); }
function origin(value) {
  const u = new URL(value); must(u.protocol === 'https:' && !u.username && !u.password && u.pathname === '/' && !u.search && !u.hash); return u.origin;
}

// Kept byte-compatible with upload-server/timelens_upload/oss_grant.py.
export async function makeGrant(env, batch, deviceId, now, expires) {
  must(/^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$/.test(env.OSS_BUCKET || ''), 503);
  must(/^[A-Za-z0-9]{8,128}$/.test(env.OSS_ACCESS_KEY_ID || '') && env.OSS_ACCESS_KEY_SECRET, 503);
  const shared = batch.scope === 'shared';
  const prefix = `timelens/direct/v1/users/${batch.batchId}/devices/${shared ? '' : deviceId + '/'}`;
  const day = iso(now).slice(0,10).replaceAll('-', '');
  const fields = {'x-oss-signature-version':'OSS4-HMAC-SHA256',
    'x-oss-credential':`${env.OSS_ACCESS_KEY_ID}/${day}/cn-beijing/oss/aliyun_v4_request`,
    'x-oss-date':iso(now).replaceAll(/[-:]/g,'').replace(/\.\d+Z$/, 'Z'),
    'x-oss-object-acl':'private', 'x-oss-server-side-encryption':'AES256', 'x-oss-forbid-overwrite':'true',
    success_action_status:'204', 'Content-Type':'image/webp', 'x-oss-meta-timelens-user':batch.batchId,
    'x-oss-meta-timelens-device':shared ? '' : deviceId, 'x-oss-meta-timelens-study-end':iso(batch.plannedEndMs)};
  let key = enc.encode('aliyun_v4' + env.OSS_ACCESS_KEY_SECRET);
  for (const part of [day, 'cn-beijing', 'oss', 'aliyun_v4_request']) key = await hmac(key, part);
  async function signed(type, scope) {
    const form = {...fields, 'Content-Type':type};
    const conditions = [{bucket:env.OSS_BUCKET}, ...Object.entries(form).filter(([k]) => !(shared && k === 'x-oss-meta-timelens-device')).map(([k,v]) => ({[k]:v})),
      ['starts-with','$key',scope], ['content-length-range',1,20*1024*1024],
      ...['sha256','task','captured-at','display'].map(k => ['starts-with',`$x-oss-meta-timelens-${k}`,''])];
    if (shared) conditions.push(['starts-with','$x-oss-meta-timelens-device','']);
    const policy = btoa(JSON.stringify({expiration:iso(expires),conditions}));
    return {...form, policy, 'x-oss-signature':hex(await hmac(key, policy))};
  }
  return {format:shared?'timelens-oss-shared-post-v1':'timelens-oss-post-v1',deviceId:shared?'*':deviceId,userId:batch.batchId,
    bucket:env.OSS_BUCKET,region:'cn-beijing',endpoint:`https://${env.OSS_BUCKET}.oss-cn-beijing.aliyuncs.com/`,prefix,
    issuedAt:iso(now),expiresAt:iso(expires),archiveEndAt:iso(batch.plannedEndMs),
    fields:await signed('image/webp',prefix),manifestFields:await signed('application/json',shared?prefix:prefix+'manifests/')};
}

export class Issuer {
  constructor(ctx, env) { this.ctx = ctx; this.env = env; }
  async fetch(req) {
    try {
      const publicConfig=req.method==='GET' && new URL(req.url).pathname==='/v1/config';
      must(req.method==='POST'||publicConfig,404);
      // Read the bounded request BEFORE locking persistent state. Slow anonymous
      // bodies cannot monopolize all device renewals.
      const v=publicConfig?{}:await body(req);
      // Catch inside the callback: a rejected blockConcurrencyWhile callback
      // resets the Durable Object, even if its caller catches the rejection.
      return await this.ctx.blockConcurrencyWhile(async()=>{
        try {return await this.route(req,v);}
        catch(e) {return json({error:'RequestRejected'},[400,401,403,404,409,410,413,415,429,503].includes(e.status)?e.status:503);}
      });
    } catch (e) { return json({error:'RequestRejected'}, [400,401,403,404,409,410,413,415,429,503].includes(e.status)?e.status:503); }
  }
  async admin(req, sync = false) {
    const token = req.headers.get('authorization')?.replace(/^Bearer /,'');
    must(secret(token) && (await hash(token) === await hash(this.env.ISSUER_ADMIN_TOKEN || '') || sync && await hash(token) === await hash(this.env.ISSUER_SYNC_TOKEN || '')), 401);
  }
  async issue(batch, device, now, force = false) {
    must(!batch.disabled && !device.disabled, 403);
    const ceiling = Math.min(batch.deadlineMs, device.deadlineMs) - DRAIN;
    must(ceiling > now + 60000, 410);
    // Valid grants are reused. Force refresh is throttled to once per minute.
    if (device.grant && Date.parse(device.grant.expiresAt) <= ceiling && Date.parse(device.grant.expiresAt) > now && (now < device.issuedMs + 60000 || !force && Date.parse(device.grant.expiresAt) > now + DAY)) return device.grant;
    const expires = Math.min(now + 7*DAY, ceiling);
    const grant = await makeGrant(this.env, {...batch,plannedEndMs:device.plannedEndMs??batch.plannedEndMs}, device.deviceId, now, expires);
    device.grant = grant; device.issuedMs = now; device.lastGrantExpiresMs = Math.max(device.lastGrantExpiresMs || 0, expires);
    // Persist expiry BEFORE sending a usable grant. A lost reply can be retried safely.
    await this.ctx.storage.put('device:'+device.deviceId, device);
    return grant;
  }
  async route(req,v) {
    const route = new URL(req.url).pathname, now = Math.floor(Date.now()/1000)*1000, s = this.ctx.storage;
    if (req.method==='GET' && route==='/v1/config') {
      const batch=await s.get('batch:'+this.env.OPEN_REGISTRATION_BATCH);
      must(batch && !batch.disabled && batch.scope==='device',503);
      const days=bounded(Number(this.env.OPEN_REGISTRATION_WINDOW_DAYS||7),1,30);
      return json({format:'timelens-cloudflare-registration-v1',issuerUrl:origin(this.env.PUBLIC_ISSUER_URL)+'/',batchId:batch.batchId,scope:'device',bucket:this.env.OSS_BUCKET,studyWindowDays:days});
    }
    if (route==='/v1/register') {
      exact(v,['batchId','participantName','deviceId','deviceSecret','archiveEndAt']);
      must(id(v.batchId) && uuid(v.deviceId) && secret(v.deviceSecret));
      const declared=typeof v.participantName==='string'?v.participantName:'';
      const name=declared.trim();
      must(name && [...name].length<=64 && !['.','..'].includes(name) && !/[\\/\p{Cc}\u200b-\u200f\u202a-\u202e\u2060-\u206f\ufeff]/u.test(declared));
      const plan=date(v.archiveEndAt),digest=await hash(v.deviceSecret);
      let device=await s.get('device:'+v.deviceId);
      const batch=await s.get('batch:'+v.batchId);
      must(batch && !batch.disabled && batch.scope==='device',403);
      if(device) {
        // Existing identities may recover a lost reply after admission closes.
        // Never reset the original deadline, private key or device count.
        must(device.batchId===v.batchId && device.secretHash===digest && (device.plannedEndMs??batch.plannedEndMs)===plan,409);
      } else {
        must(v.batchId===this.env.OPEN_REGISTRATION_BATCH && now<batch.activationEndMs && batch.count<batch.maxDevices,403);
        const days=bounded(Number(this.env.OPEN_REGISTRATION_WINDOW_DAYS||7),1,30);
        // A small positive clock skew is tolerated; old offline tasks still
        // need a live retention window. The submitted end is fixed forever.
        must(plan<=now+days*DAY+120000 && plan+RETENTION>now+DRAIN+60000);
        const count=await s.get('device-count')||0;must(count<2000,409);
        device={deviceId:v.deviceId,batchId:v.batchId,secretHash:digest,plannedEndMs:plan,deadlineMs:Math.min(batch.deadlineMs,plan+RETENTION),lastGrantExpiresMs:0,disabled:false};
        batch.count++;
        await s.put({['batch:'+v.batchId]:batch,['device:'+v.deviceId]:device,'device-count':count+1});
      }
      // Name is a self-declared display field, not authentication. It is not
      // retained by this signer; the client freezes it into its OSS records.
      return json({grant:await this.issue(batch,device,now)});
    }
    if (route === '/admin/batches') {
      await this.admin(req); exact(v,['batchId','archiveEndAt','scope','maxDevices','activationExpiresAt','activationCode']);
      const publicOrigin=origin(this.env.PUBLIC_ISSUER_URL);
      must(id(v.batchId) && ['device','shared'].includes(v.scope));
      const plan = date(v.archiveEndAt), activationEnd = date(v.activationExpiresAt);
      must(plan > now && activationEnd > now && activationEnd <= plan);
      const max = bounded(v.maxDevices,1,1000);must(v.activationCode===undefined || secret(v.activationCode));
      const code=v.activationCode||random(),activationHash=await hash(code);
      const existing=await s.get('batch:'+v.batchId);
      if (existing) {must(existing.scope===v.scope && existing.plannedEndMs===plan && existing.maxDevices===max && existing.activationEndMs===activationEnd && existing.activationHash===activationHash,409);}
      else {
        must((await s.list({prefix:'batch:',limit:201})).size < 200,409);
        const batch={batchId:v.batchId,scope:v.scope,plannedEndMs:plan,deadlineMs:plan+RETENTION,maxDevices:max,activationEndMs:activationEnd,count:0,disabled:false,activationHash};
        await s.put({['batch:'+v.batchId]:batch,['activation:'+activationHash]:v.batchId});
      }
      return json({format:'timelens-cloudflare-activation-v1',issuerUrl:publicOrigin+'/',activationCode:code,
        batchId:v.batchId,scope:v.scope,bucket:this.env.OSS_BUCKET,archiveEndAt:iso(plan)},201);
    }
    if (route === '/v1/activate') {
      exact(v,['activationCode','deviceId','deviceSecret']); must(secret(v.activationCode) && uuid(v.deviceId) && secret(v.deviceSecret));
      const batchId = await s.get('activation:'+await hash(v.activationCode)); must(batchId,403);
      const batch = await s.get('batch:'+batchId); must(batch && !batch.disabled,403);
      let device = await s.get('device:'+v.deviceId); const digest = await hash(v.deviceSecret);
      if (device) { must(device.batchId === batchId && device.secretHash === digest,409); }
      else {
        must(now < batch.activationEndMs && batch.count < batch.maxDevices,403);
        const count = await s.get('device-count') || 0; must(count < 2000,409);
        device = {deviceId:v.deviceId,batchId,secretHash:digest,deadlineMs:batch.deadlineMs,lastGrantExpiresMs:0,disabled:false};
        batch.count++;
        await s.put({['batch:'+batchId]:batch,['device:'+v.deviceId]:device,'device-count':count+1});
      }
      return json({grant:await this.issue(batch,device,now)});
    }
    if (route === '/v1/renew') {
      exact(v,['deviceId','force']); must(uuid(v.deviceId) && (v.force === undefined || typeof v.force === 'boolean'));
      const device = await s.get('device:'+v.deviceId), token = req.headers.get('authorization')?.replace(/^Bearer /,'');
      must(device && secret(token) && device.secretHash === await hash(token),401);
      const batch = await s.get('batch:'+device.batchId); must(batch,403);
      return json({grant:await this.issue(batch,device,now,v.force)});
    }
    if (route === '/admin/device') {
      await this.admin(req); exact(v,['deviceId','disabled']); must(uuid(v.deviceId) && typeof v.disabled === 'boolean');
      const device = await s.get('device:'+v.deviceId); must(device,404); device.disabled = v.disabled; await s.put('device:'+v.deviceId,device); return json({ok:true});
    }
    if (route === '/admin/sync') {
      await this.admin(req,true); exact(v,['deadlines']); must(Array.isArray(v.deadlines) && v.deadlines.length <= 2000);
      // Validate the full request before making monotonic updates.
      for (const d of v.deadlines) { exact(d,['deviceId','deadlineMs']); must(uuid(d.deviceId)); bounded(d.deadlineMs,1,4102444800000); }
      const devices = await s.list({prefix:'device:',limit:2001}); must(devices.size <= 2000,503);
      const batches = await s.list({prefix:'batch:',limit:201}); const updates = {};
      for (const d of v.deadlines) {
        const device = devices.get('device:'+d.deviceId); must(device,409);
        const batch = batches.get('batch:'+device.batchId); must(batch,503);
        device.deadlineMs = Math.min(device.deadlineMs,d.deadlineMs); updates['device:'+d.deviceId]=device;
        if (batch.scope === 'shared') { batch.deadlineMs = Math.min(batch.deadlineMs,d.deadlineMs); updates['batch:'+device.batchId]=batch; }
      }
      const entries = Object.entries(updates);
      for (let i=0;i<entries.length;i+=128) await s.put(Object.fromEntries(entries.slice(i,i+128)));
      return json({format:'timelens-issuer-ledger-v1',atMs:now,devices:[...devices.values()].map(d => {
        const b = batches.get('batch:'+d.batchId);
        return {deviceId:d.deviceId,userId:d.batchId,bucket:this.env.OSS_BUCKET,scope:b.scope,
          plannedEndMs:d.plannedEndMs??b.plannedEndMs,deadlineMs:Math.min(b.deadlineMs,d.deadlineMs),lastGrantExpiresMs:d.lastGrantExpiresMs};
      })});
    }
    must(false,404);
  }
}

export default {
  async fetch(req, env) {
    // Every public route reaches the same serialized persistent issuer.
    const stub = env.ISSUER.get(env.ISSUER.idFromName('timelens-issuer-v1'));
    return stub.fetch(req);
  }
};
