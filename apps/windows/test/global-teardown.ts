import { rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// Tests write temporary manifests under test/.tmp; clear them before and after the run.
const tmp = join(dirname(fileURLToPath(import.meta.url)), ".tmp");

export default function setup() {
  rmSync(tmp, { recursive: true, force: true });
  return () => rmSync(tmp, { recursive: true, force: true });
}
