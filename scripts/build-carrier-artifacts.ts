import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

type JsonRecord = Record<string, unknown>;

const workspaceRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const sourceRoot = dirname(workspaceRoot);
const naradaCoreRoot = resolve(process.env.NARADA_CORE_ROOT ?? join(sourceRoot, "narada-core"));
const userProfile = process.env.USERPROFILE;
if (!userProfile) throw new Error("carrier_build_user_profile_required");

const contractPath = resolve(
  process.env.NARADA_CARRIER_CONTRACT
    ?? join(userProfile, "Narada", ".narada", "capabilities", "carrier-materialization.json"),
);
const runtimeRoot = join(workspaceRoot, ".ai", "runtime");
const manifestPath = join(runtimeRoot, "workspace-artifact-manifest.json");
const referencePath = join(runtimeRoot, "carrier-required-references.json");
const buildSetPath = join(runtimeRoot, "artifact-build-set.json");
mkdirSync(runtimeRoot, { recursive: true });

for (const [command, args] of [
  ["pnpm", ["--version"]],
  ["cargo", ["--version"]],
  ["rustc", ["-vV"]],
] as const) {
  run(command, [...args], workspaceRoot, "toolchain_preflight");
}
if (!existsSync(join(naradaCoreRoot, "packages", "artifact-integrity", "package.json"))) {
  throw new Error(`artifact_integrity_workspace_missing:${naradaCoreRoot}`);
}

run("pnpm", ["run", "build:node"], workspaceRoot, "workspace_build");
run(
  "pnpm",
  ["--recursive", "--if-present", "run", "build:native"],
  workspaceRoot,
  "native_dependency_graph_build",
);
// Native package builds may rotate immutable entrypoints concurrently. Publish
// site registries only after every pointer has reached its final graph state.
run(
  "pnpm",
  ["--filter", "@narada-core/mcp-runtime-proxy", "run", "build:native"],
  workspaceRoot,
  "site_registry_publication_barrier",
);
run(
  "node",
  ["--import", "tsx", join(workspaceRoot, "scripts", "build-workspace-artifact-manifest.ts")],
  workspaceRoot,
  "workspace_manifest_generation",
);
run(
  "pnpm",
  ["--dir", naradaCoreRoot, "--filter", "@narada-core/artifact-integrity", "run", "build"],
  naradaCoreRoot,
  "artifact_integrity_build",
);

const requiredReferences = collectRequiredReferences(contractPath);
requiredReferences.push(resolveCurrentMaterializerEntrypoint());
requiredReferences.sort();
writeFileSync(referencePath, `${JSON.stringify(requiredReferences, null, 2)}\n`, "utf8");
run(
  "node",
  [
    join(naradaCoreRoot, "packages", "artifact-integrity", "dist", "cli.js"),
    "build-set",
    "--workspace-manifest",
    manifestPath,
    "--required-references",
    referencePath,
    "--output",
    buildSetPath,
  ],
  workspaceRoot,
  "artifact_build_set_seal",
);

process.stdout.write(`${JSON.stringify({
  schema: "narada.carrier_artifact_preparation.v1",
  status: "prepared",
  workspace_root: workspaceRoot,
  manifest_path: manifestPath,
  build_set_path: buildSetPath,
  required_reference_count: requiredReferences.length,
})}\n`);

function run(command: string, args: string[], cwd: string, phase: string): void {
  const pnpmEntrypoint = process.platform === "win32" && command === "pnpm"
    ? resolveWindowsPnpmEntrypoint()
    : null;
  const executable = pnpmEntrypoint ? process.execPath : command;
  const executableArgs = pnpmEntrypoint ? [pnpmEntrypoint, ...args] : args;
  const result = spawnSync(executable, executableArgs, {
    cwd,
    env: process.env,
    encoding: "utf8",
    windowsHide: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.status !== 0 || result.error) {
    throw new Error(JSON.stringify({
      schema: "narada.carrier_artifact_preparation_error.v1",
      code: "carrier_artifact_preparation_failed",
      phase,
      command: executable,
      args: executableArgs,
      exit_code: result.status,
      error: result.error?.message ?? null,
      stdout: result.stdout,
      stderr: result.stderr,
    }));
  }
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
}

function resolveWindowsPnpmEntrypoint(): string {
  for (const directory of (process.env.Path ?? process.env.PATH ?? "").split(";").filter(Boolean)) {
    const candidate = join(directory, "node_modules", "corepack", "dist", "pnpm.js");
    if (existsSync(candidate)) return candidate;
  }
  throw new Error("carrier_build_pnpm_entrypoint_unavailable");
}

function collectRequiredReferences(path: string): string[] {
  const contract = readJson(path);
  if (!Array.isArray(contract.sites)) throw new Error("carrier_contract_sites_required");
  const references = new Set<string>();
  for (const siteValue of contract.sites) {
    if (!isRecord(siteValue) || typeof siteValue.registry_path !== "string") {
      throw new Error("carrier_contract_registry_path_required");
    }
    const registry = readJson(siteValue.registry_path);
    if (!Array.isArray(registry.surfaces)) throw new Error("carrier_registry_surfaces_required");
    const selected = new Set(Array.isArray(siteValue.surface_ids)
      ? siteValue.surface_ids.filter((value): value is string => typeof value === "string")
      : []);
    for (const surface of registry.surfaces) {
      if (!isRecord(surface) || typeof surface.catalog_surface_id !== "string"
        || !selected.has(surface.catalog_surface_id)) continue;
      const binding = isRecord(surface.runtime_binding) ? surface.runtime_binding : {};
      const transport = isRecord(binding.transport) ? binding.transport : {};
      if (typeof transport.command === "string" && isAbsolute(transport.command)) {
        references.add(resolve(transport.command));
      }
      if (!Array.isArray(transport.args)) continue;
      for (let index = 0; index < transport.args.length - 1; index += 1) {
        const flag = transport.args[index];
        const value = transport.args[index + 1];
        if (
          typeof flag === "string"
          && typeof value === "string"
          && ["--child-command", "--entrypoint", "--registrar-command", "--registrar-entrypoint"].includes(flag)
          && isAbsolute(value)
        ) {
          references.add(resolve(value));
        }
      }
    }
  }
  return [...references].sort();
}

function resolveCurrentMaterializerEntrypoint(): string {
  const artifactRoot = join(workspaceRoot, "packages", "shared", "mcp-materializer-native", "dist", "native");
  const pointer = readJson(join(artifactRoot, "current.json"));
  const artifacts = isRecord(pointer.artifacts) ? pointer.artifacts : {};
  const relative = artifacts["narada-mcp-materializer.exe"];
  if (typeof relative !== "string") throw new Error("carrier_build_materializer_pointer_invalid");
  return resolve(artifactRoot, relative);
}

function readJson(path: string): JsonRecord {
  return JSON.parse(readFileSync(path, "utf8")) as JsonRecord;
}

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
