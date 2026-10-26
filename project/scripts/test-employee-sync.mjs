import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { build } from 'esbuild';
import { fileURLToPath } from 'node:url';
const built = await build({entryPoints:[fileURLToPath(new URL('../src/lib/employeeSyncIssue.ts',import.meta.url))],bundle:true,write:false,platform:'node',format:'esm'});
const {employeeSyncIssue,describeSyncFailure}=await import(`data:text/javascript;base64,${Buffer.from(built.outputFiles[0].text).toString('base64')}`);
const baseline={config:{participantName:'cf',autoUploadEnabled:true},progress:{lastError:null,integrityError:null},uploads:{lastError:null,blocked:0,pending:25,ossAccepted:0},cloudIssuer:{lastError:null,lastCheckedMs:1},ossGrant:{valid:false},archive:null};
let checks=0;
function check(name,patch,contains,tone='warning',english=false){const data=structuredClone(baseline);patch(data);const result=employeeSyncIssue(data,english,1000);if(contains===null)assert.equal(result,null,name);else {assert.ok((result.title+' '+result.detail).includes(contains),name);assert.equal(result.tone,tone,name);}checks++;return result;}
check('healthy has no failure',()=>{},null);
check('authorization timeout explains proxy',d=>d.cloudIssuer.lastError='云端授权连接超时','系统代理');
check('connection failure explains proxy',d=>d.cloudIssuer.lastError='云端授权连接失败','代理程序');
check('read failure retries',d=>d.cloudIssuer.lastError='云端授权响应读取失败','系统代理');
check('valid grant keeps uploads running during renewal failure',d=>{d.cloudIssuer.lastError='云端授权连接超时';d.ossGrant.valid=true;},'截图可继续上传','info');
for(const code of [401,403,410])check('HTTP '+code,d=>d.cloudIssuer.lastError=`云端授权返回 HTTP ${code}`,'调研期限');
check('conflict never suggests reset',d=>d.cloudIssuer.lastError='HTTP 409','勿重复注册');
check('proxy authentication',d=>d.cloudIssuer.lastError='HTTP 407','代理登录');
check('rate limit retries',d=>d.cloudIssuer.lastError='HTTP 429','自动重试');
for(const code of [408,500,503])check('transient '+code,d=>d.cloudIssuer.lastError=`HTTP ${code}`,'自动重试');
check('bad config',d=>d.cloudIssuer.lastError='HTTP 400','管理员');
check('upload failure uses direct storage advice',d=>{d.uploads.lastError='OSS上传连接失败';d.cloudIssuer.lastError='云端授权连接超时';d.ossGrant.valid=true;},'直连存储服务');
check('manifest error surfaced',d=>d.progress.lastError='清单上传 HTTP 503','清单上传');
check('integrity takes precedence',d=>{d.progress.integrityError='must-not-display';d.cloudIssuer.lastError='HTTP 403';},'完整性');
check('blocked frames without reason need review',d=>d.uploads.blocked=1,'上传状态');
check('blocked authorization rejection stays specific',d=>{d.uploads.blocked=1;d.uploads.lastError='OSS上传 HTTP 403';},'HTTP 403');
check('blocked conflict stays specific',d=>{d.uploads.blocked=1;d.uploads.lastError='OSS上传 HTTP 409';},'冲突');
const authRetry=check('OSS403 renewal timeout keeps proxy diagnosis',d=>{d.progress.lastError='云端续期暂不可用，当前OSS上传已暂停等待新授权';d.cloudIssuer.lastError='云端授权连接超时';d.ossGrant.valid=true;},'系统代理');
assert.ok(!authRetry.detail.includes('截图可继续上传'));
check('authorization retry without cloud detail remains explicit',d=>d.progress.lastError='云端续期暂不可用，当前OSS上传已暂停等待新授权','等待新授权');
check('expiry takes precedence',d=>{d.archive={deadlineMs:1000,deleted:false};d.cloudIssuer.lastError='HTTP 503';},'任务已到期');
check('English timeout',d=>d.cloudIssuer.lastError='云端授权连接超时','system proxy','warning',true);
for(const error of ['https://private-bucket.oss-cn-beijing.aliyuncs.com/?token=must-not-display','代理失败 user:must-not-display@private-proxy','HTTP 403 body=must-not-display','未知设备 /Users/person/must-not-display']) {
 const result=describeSyncFailure(error);assert.ok(!JSON.stringify(result).includes('must-not-display'));assert.ok(!JSON.stringify(result).includes('aliyuncs.com'));checks++;
}
const config=JSON.parse(await fs.readFile(new URL('../config/employee-registration.json',import.meta.url),'utf8'));
assert.equal(config.format,'timelens-cloudflare-discovery-v1');assert.deepEqual(Object.keys(config).sort(),['format','issuerUrl']);checks++;
for(const file of ['../src/components/OssUploadSettings.tsx','../src/components/UploadConfirmDialog.tsx']){
 const source=await fs.readFile(new URL(file,import.meta.url),'utf8');assert.ok(!/\{(?:grant\.(?:endpoint|prefix)|confirmation\.uploadUrl\s*\|\||original\.ossGrant\?\.prefix)/.test(source));checks++;
}
console.log(`Employee sync/privacy regression: ${checks} cases passed.`);
// Render the real component with synthetic store/locale inputs, including error
// visibility and redaction. No native API, user profile or cloud data is used.
const ui=await build({stdin:{contents:`import React from 'react';import {renderToStaticMarkup} from 'react-dom/server';import {EmployeeSyncStatus} from './src/components/EmployeeSetup';export function render(data,english=false){globalThis.__timelensSyncFixture=data;globalThis.__timelensSyncEnglish=english;return renderToStaticMarkup(React.createElement(EmployeeSyncStatus));}`,resolveDir:fileURLToPath(new URL('..',import.meta.url)),loader:'tsx'},bundle:true,write:false,platform:'node',format:'cjs',jsx:'automatic',plugins:[{name:'synthetic-ui-context',setup(b){b.onResolve({filter:/\/stores\/internalStore$/},()=>({path:'store',namespace:'fixture'}));b.onResolve({filter:/\/lib\/desktop$/},()=>({path:'locale',namespace:'fixture'}));b.onLoad({filter:/.*/,namespace:'fixture'},args=>({contents:args.path==='store'?`export const useInternalStore=()=>({data:globalThis.__timelensSyncFixture,busy:false,act:async()=>true});useInternalStore.setState=()=>{};`:`export function friendlyError(){return "fixture-only";}export function useDesktopText(){const english=globalThis.__timelensSyncEnglish;return {english,text:(zh,en)=>english?en:zh};}`,loader:'js'}));}}]});
const {createRequire}=await import('node:module');const module={exports:{}};new Function('require','module','exports',ui.outputFiles[0].text)(createRequire(import.meta.url),module,module.exports);
const fixture=structuredClone(baseline);fixture.cloudIssuer.lastError='云端授权连接超时 https://private-bucket.invalid/?token=must-not-display';
let html=module.exports.render(fixture);assert.ok(html.includes('连接超时')&&html.includes('role="status"')&&html.includes('系统代理'));assert.ok(!html.includes('must-not-display')&&!html.includes('private-bucket'));checks++;
html=module.exports.render(fixture,true);assert.ok(html.includes('timed out')&&html.includes('system proxy'));checks++;
fixture.config.autoUploadEnabled=false;html=module.exports.render(fixture);assert.ok(html.includes('自动同步已暂停')&&!html.includes('连接超时'));checks++;
console.log(`Employee component rendering: 3 cases passed (${checks} total checks).`);
