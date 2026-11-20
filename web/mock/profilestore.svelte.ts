// The owner's name (mock). Real: GET/PUT /api/me {name}; zenbot sets it in the first chat,
// while it fills USER.md, and the owner can change it in Settings › You. Used by Home's greeting
// and the agent menu.
const KEY = 'zen.mock.name';
export const me = $state({ name: localStorage.getItem(KEY) || 'Adriane' });
export function setName(n: string) {
  const v = n.trim();
  if (!v) return false;
  me.name = v;
  localStorage.setItem(KEY, v);
  return true;
}
