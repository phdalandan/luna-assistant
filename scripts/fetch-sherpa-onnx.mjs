// Downloads the pinned sherpa-onnx static libraries, verifies their SHA-256, and extracts them
// to src-tauri/target/sherpa-onnx/lib. These "no-tts" builds leave out sherpa-onnx's own speech
// synthesis, which would link GPL-licensed espeak-ng. `.cargo/config.toml` links these libraries
// in place of the sherpa-onnx-sys build script, which would download unverified archives.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// Must match the exact sherpa-onnx version in src-tauri/Cargo.toml.
const VERSION = "1.13.8";
const ARCHIVES = {
  "x86_64-pc-windows-msvc": {
    name: `sherpa-onnx-v${VERSION}-win-x64-static-MD-Release-no-tts-lib`,
    sha256: "542348e56e827b59c6d249fd0dfd38dc34b7bd0c521a1ebe9eb6e2db154fa15d",
  },
  "aarch64-apple-darwin": {
    name: `sherpa-onnx-v${VERSION}-osx-arm64-static-no-tts-lib`,
    sha256: "3d7f9b8a496694af13d9802c33b8133231e397bdef302f543d19468765e83136",
  },
};

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const target =
  process.argv[2] ??
  execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(
    /^host: (.+)$/m,
  )?.[1];
const archive = ARCHIVES[target];
if (!archive) {
  console.error(`No pinned sherpa-onnx libraries for ${target}.`);
  process.exit(1);
}

const directory = join(root, "src-tauri", "target", "sherpa-onnx");
const stamp = join(directory, "version");
if (
  existsSync(stamp) &&
  readFileSync(stamp, "utf8").trim() === archive.sha256
) {
  process.exit(0);
}

const url = `https://github.com/k2-fsa/sherpa-onnx/releases/download/v${VERSION}/${archive.name}.tar.bz2`;
console.log(`Downloading sherpa-onnx ${VERSION} libraries for ${target}.`);
const response = await fetch(url);
if (!response.ok) {
  console.error(`Download failed: ${response.status} ${url}`);
  process.exit(1);
}
const bytes = Buffer.from(await response.arrayBuffer());
const actual = createHash("sha256").update(bytes).digest("hex");
if (actual !== archive.sha256) {
  console.error(`Checksum mismatch for ${archive.name}.`);
  process.exit(1);
}

rmSync(directory, { recursive: true, force: true });
mkdirSync(directory, { recursive: true });
const file = join(directory, `${archive.name}.tar.bz2`);
writeFileSync(file, bytes);
// Only the lib directory is extracted, without the archive's top-level folder.
// Relative paths, because GNU tar reads "C:" in a path as a remote host.
execFileSync(
  "tar",
  [
    "-xjf",
    `${archive.name}.tar.bz2`,
    "--strip-components=1",
    `${archive.name}/lib`,
  ],
  { cwd: directory, stdio: "inherit" },
);
rmSync(file);
writeFileSync(stamp, archive.sha256);
