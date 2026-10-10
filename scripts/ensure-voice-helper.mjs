// Builds the GPL voice helper (voice-helper/) when it is missing or its sources changed, and
// copies it to src-tauri/binaries as a Tauri sidecar. Run after fetch-sherpa-onnx.mjs.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const helper = join(root, "voice-helper");
const target =
  process.argv[2] ??
  execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(
    /^host: (.+)$/m,
  )?.[1];
if (!target) {
  console.error("Could not determine the Rust target triple.");
  process.exit(1);
}

const sources = [
  "Cargo.toml",
  "Cargo.lock",
  "build.rs",
  ".cargo/config.toml",
  ...readdirSync(join(helper, "src")).map((name) => `src/${name}`),
];
const hash = createHash("sha256");
for (const source of sources.sort()) {
  hash.update(source).update(readFileSync(join(helper, source)));
}
const version = hash.digest("hex");

const extension = target.includes("windows") ? ".exe" : "";
const binaries = join(root, "src-tauri", "binaries");
const binary = join(binaries, `luna-voice-${target}${extension}`);
const stamp = join(binaries, `luna-voice-${target}.version`);
const built = existsSync(stamp) ? readFileSync(stamp, "utf8").trim() : null;
if (existsSync(binary) && built === version) {
  process.exit(0);
}

console.log(`Building the voice helper for ${target}.`);
// Cargo reads voice-helper/.cargo/config.toml only when run from that directory.
execFileSync("cargo", ["build", "--release", "--locked", "--target", target], {
  cwd: helper,
  stdio: "inherit",
});
mkdirSync(binaries, { recursive: true });
copyFileSync(
  join(helper, "target", target, "release", `luna-voice${extension}`),
  binary,
);
writeFileSync(stamp, version);
