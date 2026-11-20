import { afterEach, describe, expect, it, vi } from 'vitest';
import { call, patch, remove, type T } from './taskstore.svelte.ts';

afterEach(() => vi.unstubAllGlobals());
describe('task mutations', () => {
  it('sends revisions and the web write header on changes and deletes', async () => {
    const fetcher = vi.fn().mockResolvedValue({ok:true, json:async () => ({})});
    vi.stubGlobal('fetch', fetcher);
    const t: T = {id:'L:Projects/a:1', rev:'rev 1', text:'Task', project:'a', goal:null, due:null, pri:null};
    await patch(t, {done:true});
    expect(fetcher.mock.calls[0][0]).toBe('/api/zen/tasks/L%3AProjects%2Fa%3A1');
    expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({done:true, rev:'rev 1'});
    expect(fetcher.mock.calls[0][1].headers['x-zen-web']).toBe('1');
    await remove(t);
    expect(fetcher.mock.calls[1][0]).toContain('?rev=rev%201');
    expect(fetcher.mock.calls[1][1].method).toBe('DELETE');
  });
  it('surfaces conflicts', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ok:false, status:409, json:async () => ({error:'changed'})}));
    await expect(call('PATCH', '/api/zen/tasks/a', {})).rejects.toThrow('changed');
  });
});
