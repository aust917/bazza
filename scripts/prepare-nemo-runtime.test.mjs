import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, symlinkSync, readlinkSync } from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { runtimeSpec, runtimeDirectory, verifyRuntimeArchive, verifyStagedRuntime,
  stageRuntimePackage, prepareNemoRuntime, removeGeneratedDirectory,
  cargoOutputDirectory, cargoProfileArguments } from './prepare-nemo-runtime.mjs';

const win = 'x86_64-pc-windows-msvc';
function temporary() {
  const root = mkdtempSync(path.join(os.tmpdir(), 'bazza-nemo-runtime-'));
  return { root, clean: () => removeGeneratedDirectory(root, os.tmpdir()) };
}
function packageFixture(root, target = win) {
  const spec = runtimeSpec(target);
  const source = path.join(root, 'package');
  mkdirSync(path.join(source, spec.directory), { recursive: true });
  for (const name of spec.required) writeFileSync(path.join(source, spec.directory, name), 'fixture:' + name);
  const license = path.join(source, 'share', 'licenses', 'nemo-speech');
  mkdirSync(license, { recursive: true });
  writeFileSync(path.join(license, 'LICENSE'), 'Fixture license');
  return source;
}

test('CPU assets are selected by exact target; unsupported Windows ARM is never guessed', () => {
  for (const target of [win, 'aarch64-apple-darwin', 'x86_64-apple-darwin']) {
    const spec = runtimeSpec(target);
    assert.equal(spec.backend, 'cpu');
    assert.equal(spec.sha256.length, 64);
    assert.match(spec.url, /\/v0\.3\.0\//);
    assert.match(spec.archive, /-cpu\./);
  }
  assert.equal(runtimeSpec('aarch64-apple-darwin').minimumOs, '13.3');
  assert.throws(() => runtimeSpec('aarch64-pc-windows-msvc'), /No pinned/);
});

test('Cargo dev/custom-target outputs remain in their own CPU or CUDA tree', () => {
  const root = path.resolve('workspace');
  assert.equal(cargoOutputDirectory(root, { profile: 'dev', target: win, host: win, targetRoot: null }),
    path.join(root, 'target', 'debug'));
  assert.equal(cargoOutputDirectory(root, { profile: 'release', target: win, host: win, targetRoot: 'target-cuda' }),
    path.join(root, 'target-cuda', 'release'));
  assert.equal(cargoOutputDirectory(root, { profile: 'dev', target: 'aarch64-apple-darwin', host: 'x86_64-apple-darwin', targetRoot: 'isolated' }),
    path.join(root, 'isolated', 'aarch64-apple-darwin', 'debug'));
  const absolute = path.resolve('absolute-target-cuda');
  assert.equal(cargoOutputDirectory(root, { profile: 'dev', target: win, host: win, targetRoot: absolute }),
    path.join(absolute, 'debug'));
  assert.deepEqual(cargoProfileArguments('dev'), []);
  assert.deepEqual(cargoProfileArguments('debug'), []);
  assert.deepEqual(cargoProfileArguments('release'), ['--release']);
  assert.deepEqual(cargoProfileArguments('acceptance'), ['--profile', 'acceptance']);
});

test('archive verification rejects truncation and same-size corruption', () => {
  const temp = temporary();
  try {
    const file = path.join(temp.root, 'archive.zip');
    writeFileSync(file, 'abc');
    const spec = { size: 3, sha256: createHash('sha256').update('abc').digest('hex') };
    verifyRuntimeArchive(file, spec);
    writeFileSync(file, 'ab');
    assert.throws(() => verifyRuntimeArchive(file, spec), /size mismatch/);
    writeFileSync(file, 'abd');
    assert.throws(() => verifyRuntimeArchive(file, spec), /SHA-256 mismatch/);
  } finally { temp.clean(); }
});

test('runtime staging preserves parent ggml and detects a corrupted isolated library', () => {
  const temp = temporary();
  try {
    const source = packageFixture(temp.root);
    const resources = path.join(temp.root, 'resources');
    mkdirSync(resources);
    writeFileSync(path.join(resources, 'ggml.dll'), 'application ggml');
    const staged = stageRuntimePackage(source, resources, win);
    assert.equal(staged.runtimeDir, runtimeDirectory(resources, win));
    assert.equal(readFileSync(path.join(resources, 'ggml.dll'), 'utf8'), 'application ggml');
    assert.ok(verifyStagedRuntime(staged.directory, runtimeSpec(win)));
    writeFileSync(path.join(staged.runtimeDir, 'ggml.dll'), 'damaged');
    assert.equal(verifyStagedRuntime(staged.directory, runtimeSpec(win)), false);
    assert.ok(verifyStagedRuntime(stageRuntimePackage(source, resources, win).directory, runtimeSpec(win)));
    assert.equal(readFileSync(path.join(resources, 'ggml.dll'), 'utf8'), 'application ggml');
  } finally { temp.clean(); }
});

test('generated-directory cleanup refuses its parent and paths outside its scope', () => {
  const temp = temporary();
  try {
    assert.throws(() => removeGeneratedDirectory(temp.root, temp.root), /Refusing/);
    assert.throws(() => removeGeneratedDirectory(path.join(temp.root, '..', 'outside'), temp.root), /Refusing/);
  } finally { temp.clean(); }
});

test('Mac versioned dylib symlinks remain relative and cannot escape the package', (t) => {
  const temp = temporary();
  try {
    const target = 'aarch64-apple-darwin';
    const source = packageFixture(temp.root, target);
    const library = path.join(source, 'lib', 'libggml-base.dylib');
    const versioned = path.join(source, 'lib', 'libggml-base.0.25.1.dylib');
    writeFileSync(versioned, 'versioned library');
    rmSync(library);
    try { symlinkSync(path.basename(versioned), library, 'file'); }
    catch (error) {
      if (error.code === 'EPERM') { t.skip('Windows account cannot create file symlinks'); return; }
      throw error;
    }
    const staged = stageRuntimePackage(source, path.join(temp.root, 'resources'), target);
    assert.equal(readlinkSync(path.join(staged.runtimeDir, 'libggml-base.dylib')), path.basename(versioned));
    assert.ok(verifyStagedRuntime(staged.directory, runtimeSpec(target)));
    rmSync(library);
    const outside = path.join(temp.root, 'outside-library');
    writeFileSync(outside, 'outside');
    symlinkSync(outside, library, 'file');
    assert.throws(() => stageRuntimePackage(source, path.join(temp.root, 'other-resources'), target), /symlink escapes/);
  } finally { temp.clean(); }
});

test('real Windows archive stages, reuses, and repairs only its isolated runtime', {
  skip: process.platform !== 'win32' || !process.env.BAZZA_TEST_NEMO_ARCHIVE,
}, async () => {
  const temp = temporary();
  try {
    const resources = path.join(temp.root, 'resources');
    mkdirSync(resources);
    writeFileSync(path.join(resources, 'ggml.dll'), 'existing application runtime');
    const options = { target: win, resourceRoot: resources,
      cacheDir: path.join(temp.root, 'cache'), archive: process.env.BAZZA_TEST_NEMO_ARCHIVE };
    const first = await prepareNemoRuntime(options);
    assert.equal(first.reused, false);
    assert.equal((await prepareNemoRuntime(options)).reused, true);
    const file = path.join(first.runtimeDir, 'ggml.dll');
    const bytes = readFileSync(file);
    bytes[0] ^= 0xff;
    writeFileSync(file, bytes);
    assert.equal(verifyStagedRuntime(first.directory, runtimeSpec(win)), false);
    const repaired = await prepareNemoRuntime(options);
    assert.equal(repaired.reused, false);
    assert.ok(verifyStagedRuntime(repaired.directory, runtimeSpec(win)));
    assert.equal(readFileSync(path.join(resources, 'ggml.dll'), 'utf8'), 'existing application runtime');
  } finally { temp.clean(); }
});
