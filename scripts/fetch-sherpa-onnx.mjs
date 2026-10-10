// Downloads and verifies the pinned sherpa-onnx libraries: "no-tts" for Luna (no GPL espeak-ng)
// and the full build for the GPL voice helper. Each is extracted where its .cargo/config.toml links it.
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

// Must match the exact sherpa-onnx version in both Cargo.toml files.
const VERSION = "1.13.8";
const ARCHIVES = {
  "x86_64-pc-windows-msvc": {
    luna: {
      name: `sherpa-onnx-v${VERSION}-win-x64-static-MD-Release-no-tts-lib`,
      sha256:
        "542348e56e827b59c6d249fd0dfd38dc34b7bd0c521a1ebe9eb6e2db154fa15d",
    },
    helper: {
      name: `sherpa-onnx-v${VERSION}-win-x64-static-MD-Release-lib`,
      sha256:
        "a0f44cd91486e448c2be1f1d3662edb4f473ca3cb38a803cee158760cc588428",
    },
  },
  "aarch64-apple-darwin": {
    luna: {
      name: `sherpa-onnx-v${VERSION}-osx-arm64-static-no-tts-lib`,
      sha256:
        "3d7f9b8a496694af13d9802c33b8133231e397bdef302f543d19468765e83136",
    },
    helper: {
      name: `sherpa-onnx-v${VERSION}-osx-arm64-static-lib`,
      sha256:
        "9091bf160dc7fdacedbc906b212badf53c2993f4e5277a0e03998e96c31d60da",
    },
  },
};

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const target =
  process.argv[2] ??
  execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(
    /^host: (.+)$/m,
  )?.[1];
const archives = ARCHIVES[target];
if (!archives) {
  console.error(`No pinned sherpa-onnx libraries for ${target}.`);
  process.exit(1);
}

await fetchInto(
  archives.luna,
  join(root, "src-tauri", "target", "sherpa-onnx"),
);
await fetchInto(
  archives.helper,
  join(root, "voice-helper", "target", "sherpa-onnx"),
);

async function fetchInto(archive, directory) {
  const stamp = join(directory, "version");
  if (
    existsSync(stamp) &&
    readFileSync(stamp, "utf8").trim() === archive.sha256
  ) {
    return;
  }
  const url = `https://github.com/k2-fsa/sherpa-onnx/releases/download/v${VERSION}/${archive.name}.tar.bz2`;
  console.log(`Downloading ${archive.name}.`);
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
  writeFileSync(join(directory, `${archive.name}.tar.bz2`), bytes);
  // Only the lib directory, without the top-level folder. Relative paths, because GNU tar
  // reads "C:" in a path as a remote host.
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
  rmSync(join(directory, `${archive.name}.tar.bz2`));
  writeFileSync(stamp, archive.sha256);
}
