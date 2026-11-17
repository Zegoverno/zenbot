// zen-preview.js: lets zen's Preview tab point at elements of an app in development.
//
// Load it first in the app's <head>, in its development build only (so it hears the first errors):
//   <script src="https://<your zen host>/zen-preview.js"></script>
// It does nothing unless the page is framed by zen's Preview on the same host (another port), and
// it answers only that window. With "Point" on, hovering outlines an element and a click pins it
// (the click doesn't reach the app): its tag, text, CSS path, box, key styles, the last console
// errors, its source file and line (Svelte's dev build; React 18's; or a `data-source="file:line"`
// attribute) and a best-effort screenshot go to zen as one message. Escape turns pointing off.
(() => {
  if (window.parent === window || window.__zenPreview) return;
  window.__zenPreview = true;

  let host = null; // zen's origin, once it says hello
  let picking = false;
  const errors = [];
  const keep = (e) => {
    errors.push(String(e).slice(0, 300));
    if (errors.length > 5) errors.shift();
  };
  const origError = console.error;
  console.error = (...a) => {
    keep(a.map((x) => (x instanceof Error ? x.message : typeof x === 'string' ? x : JSON.stringify(x))).join(' '));
    return origError.apply(console, a);
  };
  addEventListener('error', (e) => keep(e.message));
  addEventListener('unhandledrejection', (e) => keep(e.reason instanceof Error ? e.reason.message : e.reason));

  const send = (msg) => host && window.parent.postMessage({ zen: 'preview', ...msg }, host);

  addEventListener('message', (e) => {
    if (e.source !== window.parent || !e.data || e.data.zen !== 'preview') return;
    let o;
    try { o = new URL(e.origin); } catch { return; }
    // Only zen on this very host (its own port) is answered.
    if (o.hostname !== location.hostname || o.protocol !== location.protocol) return;
    if (e.data.type === 'hello') {
      host = e.origin;
      send({ type: 'ready' });
    } else if (e.data.type === 'pick' && e.origin === host) setPicking(e.data.on === true);
  });

  // ---- pointing ----
  const box = document.createElement('div');
  box.setAttribute('data-zen-preview', '');
  Object.assign(box.style, { position: 'fixed', pointerEvents: 'none', zIndex: '2147483647', outline: '2px solid #2f7de1', background: 'rgba(47,125,225,0.12)', display: 'none', borderRadius: '2px' });

  function setPicking(on) {
    picking = on;
    if (on && !box.isConnected) document.documentElement.appendChild(box);
    box.style.display = 'none';
    document.documentElement.style.cursor = on ? 'crosshair' : '';
  }
  const ours = (el) => el === box || !(el instanceof Element);
  addEventListener('mousemove', (e) => {
    if (!picking) return;
    const el = document.elementFromPoint(e.clientX, e.clientY);
    if (!el || ours(el)) return;
    const r = el.getBoundingClientRect();
    Object.assign(box.style, { display: 'block', left: `${r.left}px`, top: `${r.top}px`, width: `${r.width}px`, height: `${r.height}px` });
  }, true);
  for (const type of ['pointerdown', 'mousedown', 'pointerup', 'mouseup']) {
    addEventListener(type, (e) => { if (picking) { e.preventDefault(); e.stopPropagation(); } }, true);
  }
  addEventListener('click', (e) => {
    if (!picking) return;
    e.preventDefault();
    e.stopPropagation();
    const el = document.elementFromPoint(e.clientX, e.clientY);
    if (el && !ours(el)) void pin(el);
  }, true);
  addEventListener('keydown', (e) => {
    if (picking && e.key === 'Escape') {
      setPicking(false);
      send({ type: 'pick', on: false });
    }
  }, true);

  // ---- what a pin says ----
  function source(el) {
    for (let n = el, i = 0; n && i < 30; n = n.parentElement, i++) {
      const loc = n.__svelte_meta && n.__svelte_meta.loc;
      if (loc && loc.file) return { file: String(loc.file), line: loc.line, column: loc.column };
      const attr = n.getAttribute && n.getAttribute('data-source');
      if (attr) {
        const m = /^(.+?)(?::(\d+))?(?::(\d+))?$/.exec(attr);
        if (m) return { file: m[1], line: m[2] ? Number(m[2]) : undefined, column: m[3] ? Number(m[3]) : undefined };
      }
      const fiberKey = Object.keys(n).find((k) => k.startsWith('__reactFiber$'));
      for (let f = fiberKey && n[fiberKey], j = 0; f && j < 30; f = f.return, j++) {
        if (f._debugSource) return { file: f._debugSource.fileName, line: f._debugSource.lineNumber, column: f._debugSource.columnNumber };
      }
    }
    return null;
  }

  function selector(el) {
    const parts = [];
    for (let n = el; n && n.nodeType === 1 && parts.length < 6; n = n.parentElement) {
      if (n.id) { parts.unshift(`#${CSS.escape(n.id)}`); break; }
      let s = n.tagName.toLowerCase();
      const cls = [...n.classList].filter((c) => !/^svelte-|^s-/.test(c)).slice(0, 2);
      if (cls.length) s += '.' + cls.map((c) => CSS.escape(c)).join('.');
      const sib = n.parentElement ? [...n.parentElement.children].filter((c) => c.tagName === n.tagName) : [];
      if (sib.length > 1) s += `:nth-of-type(${sib.indexOf(n) + 1})`;
      parts.unshift(s);
    }
    return parts.join(' > ');
  }

  const STYLES = ['display', 'position', 'width', 'height', 'margin', 'padding', 'gap', 'font-family', 'font-size', 'font-weight', 'line-height', 'color', 'background-color', 'border', 'border-radius', 'box-shadow', 'text-align', 'justify-content', 'align-items'];

  // A screenshot by drawing a copy of the element, its computed styles inlined, through SVG; best
  // effort: images and fonts from elsewhere may be missing, and some browsers refuse it.
  async function shot(el) {
    const r = el.getBoundingClientRect();
    if (!r.width || !r.height) return null;
    const clone = el.cloneNode(true);
    const src = [el, ...el.querySelectorAll('*')].slice(0, 1500);
    const dst = [clone, ...clone.querySelectorAll('*')].slice(0, 1500);
    src.forEach((s, i) => {
      const d = dst[i];
      if (!d || !d.style) return;
      const cs = getComputedStyle(s);
      for (let k = 0; k < cs.length; k++) d.style.setProperty(cs[k], cs.getPropertyValue(cs[k]));
    });
    clone.style.margin = '0';
    const w = Math.ceil(r.width), h = Math.ceil(r.height);
    const xml = new XMLSerializer().serializeToString(clone);
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}"><foreignObject width="100%" height="100%"><div xmlns="http://www.w3.org/1999/xhtml">${xml}</div></foreignObject></svg>`;
    const img = new Image();
    img.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg);
    await Promise.race([img.decode(), new Promise((_, no) => setTimeout(() => no(new Error('timeout')), 3000))]);
    const k = Math.min(2, devicePixelRatio || 1, 1600 / Math.max(w, h));
    const c = document.createElement('canvas');
    c.width = Math.max(1, Math.round(w * k));
    c.height = Math.max(1, Math.round(h * k));
    const g = c.getContext('2d');
    g.scale(k, k);
    g.drawImage(img, 0, 0);
    return c.toDataURL('image/png');
  }

  async function pin(el) {
    const r = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    const styles = {};
    for (const p of STYLES) styles[p] = cs.getPropertyValue(p);
    let image = null;
    try { image = await shot(el); } catch { image = null; }
    send({
      type: 'pin',
      pin: {
        tag: el.tagName.toLowerCase(),
        selector: selector(el),
        text: (el.innerText || el.getAttribute('aria-label') || el.getAttribute('alt') || '').slice(0, 200),
        url: location.pathname + location.search + location.hash,
        rect: { x: r.left + scrollX, y: r.top + scrollY, w: r.width, h: r.height },
        viewport: { w: innerWidth, h: innerHeight },
        source: source(el),
        styles,
        errors: errors.slice(),
        image,
      },
    });
  }
})();
