// Package only the compiled app, with source identity and checksums; never user data.
import { mkdir, readdir, readFile, copyFile, writeFile, stat, rm } from "node:fs/promises";
import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const project = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const platform = process.argv[2];
if (!["windows", "macos"].includes(platform)) throw new Error("Expected windows or macos");
const sourceCommit = execFileSync("git", ["rev-parse", "HEAD"], { cwd: project, encoding: "utf8" }).trim();
const sourceDirty = !!execFileSync("git", ["status", "--porcelain"], { cwd: project, encoding: "utf8" }).trim();
const repository = execFileSync("git", ["rev-parse", "--show-toplevel"], { cwd: project, encoding: "utf8" }).trim();
const sourcePaths = [...new Set(execFileSync("git", ["ls-files", "--cached", "--others", "--exclude-standard", "-z"], { cwd: repository, encoding: "utf8" }).split("\0").filter(Boolean))].sort();
const sourceHash = createHash("sha256");
for (const name of sourcePaths) {
  sourceHash.update(name + "\0");
  try { sourceHash.update(await readFile(path.join(repository, name))); }
  catch (error) { if (error.code !== "ENOENT") throw error; sourceHash.update("DELETED"); }
  sourceHash.update("\0");
}
const sourceTreeSha256 = sourceHash.digest("hex");
const { version } = JSON.parse(await readFile(path.join(project, "package.json"), "utf8"));
const config = JSON.parse(await readFile(path.join(project, "src-tauri", "tauri.conf.json"), "utf8"));
if (config.version !== version) throw new Error("Frontend and bundle versions differ");
const release = path.join(project, "src-tauri", "target", "release");
const output = path.join(project, "src-tauri", "target", "distribution", platform);
await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });
const files = [];
async function include(source, name = path.basename(source)) {
  const destination = path.join(output, name);
  await copyFile(source, destination);
  const bytes = await readFile(destination);
  files.push({ name, sizeBytes: (await stat(destination)).size, sha256: createHash("sha256").update(bytes).digest("hex") });
}
const bundle = path.join(release, "bundle", platform === "windows" ? "nsis" : "dmg");
const packages = (await readdir(bundle)).filter(name => name.includes(`_${version}_`) && (platform === "windows" ? name.endsWith("-setup.exe") : name.endsWith(".dmg")));
if (packages.length !== 1) throw new Error(`Expected one current installer, found ${packages.length}`);
await include(path.join(bundle, packages[0]));
if (platform === "windows") await include(path.join(release, "timelens.exe"), `TimeLens-${version}-windows-x64.exe`);
let signing;
if (platform === "macos") {
  const app = path.join(release, "bundle", "macos", `${config.productName}.app`);
  const bundleVersion = execFileSync("plutil", ["-extract", "CFBundleShortVersionString", "raw", path.join(app, "Contents", "Info.plist")], { encoding: "utf8" }).trim();
  if (bundleVersion !== version) throw new Error("Mac app bundle version differs from source");
  const details = spawnSync("codesign", ["-dv", "--verbose=4", app], { encoding: "utf8" });
  const info = details.stderr || "";
  const verify = spawnSync("codesign", ["--verify", "--deep", "--strict", app], { encoding: "utf8" });
  if (verify.status !== 0) throw new Error("Mac app signature integrity validation failed; do not distribute this bundle");
  const staple = spawnSync("xcrun", ["stapler", "validate", app], { encoding: "utf8" });
  signing = { type: info.includes("Signature=adhoc") ? "ad-hoc" : details.status === 0 ? "certificate" : "unsigned",
    verified: verify.status === 0, notarizationTicketStapled: staple.status === 0,
    developerId: info.includes("Authority=Developer ID Application:"),
    teamIdentifier: info.match(/^TeamIdentifier=(.+)$/m)?.[1] || null };
} else {
  signing = [];
  for (const file of files) {
    const signature = JSON.parse(execFileSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command",
      "Get-AuthenticodeSignature -LiteralPath $env:TIMELENS_SIGN_CHECK | Select-Object @{Name='Status';Expression={$_.Status.ToString()}},StatusMessage | ConvertTo-Json -Compress"],
      { env: { ...process.env, TIMELENS_SIGN_CHECK: path.join(output, file.name) }, encoding: "utf8" }));
    signing.push({ name: file.name, authenticodeStatus: signature.Status, statusMessage: signature.StatusMessage });
  }
}
await writeFile(path.join(output, "INTERNAL-TEST-PACKAGE.txt"), "Internal test package. No employee data or upload grants included.\nNo Developer ID notarization or Windows signing certificate is configured; inspect BUILD-INFO.json.\nDo not treat this build as a signed production release.\n");
await writeFile(path.join(output, "BUILD-INFO.json"), JSON.stringify({
  version, sourceCommit, sourceDirty, sourceTreeSha256, platform, architecture: process.arch, signing,
  runUrl: process.env.GITHUB_SERVER_URL && `${process.env.GITHUB_SERVER_URL}/${process.env.GITHUB_REPOSITORY}/actions/runs/${process.env.GITHUB_RUN_ID}`,
  builtAt: new Date().toISOString(),
  acceptance: "CI tests and packaging only; desktop capture requires separate authorized machine testing.", files,
}, null, 2) + "\n");
await writeFile(path.join(output, "SHA256SUMS"), files.map(file => `${file.sha256}  ${file.name}`).join("\n") + "\n");
console.log(`Packaged ${files.length} binaries for ${platform}, source ${sourceCommit}`);
