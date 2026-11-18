import { describe, expect, it } from 'vitest';
import { framable, inject, localPath, targetPort } from './preview.ts';

describe('preview gateway rules', () => {
  it('shows dev server ports 3000–9999, never blocked ones', () => {
    expect(targetPort('5173', [4400, 8100])).toBe(5173);
    for (const bad of ['4400', '8100', '2999', '10000', '80', '5173x', ' 5173', '', undefined]) expect(targetPort(bad, [4400, 8100]), String(bad)).toBeNull();
  });
  it('redirects only to paths on this host', () => {
    expect(localPath('/app?x=1#y')).toBe('/app?x=1#y');
    for (const bad of ['//evil.example/', '/\\evil.example', 'https://evil.example/', 'x', '/a\nb', null]) expect(localPath(bad)).toBe('/');
  });
  it('puts the pointing script first in <head>, or at the start', () => {
    expect(inject('<!doctype html><html><head lang="x"><title>t</title></head>')).toBe('<!doctype html><html><head lang="x"><script src="/__zen/preview.js"></script><title>t</title></head>');
    expect(inject('<!DOCTYPE html><p>hi')).toBe('<!DOCTYPE html><script src="/__zen/preview.js"></script><p>hi');
    expect(inject('<p>hi')).toBe('<script src="/__zen/preview.js"></script><p>hi');
  });
  it('drops what stops framing, and keeps the rest of the policy', () => {
    const h = framable({ 'x-frame-options': 'DENY', 'content-security-policy': "default-src 'self'; frame-ancestors 'none'", 'content-type': 'text/html' });
    expect(h['x-frame-options']).toBeUndefined();
    expect(h['content-security-policy']).toBe("default-src 'self'");
    expect(framable({ 'content-security-policy': "frame-ancestors 'none'" })['content-security-policy']).toBeUndefined();
  });
});
