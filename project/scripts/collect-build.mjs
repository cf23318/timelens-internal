// Package only the compiled app, with source identity and checksums; never user data.
import { mkdir, readdir, readFile, copyFile, writeFile, stat, rm, mkdtemp, lstat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const project = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const platform = process.argv[2];
if (!["windows", "macos"].includes(platform)) throw new Error("Expected windows or macos");
const sourceCommit = execFileSync("git", ["rev-parse", "HEAD"], { cwd: project, encoding: "utf8" }).trim();
const sourceChanges = execFileSync("git", ["status", "--porcelain", "--untracked-files=all"], { cwd: project, encoding: "utf8" }).split(/\r?\n/).filter(Boolean);
const sourceDirty = sourceChanges.length > 0;
const sourceDiff = execFileSync("git", ["diff", "HEAD", "--binary"], { cwd: project, maxBuffer: 32 * 1024 * 1024 });
console.log(JSON.stringify({ sourceDirty, sourceChanges, trackedDiffBytes: sourceDiff.length }));
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
const buildProfile=process.env.TIMELENS_BUILD_PROFILE||"release";
if(!["debug","release"].includes(buildProfile))throw new Error("Invalid build profile");
const release = path.join(project, "src-tauri", "target", buildProfile);
async function inspectBinary(binary) {
  const bytes=await readFile(binary);
  // Never run an old app that does not implement the non-GUI inspection protocol.
  if(!bytes.includes(Buffer.from("timelens-build-inspection-v1")))throw new Error("Artifact lacks safe build inspection; refuse stale package");
  const inspection=await mkdtemp(path.join(tmpdir(),"timelens-build-inspection-"));
  try {
    const file=path.join(inspection,"build-info.json"),profile=path.join(inspection,"unused-profile");
    await mkdir(profile);
    execFileSync(binary,["--timelens-build-info-file",file],{timeout:10000,env:{...process.env,TIMELENS_PROFILE_DIR:profile}});
    const value=JSON.parse(await readFile(file,"utf8"));
    if(value.schema!==1 || value.buildInfoProtocol!=="timelens-build-inspection-v1" || typeof value.employeeMode!=="boolean" || typeof value.bootstrapCredentialEmbedded!=="boolean" || value.bootstrapCredentialEmbedded&&!value.employeeMode)throw new Error("Compiled deployment metadata is invalid");
    return value;
  } finally {await rm(inspection,{recursive:true,force:true});}
}
const deployment=await inspectBinary(path.join(release,platform==="windows"?"timelens.exe":"timelens"));
if(process.env.TIMELENS_PUBLIC_EMPLOYEE_BUILD==='1' && (!deployment.employeeMode || deployment.bootstrapCredentialEmbedded))throw new Error("Public employee packages must use credential-free self-registration");
if(process.env.TIMELENS_ACTIVATION_CONFIG) {
  const digest=createHash("sha256").update(await readFile(process.env.TIMELENS_ACTIVATION_CONFIG)).digest("hex");
  if(!deployment.employeeMode || deployment.presetFingerprint!==digest)throw new Error("Compiled activation preset differs from build input; refuse stale package");
} else if(deployment.employeeMode)throw new Error("Unexpected employee credentials in a generic package");
const artifactInspection=[];
async function sameDeployment(binary,component) {
  const value=await inspectBinary(binary);
  if(value.employeeMode!==deployment.employeeMode || value.bootstrapCredentialEmbedded!==deployment.bootstrapCredentialEmbedded || value.presetFingerprint!==deployment.presetFingerprint)throw new Error(`${component} activation preset differs from standalone binary; refuse stale installer`);
  artifactInspection.push({component,employeeMode:value.employeeMode,bootstrapCredentialEmbedded:value.bootstrapCredentialEmbedded,presetFingerprint:value.presetFingerprint});
}
const output = process.env.TIMELENS_DISTRIBUTION_DIR ? path.resolve(process.env.TIMELENS_DISTRIBUTION_DIR) : path.join(project, "src-tauri", "target", "distribution", platform);
if(process.env.TIMELENS_DISTRIBUTION_DIR) {
  try {
    if(!(await lstat(output)).isDirectory())throw new Error("Distribution output must be a real directory");
    const entries=await readdir(output);
    if(entries.length && !entries.includes(".timelens-build-output"))throw new Error("Refuse to replace a non-empty unmanaged distribution directory");
  }catch(error){if(error.code!=="ENOENT")throw error;}
}
await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });
await writeFile(path.join(output,".timelens-build-output"),"TimeLens generated build output\n");
if (sourceDiff.length) await writeFile(path.join(output, "SOURCE-DIFF.patch"), sourceDiff);
const files = [];
async function include(source, name = path.basename(source)) {
  const destination = path.join(output, name);
  await copyFile(source, destination);
  const bytes = await readFile(destination);
  files.push({ name, sizeBytes: (await stat(destination)).size, sha256: createHash("sha256").update(bytes).digest("hex") });
}
const bundleRoot=process.env.TIMELENS_BUNDLE_DIR?path.resolve(process.env.TIMELENS_BUNDLE_DIR):path.join(release,"bundle");
const bundle = path.join(bundleRoot, platform === "windows" ? "nsis" : "dmg");
const packages = (await readdir(bundle)).filter(name => name.includes(`_${version}_`) && (platform === "windows" ? name.endsWith("-setup.exe") : name.endsWith(".dmg")));
if (packages.length !== 1) throw new Error(`Expected one current installer, found ${packages.length}`);
await include(path.join(bundle, packages[0]));
if (platform === "windows") await include(path.join(release, "timelens.exe"), `TimeLens-${version}-windows-x64.exe`);
let signing;
if (platform === "macos") {
  const app = path.join(bundleRoot, "macos", `${config.productName}.app`);
  await sameDeployment(path.join(app,"Contents","MacOS","timelens"),"macos-app");
  const bundleVersion = execFileSync("plutil", ["-extract", "CFBundleShortVersionString", "raw", path.join(app, "Contents", "Info.plist")], { encoding: "utf8" }).trim();
  if (bundleVersion !== version) throw new Error("Mac app bundle version differs from source");
  const details = spawnSync("codesign", ["-dv", "--verbose=4", app], { encoding: "utf8" });
  const info = details.stderr || "";
  const verify = spawnSync("codesign", ["--verify", "--deep", "--strict", app], { encoding: "utf8" });
  if (verify.status !== 0) throw new Error("Mac app signature integrity validation failed; do not distribute this bundle");
  const mountRoot=await mkdtemp(path.join(tmpdir(),"timelens-installer-inspection-")),mount=path.join(mountRoot,"image");
  let attached=false;
  try {
    await mkdir(mount);
    execFileSync("hdiutil",["attach","-readonly","-nobrowse","-mountpoint",mount,path.join(bundle,packages[0])],{stdio:"pipe",timeout:60000});attached=true;
    const installedApp=path.join(mount,`${config.productName}.app`);
    const installedVersion=execFileSync("plutil",["-extract","CFBundleShortVersionString","raw",path.join(installedApp,"Contents","Info.plist")],{encoding:"utf8"}).trim();
    if(installedVersion!==version)throw new Error("DMG app version differs from source");
    await sameDeployment(path.join(installedApp,"Contents","MacOS","timelens"),"dmg");
  } finally {
    if(attached) {
      // macOS may briefly hold a newly inspected read-only image. Retry busy
      // unmounts; never force another process off a mounted volume.
      let detached=false,lastError;
      for(let attempt=0;attempt<6;attempt++) {
        const result=spawnSync("hdiutil",["detach",mount],{encoding:"utf8",timeout:10000});
        if(result.status===0){detached=true;break;}
        lastError=result.stderr||result.error?.message||"unknown detach failure";
        if(result.status!==16)break;
        await new Promise(resolve=>setTimeout(resolve,1000));
      }
      if(!detached)throw new Error(`Cannot detach this build's read-only inspection image: ${lastError}`);
    }
    await rm(mountRoot,{recursive:true,force:true});
  }
  const staple = spawnSync("xcrun", ["stapler", "validate", app], { encoding: "utf8" });
  signing = { type: info.includes("Signature=adhoc") ? "ad-hoc" : details.status === 0 ? "certificate" : "unsigned",
    verified: verify.status === 0, notarizationTicketStapled: staple.status === 0,
    developerId: info.includes("Authority=Developer ID Application:"),
    teamIdentifier: info.match(/^TeamIdentifier=(.+)$/m)?.[1] || null };
} else {
  const extracted=await mkdtemp(path.join(tmpdir(),"timelens-nsis-inspection-"));
  try {
    const candidates=["7z",path.join(process.env.ProgramFiles||"C:\\Program Files","7-Zip","7z.exe")];
    const sevenZip=candidates.find(p=>spawnSync(p,["i"],{stdio:"ignore"}).status===0);
    if(!sevenZip)throw new Error("7-Zip is required to verify the NSIS installer contents");
    execFileSync(sevenZip,["x",path.join(bundle,packages[0]),`-o${extracted}`,"-y"],{stdio:"pipe",timeout:60000});
    const matches=[];
    async function findApp(dir,depth=0){if(depth>10)throw new Error("Unexpected installer tree");for(const entry of await readdir(dir,{withFileTypes:true})){if(entry.isSymbolicLink())throw new Error("Unexpected installer symlink");const file=path.join(dir,entry.name);if(entry.isDirectory())await findApp(file,depth+1);else if(entry.name.toLowerCase()==="timelens.exe")matches.push(file);}}
    await findApp(extracted);if(matches.length!==1)throw new Error("Expected one main program in NSIS installer");
    await sameDeployment(matches[0],"nsis");
  } finally {await rm(extracted,{recursive:true,force:true});}
  signing = [];
  for (const file of files) {
    const signature = JSON.parse(execFileSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command",
      "Get-AuthenticodeSignature -LiteralPath $env:TIMELENS_SIGN_CHECK | Select-Object @{Name='Status';Expression={$_.Status.ToString()}},StatusMessage | ConvertTo-Json -Compress"],
      { env: { ...process.env, TIMELENS_SIGN_CHECK: path.join(output, file.name) }, encoding: "utf8" }));
    signing.push({ name: file.name, authenticodeStatus: signature.Status, statusMessage: signature.StatusMessage });
  }
}
await writeFile(path.join(output, "INTERNAL-TEST-PACKAGE.txt"), (deployment.bootstrapCredentialEmbedded ? "Employee-configured package contains a batch activation credential. Distribute internally; never publish it as a credential-free generic package.\n" : deployment.employeeMode ? "Employee self-registration package. Contains public service configuration only, no activation credential or OSS secret. Enter a name to register; no upload setup required.\n" : "Generic diagnostic package. No employee data or activation credentials included. Employee onboarding requires a configured build.\n")+"Inspect BUILD-INFO.json for source, preset and signature state. Do not treat an unsigned package as a signed production release.\n");
await writeFile(path.join(output, "BUILD-INFO.json"), JSON.stringify({
  version, sourceCommit, sourceDirty, sourceChanges, sourceDiffSha256: createHash("sha256").update(sourceDiff).digest("hex"), sourceTreeSha256, platform, architecture: process.arch, signing,
  runUrl: process.env.GITHUB_SERVER_URL && `${process.env.GITHUB_SERVER_URL}/${process.env.GITHUB_REPOSITORY}/actions/runs/${process.env.GITHUB_RUN_ID}`,
  builtAt: new Date().toISOString(), buildProfile, deployment, artifactInspection,
  acceptance: "CI tests and packaging only; desktop capture requires separate authorized machine testing.", files,
}, null, 2) + "\n");
await writeFile(path.join(output, "SHA256SUMS"), files.map(file => `${file.sha256}  ${file.name}`).join("\n") + "\n");
console.log(`Packaged ${files.length} binaries for ${platform}, source ${sourceCommit}`);
