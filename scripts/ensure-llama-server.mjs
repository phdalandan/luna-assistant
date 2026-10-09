// Builds the bundled llama-server when it is missing or the pinned version changed.
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const buildScript = join(root, "scripts", "build-llama-server.sh");
const pinnedTag = readFileSync(buildScript, "utf8").match(
  /^LLAMA_CPP_TAG="(.+)"$/m,
)?.[1];
if (!pinnedTag) {
  console.error("Could not read LLAMA_CPP_TAG from build-llama-server.sh.");
  process.exit(1);
}

const target = execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(
  /^host: (.+)$/m,
)?.[1];
if (!target) {
  console.error("Could not determine the Rust target triple.");
  process.exit(1);
}

const binaries = join(root, "src-tauri", "binaries");
const extension = process.platform === "win32" ? ".exe" : "";
const binary = join(binaries, `llama-server-${target}${extension}`);
const stamp = join(binaries, `llama-server-${target}.version`);
const builtTag = existsSync(stamp) ? readFileSync(stamp, "utf8").trim() : null;

if (existsSync(binary) && builtTag === pinnedTag) {
  process.exit(0);
}

console.log(
  `Building llama-server ${pinnedTag} for ${target}. This runs once.`,
);
const result = spawnSync("bash", [buildScript, target], { stdio: "inherit" });
if (result.error) {
  console.error(`Could not run bash: ${result.error.message}`);
}
process.exit(result.status ?? 1);
