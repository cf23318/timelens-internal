// Package only the compiled app, with source identity and checksums; never user data.
import { mkdir, readdir, readFile, copyFile, writeFile, stat, rm } from "node:fs/promises";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const project = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const platform = process.argv[2];
if (!["windows", "macos"].includes(platform)) throw new Error("Expected windows or macos");
const sourceCommit = execFileSync("git", ["rev-parse", "HEAD"], { cwd: project, encoding: "utf8" }).trim();
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
await writeFile(path.join(output, "BUILD-INFO.json"), JSON.stringify({
  version, sourceCommit, platform, architecture: process.arch,
  runUrl: process.env.GITHUB_SERVER_URL && `${process.env.GITHUB_SERVER_URL}/${process.env.GITHUB_REPOSITORY}/actions/runs/${process.env.GITHUB_RUN_ID}`,
  builtAt: new Date().toISOString(),
  acceptance: "CI tests and packaging only; desktop capture requires separate authorized machine testing.", files,
}, null, 2) + "\n");
await writeFile(path.join(output, "SHA256SUMS"), files.map(file => `${file.sha256}  ${file.name}`).join("\n") + "\n");
console.log(`Packaged ${files.length} binaries for ${platform}, source ${sourceCommit}`);
