import { describe, expect, it } from 'vitest';
import { framable, inject, previewPath, targetPort } from './preview.ts';

describe('preview gateway rules', () => {
  it('shows dev server ports 3000–9999, never blocked ones', () => {
    expect(targetPort('5173', [4400, 8100])).toBe(5173);
    for (const bad of ['4400', '8100', '2999', '10000', '80', '5173x', ' 5173', '', undefined]) expect(targetPort(bad, [4400, 8100]), String(bad)).toBeNull();
  });
  it('reads /preview/<port>/… into the port and the rest', () => {
    expect(previewPath('/preview/5173/app/x?y=1')).toEqual({ port: '5173', prefix: '/preview/5173', rest: '/app/x?y=1' });
    expect(previewPath('/preview/5173')).toEqual({ port: '5173', prefix: '/preview/5173', rest: '/' });
    for (const bad of ['/preview/', '/preview/abc/', '/previewx/5173/', '/api/preview/5173/']) expect(previewPath(bad), bad).toBeNull();
  });
  it('makes the browser check with the dev server unless it set its own caching', () => {
    expect(framable({ 'last-modified': 'Mon, 01 Jan 2024 00:00:00 GMT' })['cache-control']).toBe('no-cache');
    expect(framable({ 'cache-control': 'max-age=31536000, immutable' })['cache-control']).toBe('max-age=31536000, immutable');
  });
  it('puts the pointing script first in <head>, or at the start', () => {
    expect(inject('<!doctype html><html><head lang="x"><title>t</title></head>')).toBe('<!doctype html><html><head lang="x"><script src="/zen-preview.js"></script><title>t</title></head>');
    expect(inject('<!DOCTYPE html><p>hi')).toBe('<!DOCTYPE html><script src="/zen-preview.js"></script><p>hi');
    expect(inject('<p>hi')).toBe('<script src="/zen-preview.js"></script><p>hi');
  });
  it('drops what stops framing, and keeps the rest of the policy', () => {
    const h = framable({ 'x-frame-options': 'DENY', 'content-security-policy': "default-src 'self'; frame-ancestors 'none'", 'content-type': 'text/html' });
    expect(h['x-frame-options']).toBeUndefined();
    expect(h['content-security-policy']).toBe("default-src 'self'");
    expect(framable({ 'content-security-policy': "frame-ancestors 'none'" })['content-security-policy']).toBeUndefined();
  });
});
