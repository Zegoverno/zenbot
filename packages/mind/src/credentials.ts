// File-backed CredentialStore compatible with the `pi-ai login` CLI format ({ [providerId]: credential }).
// Writes are serialized per process; the file is created with mode 0600.

import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import type { CredentialStore } from "@earendil-works/pi-ai";

type Json = any;

export class FileCredentialStore implements CredentialStore {
  private chain: Promise<unknown> = Promise.resolve();
  private path: string;
  constructor(path: string) {
    this.path = path;
  }

  private load(): Record<string, Json> {
    if (!existsSync(this.path)) return {};
    try {
      return JSON.parse(readFileSync(this.path, "utf-8"));
    } catch {
      return {};
    }
  }

  private save(data: Record<string, Json>) {
    mkdirSync(dirname(this.path), { recursive: true, mode: 0o700 });
    const tmp = `${this.path}.tmp`;
    writeFileSync(tmp, JSON.stringify(data, null, 2), { mode: 0o600 });
    renameSync(tmp, this.path);
  }

  private enqueue<T>(fn: () => Promise<T>): Promise<T> {
    const next = this.chain.then(fn, fn);
    this.chain = next.catch(() => undefined);
    return next;
  }

  async read(providerId: string): Promise<Json> {
    return this.load()[providerId];
  }

  async list(): Promise<Json[]> {
    return Object.entries(this.load()).map(([providerId, c]) => ({ providerId, type: c?.type }));
  }

  modify(providerId: string, fn: (current: Json) => Promise<Json>): Promise<Json> {
    return this.enqueue(async () => {
      const data = this.load();
      const next = await fn(data[providerId]);
      if (next === undefined) return data[providerId];
      data[providerId] = next;
      this.save(data);
      return next;
    });
  }

  delete(providerId: string): Promise<void> {
    return this.enqueue(async () => {
      const data = this.load();
      delete data[providerId];
      this.save(data);
    });
  }
}
