import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

// Guard the settings-to-kernel contract independently of the shared e2e scenario.
const source = readFileSync(new URL('./SettingsSections.svelte', import.meta.url), 'utf8');
describe('engine settings contract', () => {
  it('uses authenticated models only and exposes login state and errors', () => {
    expect(source).toContain("models!.authenticated[m.id.split('/')[0]] === true");
    for (const status of ['starting', 'pending', 'complete', 'error', 'expired']) expect(source).toContain(`'${status}'`);
    expect(source).toContain('engine.login.code');
    expect(source).toContain('engine.login.url');
    expect(source).toContain('Could not load models: {modelsError}');
  });
  it('confirms logout and saves a password key without restarting', () => {
    expect(source).toContain('window.confirm(');
    expect(source).toContain('`engines/${id}/logout`, \'POST\'');
    expect(source).toContain("'engines/openrouter/key', 'PUT', {key}");
    expect(source).toContain('type="password"');
    expect(source).toContain('kernel upgrade or service restart');
  });
});
