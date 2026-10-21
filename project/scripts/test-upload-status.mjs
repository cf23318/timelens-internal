import assert from "node:assert/strict";
import { build } from "esbuild";
import { fileURLToPath } from "node:url";

const result = await build({ entryPoints: [fileURLToPath(new URL("../src/lib/taskStatus.ts", import.meta.url))], bundle: true, write: false, platform: "node", format: "esm" });
const { taskOverviewLabel, directFramesAccepted } = await import(`data:text/javascript;base64,${Buffer.from(result.outputFiles[0].text).toString("base64")}`);
const complete = {
  config: { uploadMode: "ossDirect" },
  progress: { status: "completed", endedAtMs: 100, needsFlush: false, integrityError: null, lastError: null, receipt: null, storedBundle: null, bundlePath: null },
  snapshots: { count: 16 },
  uploads: { pending: 0, blocked: 0, legacy: 0, ossAccepted: 16 },
  uploadActive: false, archive: null,
};
let checks = 0;
function check(name, mutate, expected, accepted = false, english = false) {
  const data = structuredClone(complete);
  mutate(data);
  assert.equal(taskOverviewLabel(data, english, 1000), expected, name);
  assert.equal(directFramesAccepted(data), accepted, `${name}: receipt completeness`);
  checks++;
}
check("all accepted and worker stopped", () => {}, "OSS已接收 · 待核验", true);
check("English accepted", () => {}, "OSS accepted · Verification pending", true, true);
check("all accepted while final manifest is in flight", d => { d.uploadActive = true; }, "OSS已接收 · 同步清单中", true);
check("manifest retry keeps acceptance distinct", d => { d.uploadActive = true; d.progress.lastError = "清单上传连接失败，将重试"; }, "OSS已接收 · 清单重试中", true);
check("manifest rejection needs attention", d => { d.progress.lastError = "清单上传 HTTP 403，未确认成功"; }, "OSS已接收 · 需处理", true);
check("partial receipts never mean sent", d => { d.uploads.ossAccepted = 15; d.uploads.pending = 1; }, "已结束 · 待上传");
check("partial background upload", d => { d.uploads.ossAccepted = 15; d.uploads.pending = 1; d.uploadActive = true; }, "已结束 · 上传中");
check("offline background retry", d => { d.uploads.ossAccepted = 15; d.uploads.pending = 1; d.uploadActive = true; d.progress.lastError = "OSS上传连接失败"; }, "已结束 · 自动重试中");
check("conflicting receipt needs review", d => { d.uploads.ossAccepted = 15; d.uploads.blocked = 1; }, "已结束 · 上传需处理");
check("inconsistent counts never mean sent", d => { d.uploads.ossAccepted = 15; }, "已结束 · 待上传");
check("unflushed writes never mean sent", d => { d.progress.needsFlush = true; }, "已结束 · 待上传");
check("integrity errors never mean sent", d => { d.progress.integrityError = "pending review"; }, "已结束 · 待上传");
check("legacy pictures excluded", d => { d.uploads.legacy = 1; }, "已结束 · 待上传");
check("legacy receipt excluded", d => { d.progress.receipt = {}; }, "已结束 · 待上传");
check("zero pictures cannot claim acceptance", d => { d.snapshots.count = 0; d.uploads.ossAccepted = 0; }, "已结束 · 待上传");
check("recording remains recording", d => { d.progress.status = "recording"; d.progress.endedAtMs = null; }, "采集中");
check("paused remains paused", d => { d.progress.status = "paused"; d.progress.endedAtMs = null; }, "已暂停");
check("receiver mode is unchanged", d => { d.config.uploadMode = "receiver"; }, "已结束 · 待上传");
check("reconciled takes precedence", d => { d.progress.status = "uploaded"; }, "归档清单已对账");
check("expired takes precedence", d => { d.archive = { deleted: false, deadlineMs: 1000 }; }, "已到期 · 等待清理", true);
check("deleted takes precedence", d => { d.archive = { deleted: true, deadlineMs: 500 }; }, "已到期清理", true);
console.log(`Upload status regression: ${checks} cases passed.`);
