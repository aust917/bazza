#!/usr/bin/env node
// Stages a pinned CPU runtime in its own directory. Its ggml/llama libraries
// must never be copied into the application's top-level runtime directory.
// Usage: node scripts/prepare-nemo-runtime.mjs --target <triple>
//        [--out <resource-root>] [--cache-dir <dir>] [--archive <verified-source>]
import { execFileSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { createWriteStream, existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync,
  statSync, lstatSync, realpathSync, readlinkSync, cpSync, renameSync, rmSync } from 'node:fs';
import { pipeline } from 'node:stream/promises';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';

export const NEMO_RUNTIME_VERSION = '0.3.0';
const release = 'https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.3.0/';
const specs = {
  'x86_64-pc-windows-msvc': {
    packageName: 'nemo-speech-0.3.0-windows-x86_64-cpu', extension: '.zip',
    size: 8694509, sha256: '4ed9740e361e4653f56c3dce7d1e81883a1c8b831c1bd7d01567fff1130b5b05',
    directory: 'bin', library: 'nemo_speech_asr_c.dll', minimumOs: null,
    required: ['nemo_speech_asr_c.dll', 'nemo_speech_asr.dll', 'nemo_speech_speechlm.dll',
      'llama-common.dll', 'llama.dll', 'ggml.dll', 'ggml-base.dll', 'ggml-cpu.dll',
      'msvcp140.dll', 'vcruntime140.dll', 'vcruntime140_1.dll', 'vcomp140.dll'],
  },
  'aarch64-apple-darwin': {
    packageName: 'nemo-speech-0.3.0-macos-aarch64-cpu', extension: '.tar.gz',
    size: 6895976, sha256: '2c3a7a719b778daeb14f147e011891f16e4b2ac62388d8b8d8b365cc2eebdf15',
    directory: 'lib', library: 'libnemo_speech_asr_c.dylib', minimumOs: '13.3',
  },
  'x86_64-apple-darwin': {
    packageName: 'nemo-speech-0.3.0-macos-x86_64-cpu', extension: '.tar.gz',
    size: 7742012, sha256: '97e98aa5eb82dffa4f513f26336b5301d792c9fb43f720ac8f9520a0e92ffe3f',
    directory: 'lib', library: 'libnemo_speech_asr_c.dylib', minimumOs: '13.3',
  },
};
const macRequired = ['libnemo_speech_asr_c.dylib', 'libnemo_speech_asr.dylib',
  'libnemo_speech_speechlm.dylib', 'libnemo_speech_text_normalization.dylib',
  'libllama-common.dylib', 'libllama.dylib', 'libggml.dylib',
  'libggml-base.dylib', 'libggml-cpu.dylib', 'libggml-blas.dylib'];

export function runtimeSpec(target) {
  const spec = specs[target];
  if (!spec) throw new Error('No pinned NeMo CPU runtime for target ' + target);
  return { ...spec, target, backend: 'cpu', version: NEMO_RUNTIME_VERSION,
    archive: spec.packageName + spec.extension,
    url: release + spec.packageName + spec.extension,
    required: [...(spec.required ?? macRequired)] };
}

export function runtimeDirectory(resourceRoot, target) {
  const spec = runtimeSpec(target);
  return path.join(path.resolve(resourceRoot), 'nemo-speech', NEMO_RUNTIME_VERSION, target, spec.directory);
}

export function cargoOutputDirectory(root, { profile, target, host, targetRoot = process.env.CARGO_TARGET_DIR }) {
  const base = targetRoot ? path.resolve(root, targetRoot) : path.join(root, 'target');
  const directory = profile === 'dev' ? 'debug' : profile;
  return target === host ? path.join(base, directory) : path.join(base, target, directory);
}

export function cargoProfileArguments(profile) {
  if (profile === 'release') return ['--release'];
  if (profile === 'dev' || profile === 'debug') return [];
  return ['--profile', profile];
}

function contained(parent, child) {
  const relative = path.relative(path.resolve(parent), path.resolve(child));
  return relative !== '' && !relative.startsWith('..' + path.sep) && relative !== '..' && !path.isAbsolute(relative);
}

export function removeGeneratedDirectory(directory, parent) {
  if (!contained(parent, directory)) throw new Error('Refusing to remove directory outside generated runtime scope');
  if (!existsSync(directory)) return;
  if (!contained(realpathSync(parent), realpathSync(directory))) {
    throw new Error('Generated runtime directory resolves outside its scope');
  }
  rmSync(directory, { recursive: true, force: true });
}

function sha256(file) {
  return createHash('sha256').update(readFileSync(file)).digest('hex');
}

export function verifyRuntimeArchive(file, spec) {
  if (statSync(file).size !== spec.size) throw new Error('NeMo archive size mismatch: ' + file);
  if (sha256(file) !== spec.sha256) throw new Error('NeMo archive SHA-256 mismatch: ' + file);
}

function filesUnder(root) {
  const files = [];
  const visit = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const file = path.join(directory, entry.name);
      if (entry.isDirectory()) visit(file);
      else files.push(file);
    }
  };
  visit(root);
  return files.sort();
}

function requiredPresent(directory, spec) {
  return spec.required.every((name) => {
    const file = path.join(directory, spec.directory, name);
    return existsSync(file) && statSync(file).isFile();
  });
}

export function verifyStagedRuntime(directory, spec) {
  try {
    const receipt = JSON.parse(readFileSync(path.join(directory, 'runtime.json'), 'utf8'));
    if (receipt.archiveSha256 !== spec.sha256 || receipt.target !== spec.target ||
        receipt.version !== spec.version || receipt.backend !== 'cpu' || !requiredPresent(directory, spec)) return false;
    return receipt.files.length > 0 && receipt.files.every((entry) => {
      const file = path.resolve(directory, entry.path);
      return contained(directory, file) && existsSync(file) &&
        contained(realpathSync(directory), realpathSync(file)) &&
        statSync(file).size === entry.size && sha256(file) === entry.sha256 &&
        (entry.symlink === undefined || (lstatSync(file).isSymbolicLink() && readlinkSync(file) === entry.symlink));
    });
  } catch { return false; }
}

// packageRoot must come from an archive checked by verifyRuntimeArchive.
export function stageRuntimePackage(packageRoot, resourceRoot, target) {
  const spec = runtimeSpec(target);
  if (!requiredPresent(packageRoot, spec)) throw new Error('NeMo package is missing required runtime libraries');
  const licenses = path.join(packageRoot, 'share', 'licenses');
  if (!existsSync(licenses)) throw new Error('NeMo package is missing its license directory');
  for (const file of filesUnder(packageRoot)) {
    if (lstatSync(file).isSymbolicLink() && !contained(realpathSync(packageRoot), realpathSync(file))) {
      throw new Error('NeMo package symlink escapes package directory');
    }
  }
  const versionRoot = path.join(path.resolve(resourceRoot), 'nemo-speech', spec.version);
  mkdirSync(versionRoot, { recursive: true });
  const destination = path.join(versionRoot, target);
  const staging = path.join(versionRoot, '.' + target + '-' + randomUUID());
  mkdirSync(staging);
  try {
    for (const relative of ['bin', 'lib', path.join('share', 'licenses')]) {
      const source = path.join(packageRoot, relative);
      if (existsSync(source)) cpSync(source, path.join(staging, relative), {
        recursive: true, dereference: false, verbatimSymlinks: true,
      });
    }
    const files = filesUnder(staging).map((file) => ({
      path: path.relative(staging, file).split(path.sep).join('/'),
      size: statSync(file).size, sha256: sha256(file),
      ...(lstatSync(file).isSymbolicLink() ? { symlink: readlinkSync(file) } : {}),
    }));
    writeFileSync(path.join(staging, 'runtime.json'), JSON.stringify({
      version: spec.version, sourceCommit: '20999cc921d2b14aa58708f61756a8b2e787ba20',
      target, backend: 'cpu', minimumOs: spec.minimumOs, archiveUrl: spec.url,
      archiveSha256: spec.sha256, archiveSize: spec.size,
      runtimeSubdirectory: spec.directory, library: spec.library, files,
    }, null, 2) + '\n');
    if (!verifyStagedRuntime(staging, spec)) throw new Error('NeMo staged runtime verification failed');
    removeGeneratedDirectory(destination, versionRoot);
    renameSync(staging, destination);
    return { directory: destination, runtimeDir: runtimeDirectory(resourceRoot, target), spec, reused: false };
  } finally {
    removeGeneratedDirectory(staging, versionRoot);
  }
}

export async function prepareNemoRuntime({ target, resourceRoot, cacheDir, archive }) {
  const spec = runtimeSpec(target);
  const destination = path.join(path.resolve(resourceRoot), 'nemo-speech', spec.version, target);
  if (verifyStagedRuntime(destination, spec)) {
    return { directory: destination, runtimeDir: runtimeDirectory(resourceRoot, target), spec, reused: true };
  }
  const cache = path.resolve(cacheDir ?? path.join(os.homedir(), '.cache', 'bazza', 'nemo-speech', spec.version));
  mkdirSync(cache, { recursive: true });
  let source = archive ? path.resolve(archive) : path.join(cache, spec.archive);
  if (archive) verifyRuntimeArchive(source, spec);
  else {
    let valid = false;
    try { verifyRuntimeArchive(source, spec); valid = true; } catch { /* download a verified replacement */ }
    if (!valid) {
      const partial = source + '.part-' + randomUUID();
      try {
        const response = await fetch(spec.url, { redirect: 'follow',
          headers: { 'user-agent': 'bazza-prepare-nemo-runtime' }, signal: AbortSignal.timeout(180000) });
        if (!response.ok) throw new Error('NeMo runtime download failed: HTTP ' + response.status);
        await pipeline(response.body, createWriteStream(partial, { flags: 'wx' }));
        verifyRuntimeArchive(partial, spec);
        rmSync(source, { force: true });
        renameSync(partial, source);
      } finally { rmSync(partial, { force: true }); }
    }
  }
  const extracted = path.join(cache, '.extract-' + randomUUID());
  mkdirSync(extracted);
  try {
    if (spec.extension === '.zip') {
      const literal = (value) => "'" + value.replaceAll("'", "''") + "'";
      execFileSync('powershell', ['-NoProfile', '-NonInteractive', '-Command',
        'Expand-Archive -LiteralPath ' + literal(source) + ' -DestinationPath ' + literal(extracted) + ' -Force']);
    } else {
      execFileSync('tar', ['-xzf', source, '-C', extracted]);
    }
    return stageRuntimePackage(path.join(extracted, spec.packageName), resourceRoot, target);
  } finally { removeGeneratedDirectory(extracted, cache); }
}

const script = fileURLToPath(import.meta.url);
if (process.argv[1] && path.resolve(process.argv[1]) === script) {
  const args = process.argv.slice(2);
  if (args.includes('--help')) {
    console.log('Usage: node scripts/prepare-nemo-runtime.mjs [--target <triple>] [--out <resource-root>] [--cache-dir <dir>] [--archive <file>]');
    process.exit(0);
  }
  const option = (name, fallback) => {
    const index = args.indexOf(name);
    if (index < 0) return fallback;
    if (!args[index + 1] || args[index + 1].startsWith('--')) throw new Error('Missing value for ' + name);
    return args[index + 1];
  };
  const root = path.resolve(path.dirname(script), '..');
  try {
    const known = new Set(['--target', '--out', '--cache-dir', '--archive']);
    for (let index = 0; index < args.length; index += 2) {
      if (!known.has(args[index])) throw new Error('Unknown option ' + args[index]);
      option(args[index]);
    }
    const target = option('--target') ?? execFileSync('rustc', ['-vV']).toString().match(/host: (\S+)/)[1];
    const result = await prepareNemoRuntime({
      target, resourceRoot: option('--out', path.join(root, 'src-tauri', 'resources')),
      cacheDir: option('--cache-dir'), archive: option('--archive'),
    });
    console.log('[prepare-nemo-runtime] ' + (result.reused ? 'verified ' : 'staged ') + result.runtimeDir);
  } catch (error) {
    console.error('[prepare-nemo-runtime] ' + error.message);
    process.exitCode = 1;
  }
}
